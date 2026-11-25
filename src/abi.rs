//! The C ABI. One handle owns the HTTP clients, cookies, the rule engine and
//! the JavaScript runtime; each call fetches, parses and answers, or returns
//! NULL and sets the handle's last error.
//!
//! A handle can be used from several threads at once; answers and errors are
//! kept per calling thread. Every returned pointer is valid until the next
//! call on that thread.

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use serde::Deserialize;

use crate::model::book::Book;
use crate::model::book_chapter::BookChapter;
use crate::model::book_source::BookSource;
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

fn source(json: &str) -> anyhow::Result<BookSource> {
    serde_json::from_str(json).map_err(|error| anyhow::anyhow!("source JSON: {error}"))
}

fn set_error(message: impl std::fmt::Display) {
    let value = CString::new(message.to_string().replace('\0', "")).unwrap_or_default();
    ERROR.with(|cell| *cell.borrow_mut() = value);
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
    let result = catch_unwind(AssertUnwindSafe(|| f(engine)));
    match result {
        Ok(Ok(value)) => match serde_json::to_string(&value) {
            Ok(json) => answer(json),
            Err(error) => {
                set_error(format!("serialize the result: {error}"));
                std::ptr::null()
            }
        },
        Ok(Err(error)) => {
            set_error(format!("{error:#}"));
            std::ptr::null()
        }
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            set_error(format!("engine panic: {message}"));
            std::ptr::null()
        }
    }
}

fn run_text<F>(engine: *mut LegadoEngine, f: F) -> *const c_char
where
    F: FnOnce(&LegadoEngine) -> anyhow::Result<String>,
{
    if engine.is_null() {
        return std::ptr::null();
    }
    let engine = unsafe { &*engine };
    let result = catch_unwind(AssertUnwindSafe(|| f(engine)));
    match result {
        Ok(Ok(value)) => answer(value),
        Ok(Err(error)) => {
            set_error(format!("{error:#}"));
            std::ptr::null()
        }
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            set_error(format!("engine panic: {message}"));
            std::ptr::null()
        }
    }
}

#[no_mangle]
pub extern "C" fn legado_version() -> *const c_char {
    static VERSION: &[u8] = b"legado-engine 0.2.0\0";
    VERSION.as_ptr() as *const c_char
}

/// Open the engine. `config_json` may be NULL for the defaults:
/// `{"storageDir": null, "proxy": null, "timeoutSecs": 30, "userNs": "legado-engine", "direct": false}`.
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
        Ok(engine.inner.search(&source, key, page.max(1))?)
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
        Ok(engine.inner.explore(&source, url)?)
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
        Ok(engine.inner.info(&source, book_url)?)
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
        Ok(engine.inner.toc(&source, &book)?)
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
        Ok::<_, anyhow::Error>(engine.inner.cookie_header(&source, url)?)
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
