//! `Engine::download`: bytes go to disk verbatim, and the charset is sniffed
//! from the file when the server does not declare one.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;

use legado_engine::model::book_source::BookSource;
use legado_engine::pipeline::{Config, Engine};

fn serve(gbk: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Detached: the test process exits when the harness is done, and joining
    // would block in `incoming()` waiting for a connection that never comes.
    thread::spawn(move || {
        for stream in listener.incoming().take(6) {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (status, content_type, body): (&str, &str, Vec<u8>) = match path {
                "/gbk.txt" => ("200 OK", "text/plain", gbk.to_vec()),
                "/utf8.txt" => (
                    "200 OK",
                    "text/plain; charset=utf-8",
                    "第一行\n第二行\n".as_bytes().to_vec(),
                ),
                _ => ("404 Not Found", "text/plain", b"no".to_vec()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn source(base: &str) -> BookSource {
    BookSource {
        book_source_name: "local".to_string(),
        book_source_url: base.to_string(),
        ..Default::default()
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("legado-download-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_gbk_file_lands_byte_exact_and_is_reported_as_gb18030() {
    let text = "更多精校小说尽在知轩藏书下载";
    let (encoded, _, _) = encoding_rs::GBK.encode(text);
    let gbk: &'static [u8] = Box::leak(encoded.into_owned().into_boxed_slice());
    let base = serve(gbk);
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();

    let dir = temp_dir("gbk");
    let dest = dir.join("nested").join("book.txt");
    let done = engine.download(&source(&base), "/gbk.txt", &dest).unwrap();

    assert_eq!(done.path, dest.to_string_lossy());
    assert_eq!(done.size, gbk.len() as u64);
    assert_eq!(done.charset, "gb18030");
    assert_eq!(done.url, format!("{base}/gbk.txt"));
    assert_eq!(std::fs::read(&dest).unwrap(), gbk);
    assert!(!dir.join("nested").join("book.txt.part").exists());
    assert_eq!(
        String::from_utf8_lossy(&std::fs::read(&dest).unwrap()),
        String::from_utf8_lossy(gbk)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_declared_utf8_file_keeps_its_bytes() {
    let base = serve(b"");
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();
    let dir = temp_dir("utf8");
    let dest = dir.join("book.txt");

    let done = engine.download(&source(&base), "/utf8.txt", &dest).unwrap();
    assert_eq!(done.charset, "utf-8");
    assert_eq!(
        done.content_type.as_deref(),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "第一行\n第二行\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_download_writes_nothing() {
    let base = serve(b"");
    let engine = Engine::new(Config {
        direct: true,
        timeout_secs: Some(5),
        ..Config::default()
    })
    .unwrap();
    let dir = temp_dir("missing");
    let dest = dir.join("book.txt");

    let error = engine
        .download(&source(&base), "/missing.txt", &dest)
        .unwrap_err();
    assert!(error.to_string().contains("404"), "{error:#}");
    assert!(!dest.exists());
    assert!(!dir.join("book.txt.part").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
