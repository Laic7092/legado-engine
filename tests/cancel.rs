//! Cancellation: the guarantee is "the call comes back promptly".
//!
//! Every test here starts a call that would otherwise block for tens of
//! seconds (a hanging server, a rate-limit window, an infinite loop inside a
//! source script), cancels it from the other thread and insists on an answer.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use legado_engine::cancel;
use legado_engine::model::book_source::BookSource;
use legado_engine::pipeline::{Config, Engine};

/// A server that accepts and then says nothing. The point is the silence:
/// the client sits in a read that would only end at its own timeout.
fn serve_silence() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                let _ = stream.read(&mut buffer);
                // Hold the connection open, answering nothing.
                thread::sleep(Duration::from_secs(30));
            });
        }
    });
    port
}

/// A server that answers every request with one search result and counts how
/// many requests it saw.
fn serve_ok(body: &'static str) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    thread::spawn(move || {
        for stream in listener.incoming().take(16) {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer);
            seen.fetch_add(1, Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, count)
}

fn engine(timeout_secs: u64) -> Engine {
    Engine::new(Config {
        direct: true,
        timeout_secs: Some(timeout_secs),
        ..Config::default()
    })
    .unwrap()
}

fn source(port: u16, extra: &str) -> BookSource {
    serde_json::from_str(&format!(
        r##"{{"bookSourceName":"t","bookSourceUrl":"http://127.0.0.1:{port}","searchUrl":"/s?q={{{{key}}}}{extra}",
             "ruleSearch":{{"bookList":"div.row","name":"a@text","bookUrl":"a@href"}}}}"##
    ))
    .unwrap()
}

#[test]
fn a_hanging_search_can_be_cancelled() {
    let port = serve_silence();
    // 30 秒总超时: 不取消的话这条线程要卡满 30 秒。
    let engine = Arc::new(engine(30));
    let canceller = engine.canceller();
    let source = source(port, r#",{\"retry\":0}"#);

    let worker = {
        let engine = engine.clone();
        thread::spawn(move || {
            let started = Instant::now();
            let result = engine.search(&source, "x", 1);
            (started.elapsed(), result)
        })
    };

    thread::sleep(Duration::from_millis(300));
    let cancelled_at = Instant::now();
    canceller.cancel();

    let (started_at, result) = worker.join().unwrap();
    let after_cancel = cancelled_at.elapsed();
    let error = result.expect_err("a cancelled call must not answer");
    assert!(
        cancel::is_cancelled(&error),
        "the error must be a cancellation, not a timeout: {error:#}"
    );
    assert!(
        after_cancel < Duration::from_secs(2),
        "took {after_cancel:?} to stop after the cancel ({started_at:?} total)"
    );
}

#[test]
fn cancelling_stops_a_rate_limit_queue() {
    let (port, count) = serve_ok(r#"<div class="row"><a href="/b/1">One</a></div>"#);
    // 头一个请求立刻发, 之后的要排 5 秒: 取消必须能把排队的人叫走。
    let engine = Arc::new(engine(30));
    let mut source = source(port, "");
    source.concurrent_rate = Some("1/5000".to_string());
    let canceller = engine.canceller();
    let finished = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(source);

    let worker = {
        let engine = engine.clone();
        let source = source.clone();
        let finished = finished.clone();
        thread::spawn(move || {
            assert!(engine.search(&source, "one", 1).is_ok());
            finished.fetch_add(1, Ordering::SeqCst);
            let started = Instant::now();
            let result = engine.search(&source, "two", 1);
            (started.elapsed(), result)
        })
    };

    // 等第一个请求过去, 此时第二个还卡在限速窗口里。
    for _ in 0..200 {
        if finished.load(Ordering::SeqCst) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        finished.load(Ordering::SeqCst),
        1,
        "the first call did not finish"
    );
    canceller.cancel();

    let (elapsed, result) = worker.join().unwrap();
    let error = result.expect_err("the queued call must not answer");
    assert!(cancel::is_cancelled(&error), "{error:#}");
    assert!(
        elapsed < Duration::from_secs(2),
        "the queued call waited {elapsed:?} instead of leaving the queue"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "the cancelled request must not be sent"
    );
}

#[test]
fn cancelling_does_not_poison_the_next_call() {
    let (port, _) = serve_ok(r#"<div class="row"><a href="/b/1">One</a></div>"#);
    let engine = engine(5);
    let source = source(port, "");

    // 取消用在一个闲着的引擎上: 什么都不该发生, 之后的调用照常。
    engine.cancel();
    let books = engine
        .search(&source, "x", 1)
        .expect("a fresh call must work");
    assert_eq!(books.len(), 1);

    // 再取消一次, 下一次仍然照常。
    engine.cancel();
    assert_eq!(engine.search(&source, "x", 1).unwrap().len(), 1);
}

#[test]
fn an_endless_script_is_interrupted() {
    // 书源脚本里真的出现过 `while(true)` / 忘了 break 的循环。
    // 没有中断条件的话这句 eval 永远不会返回。
    let engine = Arc::new(engine(15));
    let canceller = engine.canceller();
    let source = Arc::new(source(1, ""));

    let worker = {
        let engine = engine.clone();
        let source = source.clone();
        thread::spawn(move || {
            let started = Instant::now();
            let result = engine.eval(&source, "while (true) {}", "", "");
            (started.elapsed(), result)
        })
    };

    thread::sleep(Duration::from_millis(300));
    let cancelled_at = Instant::now();
    canceller.cancel();

    let (_total, result) = worker.join().unwrap();
    let after_cancel = cancelled_at.elapsed();
    let error = result.expect_err("an endless script must not answer");
    assert!(
        cancel::is_cancelled(&error),
        "a cancelled script must report cancellation: {error:#}"
    );
    assert!(
        after_cancel < Duration::from_secs(5),
        "the script kept running for {after_cancel:?} after the cancel"
    );
}
