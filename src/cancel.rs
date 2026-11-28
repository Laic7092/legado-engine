//! Cooperative cancellation.
//!
//! A [`CancelToken`] is a flag plus a wake-up. The token of the running call is
//! attached to the thread ([`scope`]); every blocking point — HTTP send, body
//! read, retry backoff, rate-limit window, source JavaScript — checks it and
//! fails with a [`Cancelled`] error.
//!
//! Cancellation is cooperative: the guarantee is that the call returns promptly,
//! not that it stops at an exact instruction. Bytes already written are not
//! rolled back.
//!
//! # Levels
//!
//! * [`crate::pipeline::Engine::canceller`] — a [`Canceller`] cloneable into
//!   another thread; `legado_cancel(engine)` in C.
//! * [`scope`] — a token for one call. Enclosing tokens stay live, so
//!   cancelling either one stops the work.
//!
//! A cancel affects the calls in flight; later calls run normally.
//!
//! # Telling cancellation apart from failure
//!
//! In Rust: `error.downcast_ref::<Cancelled>().is_some()` (survives the context
//! layers the pipeline adds). In C: [`CANCELLED`], i.e. `LEGADO_CANCELLED`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// The exact message a cancelled call leaves in `legado_last_error`.
pub const CANCELLED: &str = "cancelled";

/// A one-shot "stop what you are doing" flag.
#[derive(Debug, Default)]
pub struct CancelToken {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        // Set the flag before waking the sleepers: a woken task must see it.
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once the token is cancelled (immediately if it already is).
    pub async fn cancelled(&self) {
        // `enable()` registers before the flag is read, which closes the
        // window where a cancel between the two would be lost.
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// The error every cancelled call fails with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(CANCELLED)
    }
}

impl std::error::Error for Cancelled {}

/// A cancelled error, ready to be returned.
pub fn error() -> anyhow::Error {
    anyhow::Error::new(Cancelled)
}

/// Was this failure a cancellation rather than a real one?
///
/// Works through the `.context(...)` layers the pipeline adds, so callers do
/// not have to know where in the stack the checkpoint fired.
pub fn is_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Cancelled>().is_some()
}

thread_local! {
    /// Tokens of the calls running on this thread, innermost last.
    static CURRENT: RefCell<Vec<Arc<CancelToken>>> = const { RefCell::new(Vec::new()) };
}

/// Makes `token` the token of the current thread until the handle is dropped.
///
/// Callers are expected to attach and drop it on the same thread (the pipeline
/// does its work on the calling thread, so this holds naturally).
pub struct Scope(());

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Attach `token` to this thread for as long as the returned handle lives.
pub fn scope(token: Arc<CancelToken>) -> Scope {
    CURRENT.with(|stack| stack.borrow_mut().push(token));
    Scope(())
}

/// The innermost token of the call running on this thread, if any.
pub fn current() -> Option<Arc<CancelToken>> {
    CURRENT.with(|stack| stack.borrow().last().cloned())
}

/// Every token of the call running on this thread, outermost first.
fn tokens() -> Vec<Arc<CancelToken>> {
    CURRENT.with(|stack| stack.borrow().clone())
}

/// Is the call running on this thread cancelled?
///
/// Every enclosing scope counts: the engine's own token and a `scope()` a Rust
/// caller attached outside it both stop the work. Anything else would make
/// nesting silently ignore the outer cancel.
pub fn requested() -> bool {
    CURRENT.with(|stack| stack.borrow().iter().any(|token| token.is_cancelled()))
}

/// Fail with a [`Cancelled`] error if the current call was cancelled.
pub fn check() -> anyhow::Result<()> {
    if requested() {
        return Err(error());
    }
    Ok(())
}

/// How often [`wait`] re-checks the enclosing scopes.
///
/// The innermost token wakes the waiter immediately; the outer ones are polled
/// because watching an arbitrary number of them would need a combinator per
/// token. 50ms is below what a user can notice and costs nothing on a download.
const OUTER_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Resolves when the current call is cancelled; never, if it has no token.
///
/// Meant for `tokio::select!` around anything that can block.
pub async fn wait() {
    if tokens().is_empty() {
        return std::future::pending().await;
    }
    loop {
        if requested() {
            return;
        }
        let poll = tokio::time::sleep(OUTER_POLL);
        match current() {
            Some(token) => tokio::select! {
                biased;
                _ = token.cancelled() => {}
                _ = poll => {}
            },
            None => poll.await,
        }
    }
}

/// Await `future`, giving up as soon as the current call is cancelled.
///
/// The future is dropped on cancellation, which is what closes the socket:
/// `reqwest` aborts the request when its future goes away.
pub async fn guard<F: std::future::Future>(future: F) -> anyhow::Result<F::Output> {
    if requested() {
        return Err(error());
    }
    tokio::select! {
        biased;
        _ = wait() => Err(error()),
        value = future => Ok(value),
    }
}

/// The mutex-protected "token of whatever is running now".
///
/// Cancelling swaps in a fresh token and fires the old one, so the cancel is
/// one-shot per call: the next call on the same handle starts uncancelled.
#[derive(Debug, Clone)]
pub struct Canceller {
    current: Arc<Mutex<Arc<CancelToken>>>,
}

impl Canceller {
    pub fn new() -> Self {
        Self {
            current: Arc::new(Mutex::new(CancelToken::new())),
        }
    }

    /// The token to attach to a call that is starting now.
    pub fn token(&self) -> Arc<CancelToken> {
        self.lock().clone()
    }

    /// Cancel the calls in flight. Later calls are unaffected.
    pub fn cancel(&self) {
        let fresh = CancelToken::new();
        let old = {
            let mut slot = self.lock();
            std::mem::replace(&mut *slot, fresh)
        };
        // Fire it after releasing the lock: a woken task may want the slot.
        old.cancel();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Arc<CancelToken>> {
        self.current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

impl Default for Canceller {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outer_cancel_stops_an_inner_scope() {
        // 引擎在自己的调用里压一个令牌；Rust 调用方在外面再压一个。
        // 任何一个被取消都要能停下工作，否则外层的取消会被静默忽略。
        let outer = CancelToken::new();
        let inner = CancelToken::new();
        let _outer = scope(outer.clone());
        let _inner = scope(inner.clone());
        assert!(!requested());
        outer.cancel();
        assert!(requested(), "the outer scope must not be shadowed");
        assert!(!inner.is_cancelled());
    }

    #[test]
    fn a_scope_is_the_token_of_this_thread() {
        let token = CancelToken::new();
        assert!(!requested());
        {
            let _scope = scope(token.clone());
            assert!(!requested());
            token.cancel();
            assert!(requested());
            assert!(check().is_err());
        }
        assert!(!requested(), "the token must be dropped with the scope");
    }

    #[test]
    fn an_outer_scope_comes_back_after_an_inner_one() {
        let outer = CancelToken::new();
        let inner = CancelToken::new();
        let guard = scope(outer.clone());
        {
            let _inner = scope(inner.clone());
            inner.cancel();
            assert!(requested());
        }
        assert!(!requested(), "the inner token must not leak outwards");
        outer.cancel();
        assert!(requested());
        drop(guard);
        assert!(!requested());
    }

    #[test]
    fn a_cancel_error_survives_context_layers() {
        let error = error().context("request the source");
        assert!(is_cancelled(&error));
        assert_eq!(format!("{error:#}"), "request the source: cancelled");
        let real = anyhow::anyhow!("the source answered 404");
        assert!(!is_cancelled(&real));
    }

    #[test]
    fn cancelling_is_one_shot_and_affects_calls_in_flight() {
        let canceller = Canceller::new();
        let running = canceller.token();
        canceller.cancel();
        assert!(running.is_cancelled(), "the call in flight is cancelled");
        let next = canceller.token();
        assert!(!next.is_cancelled(), "the next call starts clean");
        canceller.cancel();
        assert!(next.is_cancelled());
        assert!(!canceller.token().is_cancelled());
    }

    #[test]
    fn a_token_that_is_already_cancelled_wakes_immediately() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let token = CancelToken::new();
        token.cancel();
        let fired = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(200), token.cancelled())
                .await
                .is_ok()
        });
        assert!(fired, "a cancelled token must not make its waiter wait");
    }

    #[test]
    fn cancelling_from_another_thread_wakes_the_waiter() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let token = CancelToken::new();
        let _scope = scope(token.clone());
        let waiter = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            waiter.cancel();
        });
        let started = std::time::Instant::now();
        let error = runtime
            .block_on(guard(std::future::pending::<()>()))
            .unwrap_err();
        assert!(is_cancelled(&error));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the waiter was not woken up"
        );
    }
}
