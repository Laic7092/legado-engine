//! The online pipeline against a local HTTP server: search (including a
//! `java.ajax` field), detail, toc and content. No external network.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use legado_engine::model::book_chapter::BookChapter;
use legado_engine::model::book_source::BookSource;
use legado_engine::model::rss::RssSource;
use legado_engine::pipeline::{Config, Engine};

fn serve() -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        for stream in listener.incoming().take(40) {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            eprintln!("test server: {path}");
            let (status, content_type, body) = match path.split('?').next().unwrap_or("/") {
                "/find" => {
                    // 分类页把 page / key 原样回显，用来证明 `{{page}}` `{{key}}`
                    // 真的传下去了。
                    let page = path
                        .split("page=")
                        .nth(1)
                        .unwrap_or("1")
                        .split('&')
                        .next()
                        .unwrap_or("1");
                    let key = path
                        .split("q=")
                        .nth(1)
                        .unwrap_or("")
                        .split('&')
                        .next()
                        .unwrap_or("")
                        .trim();
                    (
                        "200 OK",
                        "text/html; charset=utf-8",
                        format!(
                            r#"<div class="row"><a href="/book/1">{key}</a><em>page {page}</em></div>"#
                        ),
                    )
                }
                "/search" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r#"<div class="row"><a href="/book/1">One</a><em>Ana</em></div>"#.to_string(),
                ),
                "/book/1" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r##"<h1 id="t">Detail</h1><div id="intro">Intro</div><a id="toc" href="/toc">TOC</a>"##.to_string(),
                ),
                "/toc" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r#"<div id="list"><a href="/c/1">Chapter 1</a></div>"#.to_string(),
                ),
                "/c/1" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r#"<div id="content"><p>Hello</p><p>World</p></div>"#.to_string(),
                ),
                "/api/book/1" => (
                    "200 OK",
                    "application/json",
                    r#"{"title":"Ajax Title"}"#.to_string(),
                ),
                // 订阅源：列表两页，正文两页。
                "/feed" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r##"<div class="item"><a href="/post/1">Title 1</a><time>2024-01-01</time><p>Desc</p></div>
<div class="item"><a href="/post/2">Title 2</a><time>2024-01-02</time><p>Desc</p></div>
<a id="next" href="/feed2">next</a>"##
                        .to_string(),
                ),
                "/feed.json" => (
                    "200 OK",
                    "application/json",
                    r#"{"items":[
                         {"title":"J1","url":"/post/1","date":"2024-01-01","summary":"S1"},
                         {"title":"J2","url":"/post/2","summary":"S2"}]}"#
                        .to_string(),
                ),
                "/feed2" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r#"<div class="item"><a href="/post/2">Title 2</a><time>2024-01-02</time></div>"#
                        .to_string(),
                ),
                "/post/1" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r#"<article><p>Body</p></article><a id="next" href="/post/1/2">next</a>"#
                        .to_string(),
                ),
                "/post/1/2" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    r#"<article><p>Page two</p></article>"#.to_string(),
                ),
                // 一份混了三种规则的导入合集。
                "/collection.json" => (
                    "200 OK",
                    "application/json",
                    r#"[{"bookSourceUrl":"https://a.test","bookSourceName":"A"},
                        {"sourceUrl":"https://b.test/feed","sourceName":"B"},
                        {"name":"删广告","pattern":"（本章完）","replacement":""}]"#
                        .to_string(),
                ),
                _ => ("404 Not Found", "text/plain", "not found".to_string()),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, handle)
}

#[test]
fn the_whole_online_pipeline() {
    let (port, server) = serve();
    let source_json = format!(
        r##"{{
          "bookSourceName": "local",
          "bookSourceUrl": "http://127.0.0.1:{port}",
          "searchUrl": "/search?q={{key}}",
          "ruleSearch": {{
            "bookList": "div.row",
            "name": "@js:JSON.parse(java.ajax('/api/book/1')).title",
            "author": "em@text",
            "bookUrl": "a@href"
          }},
          "ruleBookInfo": {{
            "name": "#t@text",
            "intro": "#intro@text",
            "tocUrl": "a#toc@href"
          }},
          "ruleToc": {{
            "chapterList": "#list a",
            "chapterName": "text",
            "chapterUrl": "href"
          }},
          "ruleContent": {{ "content": "#content" }}
        }}"##
    );
    let source: BookSource = serde_json::from_str(&source_json).unwrap();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();

    let books = engine.search(&source, "我的", 1).unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].name, "Ajax Title");
    assert_eq!(books[0].author, "Ana");
    assert_eq!(books[0].book_url, format!("http://127.0.0.1:{port}/book/1"));

    let book = engine.info(&source, &books[0].book_url).unwrap();
    assert_eq!(book.name, "Detail");
    assert_eq!(book.intro.as_deref(), Some("Intro"));
    assert_eq!(
        book.toc_url.as_deref(),
        Some(format!("http://127.0.0.1:{port}/toc").as_str())
    );

    let chapters = engine.toc(&source, &book).unwrap();
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].title, "Chapter 1");
    assert_eq!(chapters[0].url, format!("http://127.0.0.1:{port}/c/1"));
    assert_eq!(chapters[0].index, 0);

    let text = engine.content(&source, &book, &chapters[0]).unwrap();
    assert_eq!(text, "Hello\nWorld");

    let md5 = engine
        .eval(&source, "java.md5Encode('abc')", "", "")
        .unwrap();
    assert_eq!(md5, "900150983cd24fb0d6963f7d28e17f72");

    let _ = BookChapter::default();
    drop(server);
}

#[test]
fn the_rss_pipeline() {
    let (port, server) = serve();
    let source_json = format!(
        r##"{{
          "sourceName": "local feed",
          "sourceUrl": "http://127.0.0.1:{port}/feed",
          "ruleArticles": "div.item",
          "ruleTitle": "a@text",
          "ruleLink": "a@href",
          "rulePubDate": "time@text",
          "ruleDescription": "p@text",
          "ruleNextPage": "a#next@href",
          "ruleContent": "article"
        }}"##
    );
    let source: RssSource = serde_json::from_str(&source_json).unwrap();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();

    let page = engine.rss_articles(&source, "").unwrap();
    assert_eq!(page.articles.len(), 2);
    assert_eq!(page.articles[0].title, "Title 1");
    assert_eq!(
        page.articles[0].link,
        format!("http://127.0.0.1:{port}/post/1")
    );
    assert_eq!(page.articles[0].pub_date.as_deref(), Some("2024-01-01"));
    assert_eq!(page.articles[0].description.as_deref(), Some("Desc"));
    assert_eq!(page.articles[0].origin, source.source_url);
    assert_eq!(page.next_url, format!("http://127.0.0.1:{port}/feed2"));

    let second = engine.rss_articles(&source, &page.next_url).unwrap();
    assert_eq!(second.articles.len(), 1);
    assert_eq!(second.articles[0].order, 0);
    assert!(second.next_url.is_empty());

    // 裸选择器取文本（与书源正文规则一致）；要 HTML 就在规则里写 `@html`。
    let text = engine.rss_content(&source, &page.articles[0]).unwrap();
    assert_eq!(text, "Body\nPage two");
    drop(server);
}

#[test]
fn a_share_link_is_downloaded_and_split() {
    use legado_engine::import::Origin;

    let (port, server) = serve();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();
    let message = format!(
        "新合集来啦 legado://import/bookSource?src=http://127.0.0.1:{port}/collection.json 收好"
    );
    let imported = engine.import(&message).unwrap();
    assert_eq!(imported.declared_kind.as_deref(), Some("bookSource"));
    assert_eq!(imported.origin, Origin::Url);
    assert_eq!(
        imported.url.as_deref(),
        Some(format!("http://127.0.0.1:{port}/collection.json").as_str())
    );
    assert_eq!(imported.book_sources.len(), 1);
    assert_eq!(imported.rss_sources.len(), 1);
    assert_eq!(imported.replace_rules.len(), 1);
    // 未知字段必须原样活着回去
    assert_eq!(imported.book_sources[0]["bookSourceName"], "A");
    drop(server);
}

#[test]
fn an_explore_category_can_page() {
    let (port, server) = serve();
    let source_json = format!(
        r##"{{
          "bookSourceName": "local",
          "bookSourceUrl": "http://127.0.0.1:{port}",
          "exploreUrl": "[{{\"title\":\"全部\",\"url\":\"/find?page={{{{page}}}}\"}}]",
          "ruleSearch": {{ "bookList": "div.row", "name": "a@text", "author": "em@text", "bookUrl": "a@href" }}
        }}"##
    );
    let source: BookSource = serde_json::from_str(&source_json).unwrap();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();
    let category = format!("http://127.0.0.1:{port}/find?page={{{{page}}}}");

    // 翻页：`{{page}}` 换成了实际页码。
    for page in 1..=3 {
        let books = engine.explore_page(&source, &category, "", page).unwrap();
        assert_eq!(books[0].author, format!("page {page}"));
        assert_eq!(books[0].book_url, format!("http://127.0.0.1:{port}/book/1"));
    }

    // {{key}} 也真的传下去了
    let keyed = format!("http://127.0.0.1:{port}/find?page={{{{page}}}}&q={{{{key}}}}");
    let searched = engine.explore_page(&source, &keyed, "书", 2).unwrap();
    // `{{key}}` 与手机端一致：先按 UTF-8 百分号编码再拼进地址。
    assert_eq!(urlencoding::decode(&searched[0].name).unwrap(), "书");
    assert_eq!(searched[0].author, "page 2");

    // 老入口固定第一页（向后兼容）。
    let books = engine.explore(&source, &category).unwrap();
    assert_eq!(books[0].author, "page 1");
    drop(server);
}

#[test]
fn a_json_feed_uses_the_same_rules() {
    let (port, server) = serve();
    let source_json = format!(
        r##"{{
          "sourceName": "json feed",
          "sourceUrl": "http://127.0.0.1:{port}/feed.json",
          "ruleArticles": "$.items[*]",
          "ruleTitle": "$.title",
          "ruleLink": "$.url",
          "rulePubDate": "$.date",
          "ruleDescription": "$.summary",
          "ruleContent": "#content"
        }}"##
    );
    let source: RssSource = serde_json::from_str(&source_json).unwrap();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();

    let page = engine.rss_articles(&source, "").unwrap();
    assert_eq!(page.articles.len(), 2);
    assert_eq!(page.articles[0].title, "J1");
    assert_eq!(
        page.articles[0].link,
        format!("http://127.0.0.1:{port}/post/1")
    );
    assert_eq!(page.articles[0].pub_date.as_deref(), Some("2024-01-01"));
    assert_eq!(page.articles[1].description.as_deref(), Some("S2"));
    // JSON 里没有 pubDate 字段的条目留空，不该报错
    assert_eq!(page.articles[1].pub_date, None);
    drop(server);
}

#[test]
fn cookies_round_trip_through_the_jar() {
    let engine = Engine::new(Config {
        direct: true,
        ..Config::default()
    })
    .unwrap();
    let source = BookSource {
        book_source_url: "https://cookie.test".to_string(),
        ..Default::default()
    };
    engine
        .set_cookie(&source, "https://cookie.test/a/b", "a=1; b=2")
        .unwrap();
    let header = engine
        .cookie_header(&source, "https://cookie.test/a/b")
        .unwrap();
    assert!(header.contains("a=1"), "{header}");
    assert!(header.contains("b=2"), "{header}");
}

#[test]
fn one_handle_answers_several_threads() {
    use std::ffi::{CStr, CString};

    use legado_engine::abi::{
        legado_close, legado_last_error, legado_open, legado_search, LegadoEngine,
    };

    let (port, server) = serve();
    let config = CString::new(r#"{"direct":true,"timeoutSecs":5}"#).unwrap();
    let engine = legado_open(config.as_ptr());
    assert!(!engine.is_null());
    let address = engine as usize;

    let source = format!(
        r##"{{"bookSourceName":"local","bookSourceUrl":"http://127.0.0.1:{port}",
             "searchUrl":"/find?q={{{{key}}}}&page={{{{page}}}}",
             "ruleSearch":{{"bookList":"div.row","name":"a@text","bookUrl":"a@href"}}}}"##
    );
    let handles: Vec<_> = ["甲", "乙", "丙"]
        .into_iter()
        .map(|key| {
            let source = source.clone();
            thread::spawn(move || {
                let engine = address as *mut LegadoEngine;
                let source = CString::new(source).unwrap();
                let key_c = CString::new(key).unwrap();
                let answer = legado_search(engine, source.as_ptr(), key_c.as_ptr(), 1);
                assert!(
                    !answer.is_null(),
                    "{}",
                    unsafe { CStr::from_ptr(legado_last_error(engine)) }.to_string_lossy()
                );
                let text = unsafe { CStr::from_ptr(answer) }
                    .to_string_lossy()
                    .to_string();
                (key, text)
            })
        })
        .collect();

    // 每个线程拿到的是**自己**的答案：返回值按线程各存一份。
    for handle in handles {
        let (key, text) = handle.join().unwrap();
        let books: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        let name = books[0]["name"].as_str().unwrap();
        assert_eq!(urlencoding::decode(name).unwrap(), key);
    }
    legado_close(engine);
    drop(server);
}

/// 一个按脚本应答的迷你服务器：handler 收到 (第几次请求, 路径)，返回完整响应。
/// 用来测超时、退避、限速这类"时间"行为。
fn serve_custom<F>(handler: F) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>)
where
    F: Fn(usize, &str) -> String + Send + 'static,
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let counter = Arc::new(AtomicUsize::new(0));
    let seen = counter.clone();
    thread::spawn(move || {
        for stream in listener.incoming().take(20) {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let index = seen.fetch_add(1, Ordering::SeqCst);
            let response = handler(index, &path);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, counter)
}

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn timeout_secs_is_actually_honoured() {
    // 服务器读完请求就装死。以前 `timeoutSecs` 被 `max(60, …)` 吃掉，
    // 设成 2 秒实际要等 60 秒。
    let (port, _) = serve_custom(|_, _| {
        thread::sleep(std::time::Duration::from_secs(5));
        String::new()
    });
    let source: BookSource = serde_json::from_str(&format!(
        r##"{{"bookSourceName":"slow","bookSourceUrl":"http://127.0.0.1:{port}",
             "searchUrl":"/search?q={{{{key}}}},{{\"retry\":0}}",
             "ruleSearch":{{"bookList":"div.row","name":"a@text"}}}}"##
    ))
    .unwrap();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(2),
        ..Config::default()
    })
    .unwrap();

    let started = std::time::Instant::now();
    let error = engine.search(&source, "x", 1).unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "waited {elapsed:?} for a 2s timeout: {error:#}"
    );
}

#[test]
fn a_429_retry_after_is_honoured() {
    let (port, count) = serve_custom(|index, _| {
        if index == 0 {
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string()
        } else {
            html_response(r#"<div class="row"><a href="/book/1">One</a></div>"#)
        }
    });
    let source: BookSource = serde_json::from_str(&format!(
        r##"{{"bookSourceName":"limited","bookSourceUrl":"http://127.0.0.1:{port}",
             "searchUrl":"/search?q={{{{key}}}}",
             "ruleSearch":{{"bookList":"div.row","name":"a@text","bookUrl":"a@href"}}}}"##
    ))
    .unwrap();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(10),
        ..Config::default()
    })
    .unwrap();

    let started = std::time::Instant::now();
    let books = engine.search(&source, "x", 1).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(books.len(), 1);
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(
        elapsed >= std::time::Duration::from_millis(900),
        "retried after {elapsed:?} instead of the requested 1s"
    );
}

#[test]
fn a_source_rate_limit_applies_to_engine_requests() {
    let (port, count) =
        serve_custom(|_, _| html_response(r#"<div class="row"><a href="/book/1">One</a></div>"#));
    // `concurrentRate: "1/300"` = 每 300ms 只放一个请求。
    let source: BookSource = serde_json::from_str(&format!(
        r##"{{"bookSourceName":"rated","bookSourceUrl":"http://127.0.0.1:{port}",
             "concurrentRate":"1/300",
             "searchUrl":"/search?q={{{{key}}}}",
             "ruleSearch":{{"bookList":"div.row","name":"a@text","bookUrl":"a@href"}}}}"##
    ))
    .unwrap();
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();

    let started = std::time::Instant::now();
    for _ in 0..3 {
        engine.search(&source, "x", 1).unwrap();
    }
    let elapsed = started.elapsed();
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
    // 第 1 个立刻发，第 2 个等 ~300ms，第 3 个再等 ~300ms。
    assert!(
        elapsed >= std::time::Duration::from_millis(550),
        "three requests in {elapsed:?}: the rate limit was not applied"
    );
}
