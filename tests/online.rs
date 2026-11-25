//! The online pipeline against a local HTTP server: search (including a
//! `java.ajax` field), detail, toc and content. No external network.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use legado_engine::model::book_chapter::BookChapter;
use legado_engine::model::book_source::BookSource;
use legado_engine::pipeline::{Config, Engine};

fn serve() -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        for stream in listener.incoming().take(12) {
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
