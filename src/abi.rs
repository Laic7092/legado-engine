//! The C ABI. One handle owns the HTTP clients, cookies, the rule engine and
//! the JavaScript runtime; each call fetches, parses and answers, or returns
//! NULL and sets the handle's last error.
//!
//! A handle can be used from several threads at once; answers and errors are
//! kept per calling thread. Every returned pointer is valid until the next
//! call on that thread.

// 这个模块本身就是 C ABI：每个函数都要解引用宿主传来的指针，而 `extern "C"`
// 函数不能是 `unsafe fn`（那会改变签名）。指针的约定写在 `legado.h` 里。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use serde::Deserialize;

use crate::model::book::Book;
use crate::model::book_chapter::BookChapter;
use crate::model::book_source::BookSource;
use crate::model::rss::{rss_source_from_value, RssArticle, RssSource};
use crate::parser::replace::{ReplaceContext, Target};
use crate::pipeline::{Config, Engine};

pub struct LegadoEngine {
    inner: Engine,
}

// Answers and errors belong to the calling thread. The engine itself is safe
// to use from several threads at once (its JS contexts are thread-local and
// its HTTP/session state is locked); this is what lets the app search several
// sources in parallel on one handle. Each pointer stays valid until the next
// call on that thread, as before.
thread_local! {
    static RESULT: RefCell<CString> = RefCell::new(CString::default());
    static SOURCES_RESULT: RefCell<CString> = RefCell::new(CString::default());
    static RSS_SOURCES_RESULT: RefCell<CString> = RefCell::new(CString::default());
    static ERROR: RefCell<CString> = RefCell::new(CString::default());
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct OpenConfig {
    storage_dir: Option<String>,
    proxy: Option<String>,
    timeout_secs: Option<u64>,
    user_ns: Option<String>,
    direct: bool,
}

fn text<'a>(p: *const c_char, what: &str) -> anyhow::Result<&'a str> {
    if p.is_null() {
        anyhow::bail!("{what} is NULL");
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| anyhow::anyhow!("{what} is not UTF-8"))
}

fn answer(value: String) -> *const c_char {
    let text = CString::new(value.replace('\0', "")).unwrap_or_default();
    RESULT.with(|cell| {
        *cell.borrow_mut() = text;
        cell.borrow().as_ptr()
    })
}

fn static_answer(value: String) -> *const c_char {
    let text = CString::new(value.replace('\0', "")).unwrap_or_default();
    SOURCES_RESULT.with(|cell| {
        *cell.borrow_mut() = text;
        cell.borrow().as_ptr()
    })
}

fn rss_answer(value: String) -> *const c_char {
    let text = CString::new(value.replace('\0', "")).unwrap_or_default();
    RSS_SOURCES_RESULT.with(|cell| {
        *cell.borrow_mut() = text;
        cell.borrow().as_ptr()
    })
}

fn source(json: &str) -> anyhow::Result<BookSource> {
    serde_json::from_str(json).map_err(|error| anyhow::anyhow!("source JSON: {error}"))
}

/// The text a failure leaves in `legado_last_error`.
///
/// Cancellation is reported as exactly [`crate::cancel::CANCELLED`] whichever
/// checkpoint noticed it, so `strcmp(legado_last_error(handle), LEGADO_CANCELLED)`
/// is the whole test a host needs.
fn error_message(error: &anyhow::Error) -> String {
    if crate::cancel::is_cancelled(error) {
        crate::cancel::CANCELLED.to_string()
    } else {
        format!("{error:#}")
    }
}

fn set_error(message: impl std::fmt::Display) {
    let value = CString::new(message.to_string().replace('\0', "")).unwrap_or_default();
    ERROR.with(|cell| *cell.borrow_mut() = value);
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Turn a caught call into an answer or a last-error. Kept separate from
/// [`run_json`] because `legado_import` and `legado_replace` work without a
/// handle (an inline payload needs no HTTP client, and replace rules never
/// touch the network).
fn settled<T: serde::Serialize>(result: std::thread::Result<anyhow::Result<T>>) -> *const c_char {
    match result {
        Ok(Ok(value)) => match serde_json::to_string(&value) {
            Ok(json) => answer(json),
            Err(error) => {
                set_error(format!("serialize the result: {error}"));
                std::ptr::null()
            }
        },
        Ok(Err(error)) => {
            set_error(error_message(&error));
            std::ptr::null()
        }
        Err(panic) => {
            set_error(format!("engine panic: {}", panic_message(&*panic)));
            std::ptr::null()
        }
    }
}

fn settled_text(result: std::thread::Result<anyhow::Result<String>>) -> *const c_char {
    match result {
        Ok(Ok(value)) => answer(value),
        Ok(Err(error)) => {
            set_error(error_message(&error));
            std::ptr::null()
        }
        Err(panic) => {
            set_error(format!("engine panic: {}", panic_message(&*panic)));
            std::ptr::null()
        }
    }
}

fn run_json<T, F>(engine: *mut LegadoEngine, f: F) -> *const c_char
where
    T: serde::Serialize,
    F: FnOnce(&LegadoEngine) -> anyhow::Result<T>,
{
    if engine.is_null() {
        return std::ptr::null();
    }
    let engine = unsafe { &*engine };
    settled(catch_unwind(AssertUnwindSafe(|| f(engine))))
}

fn run_text<F>(engine: *mut LegadoEngine, f: F) -> *const c_char
where
    F: FnOnce(&LegadoEngine) -> anyhow::Result<String>,
{
    if engine.is_null() {
        return std::ptr::null();
    }
    let engine = unsafe { &*engine };
    settled_text(catch_unwind(AssertUnwindSafe(|| f(engine))))
}

/// Cancel every call this handle is running right now.
///
/// Returns immediately and is safe to call from any thread, including while
/// another thread is inside a call on the same handle. The cancelled calls
/// fail with `legado_last_error` exactly `LEGADO_CANCELLED`; calls started
/// afterwards run normally (the cancel is not sticky).
///
/// The point is latency: without it, a search the user already left keeps a
/// thread blocked on timeouts and retries for up to a minute. Safe with a NULL
/// handle (does nothing).
#[no_mangle]
pub extern "C" fn legado_cancel(engine: *mut LegadoEngine) {
    if engine.is_null() {
        return;
    }
    // No `catch_unwind`: `Engine::cancel` only swaps a pointer and fires a
    // flag, and the unwind would be a bug we want to see, not swallow.
    unsafe { &*engine }.inner.cancel();
}

#[no_mangle]
pub extern "C" fn legado_version() -> *const c_char {
    // 版本号取自 Cargo.toml，不要在代码里另写一份。
    static VERSION: &[u8] = concat!("legado-engine ", env!("CARGO_PKG_VERSION"), "\0").as_bytes();
    VERSION.as_ptr() as *const c_char
}

/// Open the engine. `config_json` may be NULL for the defaults:
/// `{"storageDir": null, "proxy": null, "timeoutSecs": 15, "userNs": "legado-engine", "direct": false}`.
#[no_mangle]
pub extern "C" fn legado_open(config_json: *const c_char) -> *mut LegadoEngine {
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        let config = if config_json.is_null() {
            OpenConfig::default()
        } else {
            let text = unsafe { CStr::from_ptr(config_json) }
                .to_str()
                .map_err(|_| "config JSON is not UTF-8".to_string())?;
            serde_json::from_str::<OpenConfig>(text)
                .map_err(|error| format!("config JSON: {error}"))?
        };
        let config = Config {
            storage_dir: config.storage_dir.as_deref().map(PathBuf::from),
            proxy: config.proxy,
            timeout_secs: config.timeout_secs,
            user_ns: config.user_ns,
            direct: config.direct,
        };
        Engine::new(config).map_err(|error| format!("{error:#}"))
    }));
    match parsed {
        Ok(Ok(inner)) => Box::into_raw(Box::new(LegadoEngine { inner })),
        Ok(Err(error)) => {
            set_error(error);
            std::ptr::null_mut()
        }
        Err(_) => {
            set_error("panic while opening the engine");
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn legado_close(engine: *mut LegadoEngine) {
    if engine.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(engine)) };
}

/// The last error of this handle, or the last `legado_open` error for NULL.
/// Empty when there is none; the pointer stays valid until the next call.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceSummary {
    name: String,
    url: String,
    group: String,
    kind: i32,
    enabled: bool,
    comment: String,
    search_url: String,
    explore_url: String,
    has_js: bool,
}

/// Parse a source list (an array, or one object) into source metadata.
/// Entries without a URL are skipped.
#[no_mangle]
pub extern "C" fn legado_sources(json: *const c_char) -> *const c_char {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let text = text(json, "json")?;
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| anyhow::anyhow!("source JSON: {error}"))?;
        let values: Vec<serde_json::Value> = match value {
            serde_json::Value::Array(values) => values,
            other => vec![other],
        };
        let mut out = Vec::new();
        for value in values {
            let source = match crate::model::book_source::book_source_from_value(value) {
                Ok(source) => source,
                Err(_) => continue,
            };
            let url = source.book_source_url.trim().to_string();
            if url.is_empty() {
                continue;
            }
            let has_js = source
                .js_lib
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
                || serde_json::to_string(&source)
                    .map(|json| {
                        let lower = json.to_lowercase();
                        lower.contains("@js:") || lower.contains("<js>")
                    })
                    .unwrap_or(false);
            out.push(SourceSummary {
                name: if source.book_source_name.trim().is_empty() {
                    url.clone()
                } else {
                    source.book_source_name.clone()
                },
                url,
                group: source.book_source_group.clone().unwrap_or_default(),
                kind: source.book_source_type.unwrap_or(0),
                enabled: source.is_enabled(),
                comment: source.book_source_comment.clone().unwrap_or_default(),
                search_url: source.search_url.clone().unwrap_or_default(),
                explore_url: source.explore_url.clone().unwrap_or_default(),
                has_js,
            });
        }
        if out.is_empty() {
            anyhow::bail!("this JSON has no book sources");
        }
        Ok(out)
    }));
    match result {
        Ok(Ok(values)) => {
            static_answer(serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string()))
        }
        Ok(Err(error)) => {
            set_error(error);
            std::ptr::null()
        }
        Err(_) => {
            set_error("panic while parsing sources");
            std::ptr::null()
        }
    }
}

#[no_mangle]
pub extern "C" fn legado_last_error(_engine: *const LegadoEngine) -> *const c_char {
    ERROR.with(|cell| cell.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn legado_search(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    key: *const c_char,
    page: i32,
) -> *const c_char {
    run_json(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let key = text(key, "key")?;
        engine.inner.search(&source, key, page.max(1))
    })
}

#[no_mangle]
pub extern "C" fn legado_explore(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    url: *const c_char,
) -> *const c_char {
    run_json(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let url = text(url, "url")?;
        engine.inner.explore(&source, url)
    })
}

/// One page of an explore category, with the values for `{{key}}` / `{{page}}`
/// in the category URL.
///
/// This is the paging entry point: a source whose explore URL is
/// `/list?page={{page}}` keeps answering page one through `legado_explore`,
/// so a host that never calls this one cannot page the 发现 page.
#[no_mangle]
pub extern "C" fn legado_explore_page(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    url: *const c_char,
    key: *const c_char,
    page: i32,
) -> *const c_char {
    run_json(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let url = text(url, "url")?;
        let key = if key.is_null() { "" } else { text(key, "key")? };
        engine.inner.explore_page(&source, url, key, page)
    })
}

#[no_mangle]
pub extern "C" fn legado_info(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    book_url: *const c_char,
) -> *const c_char {
    run_json(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let book_url = text(book_url, "book_url")?;
        engine.inner.info(&source, book_url)
    })
}

#[no_mangle]
pub extern "C" fn legado_toc(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    book_json: *const c_char,
) -> *const c_char {
    run_json(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let book: Book = serde_json::from_str(text(book_json, "book_json")?)
            .map_err(|error| anyhow::anyhow!("book JSON: {error}"))?;
        engine.inner.toc(&source, &book)
    })
}

#[no_mangle]
pub extern "C" fn legado_content(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    book_json: *const c_char,
    chapter_json: *const c_char,
) -> *const c_char {
    run_text(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let book: Book = serde_json::from_str(text(book_json, "book_json")?)
            .map_err(|error| anyhow::anyhow!("book JSON: {error}"))?;
        let chapter: BookChapter = serde_json::from_str(text(chapter_json, "chapter_json")?)
            .map_err(|error| anyhow::anyhow!("chapter JSON: {error}"))?;
        engine.inner.content(&source, &book, &chapter)
    })
}

/// Download a file with the source's headers, cookies and proxy, and write it
/// to `dest_path`. Answers
/// `{"path":"…","size":123,"contentType":null,"charset":"gb18030","url":"…"}`.
///
/// `charset` is sniffed from the file itself (declared charset, BOM, UTF-8,
/// else GB18030); the bytes are written verbatim.
#[no_mangle]
pub extern "C" fn legado_download(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    url: *const c_char,
    dest_path: *const c_char,
) -> *const c_char {
    run_json(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let url = text(url, "url")?;
        let dest = text(dest_path, "dest_path")?;
        if dest.trim().is_empty() {
            anyhow::bail!("dest_path is empty");
        }
        engine
            .inner
            .download(&source, url, std::path::Path::new(dest))
    })
}

#[no_mangle]
pub extern "C" fn legado_eval(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    script: *const c_char,
    input: *const c_char,
    base_url: *const c_char,
) -> *const c_char {
    run_text(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let script = text(script, "script")?;
        let input = if input.is_null() {
            ""
        } else {
            text(input, "input")?
        };
        let base_url = if base_url.is_null() {
            ""
        } else {
            text(base_url, "base_url")?
        };
        engine.inner.eval(&source, script, input, base_url)
    })
}

#[no_mangle]
pub extern "C" fn legado_cookie_header(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    url: *const c_char,
) -> *const c_char {
    run_text(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let url = text(url, "url")?;
        engine.inner.cookie_header(&source, url)
    })
}

#[no_mangle]
pub extern "C" fn legado_set_cookie(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    url: *const c_char,
    cookie: *const c_char,
) -> *const c_char {
    run_text(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let url = text(url, "url")?;
        let cookie = text(cookie, "cookie")?;
        engine.inner.set_cookie(&source, url, cookie)?;
        Ok(String::new())
    })
}

/// Run a rule against text already in hand, without a network request.
#[no_mangle]
pub extern "C" fn legado_eval_rule(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    rule: *const c_char,
    body: *const c_char,
    base_url: *const c_char,
) -> *const c_char {
    run_text(engine, |engine| {
        let source = source(text(source_json, "source_json")?)?;
        let rule = text(rule, "rule")?;
        let body = text(body, "body")?;
        let base_url = text(base_url, "base_url")?;
        engine.inner.eval_rule(&source, rule, body, base_url)
    })
}

/// Import whatever the user has: a `legado://import/…` share link, a URL, a
/// base64 payload or the JSON itself.
///
/// `engine` may be NULL when the payload is inline (no download needed). The
/// answer is
/// `{"declaredKind":…,"url":…,"origin":…,"bookSources":[…],"rssSources":[…],
///   "replaceRules":[…],"unknown":0}` — entries are the original JSON objects,
/// so fields this engine does not know survive the round trip.
///
/// `declaredKind` is the intent from the link (`bookSource`, `rssSource`,
/// `replaceRule`, …); the arrays are classified by **shape**, so a mixed
/// collection lands in the right buckets.
#[no_mangle]
pub extern "C" fn legado_import(engine: *mut LegadoEngine, raw: *const c_char) -> *const c_char {
    let handle = if engine.is_null() {
        None
    } else {
        Some(unsafe { &*engine })
    };
    settled(catch_unwind(AssertUnwindSafe(|| {
        let body = text(raw, "text")?;
        match handle {
            Some(handle) => handle.inner.import(body),
            None => crate::import::import_text(body),
        }
    })))
}

/// Run replace rules (净化) over text. No network, no handle.
///
/// `rules_json` is a `ReplaceRule` array, `context_json` may be NULL or
/// `{"bookName":…,"bookUrl":…,"sourceUrl":…}` (empty string means the same as
/// NULL), and `target`
/// is `"title"` or `"content"` (NULL means content).
///
/// The answer is
/// `{"text":…,"applied":[…],"skipped":[…],"errors":[…]}`: rules that were
/// filtered out (`disabled`, `scope`, `title`, …) and rules that failed (a
/// pattern this engine cannot compile) are **reported**, never silently
/// ignored — "I added a rule and nothing happened" is the usual complaint.
#[no_mangle]
pub extern "C" fn legado_replace(
    rules_json: *const c_char,
    body: *const c_char,
    context_json: *const c_char,
    target: *const c_char,
) -> *const c_char {
    settled(catch_unwind(AssertUnwindSafe(|| {
        let rules: Vec<crate::model::replace_rule::ReplaceRule> =
            serde_json::from_str(text(rules_json, "rules_json")?)
                .map_err(|error| anyhow::anyhow!("rules JSON: {error}"))?;
        let body = text(body, "text")?;
        // 空串和 NULL 一样当"没有上下文"：宿主从设置里读一个空字符串过来是常态，
        // 让它变成 "EOF while parsing a value" 只会变成一句无从下手的报错。
        let raw_context = if context_json.is_null() {
            ""
        } else {
            text(context_json, "context_json")?
        };
        let context = if raw_context.trim().is_empty() {
            ReplaceContext::default()
        } else {
            serde_json::from_str::<ReplaceContext>(raw_context)
                .map_err(|error| anyhow::anyhow!("context JSON: {error}"))?
        };
        let target = if target.is_null() {
            Target::Content
        } else {
            match text(target, "target")?.trim() {
                "" | "content" => Target::Content,
                "title" => Target::Title,
                other => anyhow::bail!("target must be \"title\" or \"content\", not {other:?}"),
            }
        };
        Ok(crate::parser::replace::apply(
            body, &rules, &context, target,
        ))
    })))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RssSourceSummary {
    name: String,
    url: String,
    group: String,
    enabled: bool,
    comment: String,
    sort_url: String,
    single_url: bool,
    has_js: bool,
}

/// Parse an RSS source list (an array, or one object) into
/// `{name, url, group, enabled, comment, sortUrl, singleUrl, hasJs}`.
/// `sortUrl` is the category JSON; the host parses it, like `exploreUrl`.
#[no_mangle]
pub extern "C" fn legado_rss_sources(json: *const c_char) -> *const c_char {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let body = text(json, "json")?;
        let value: serde_json::Value =
            serde_json::from_str(body).map_err(|error| anyhow::anyhow!("source JSON: {error}"))?;
        let values = match value {
            serde_json::Value::Array(values) => values,
            other => vec![other],
        };
        let mut out = Vec::new();
        for value in values {
            let Ok(source) = rss_source_from_value(value) else {
                continue;
            };
            let url = source.source_url.trim().to_string();
            if url.is_empty() {
                continue;
            }
            let has_js = serde_json::to_string(&source)
                .map(|json| {
                    let lower = json.to_lowercase();
                    lower.contains("@js:") || lower.contains("<js>")
                })
                .unwrap_or(false);
            out.push(RssSourceSummary {
                name: if source.source_name.trim().is_empty() {
                    url.clone()
                } else {
                    source.source_name.clone()
                },
                url,
                group: source.source_group.clone().unwrap_or_default(),
                enabled: source.is_enabled(),
                comment: source.source_comment.clone().unwrap_or_default(),
                sort_url: source.sort_url.clone().unwrap_or_default(),
                single_url: source.single_url.unwrap_or(false),
                has_js,
            });
        }
        if out.is_empty() {
            anyhow::bail!("this JSON has no RSS sources");
        }
        Ok(out)
    }));
    match result {
        Ok(Ok(values)) => {
            rss_answer(serde_json::to_string(&values).unwrap_or_else(|_| "[]".into()))
        }
        Ok(Err(error)) => {
            set_error(error);
            std::ptr::null()
        }
        Err(_) => {
            set_error("panic while parsing RSS sources");
            std::ptr::null()
        }
    }
}

/// One page of an RSS list: `{"articles":[…],"nextUrl":"…"}`.
/// `sort_url` may be NULL for the source's own `sourceUrl`.
#[no_mangle]
pub extern "C" fn legado_rss_articles(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    sort_url: *const c_char,
) -> *const c_char {
    run_json(engine, |engine| {
        let source: RssSource = serde_json::from_str(text(source_json, "source_json")?)
            .map_err(|error| anyhow::anyhow!("source JSON: {error}"))?;
        let sort_url = if sort_url.is_null() {
            ""
        } else {
            text(sort_url, "sort_url")?
        };
        engine.inner.rss_articles(&source, sort_url)
    })
}

/// One article's body, following `ruleNextPage`.
#[no_mangle]
pub extern "C" fn legado_rss_content(
    engine: *mut LegadoEngine,
    source_json: *const c_char,
    article_json: *const c_char,
) -> *const c_char {
    run_text(engine, |engine| {
        let source: RssSource = serde_json::from_str(text(source_json, "source_json")?)
            .map_err(|error| anyhow::anyhow!("source JSON: {error}"))?;
        let article: RssArticle = serde_json::from_str(text(article_json, "article_json")?)
            .map_err(|error| anyhow::anyhow!("article JSON: {error}"))?;
        engine.inner.rss_content(&source, &article)
    })
}
