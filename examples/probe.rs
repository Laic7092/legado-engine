//! Drive one book source from the command line.
//!
//! usage: cargo run --release --example probe -- <sources.json> search <key>
//!        ... explore <url>
//!        ... info <bookUrl>
//!        ... toc <bookUrl>
//!        ... content <bookUrl> <chapterUrl>
//!        ... download <bookUrl> [destDir]
//!        ... eval <script>
//!
//! `download` uses ruleBookInfo.downloadUrls (one `name::url` per line, or a
//! bare URL), then writes the file with legado_download's engine path.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use legado_engine::model::book::Book;
use legado_engine::model::book_chapter::BookChapter;
use legado_engine::model::book_source::BookSource;
use legado_engine::pipeline::{Config, Engine};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow!("usage: probe <sources.json> <search|explore|info|toc|content|download> …"))?;
    let action = args.next().unwrap_or_else(|| "search".to_string());
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let sources: Vec<BookSource> = match value {
        serde_json::Value::Array(_) => serde_json::from_value(value)?,
        other => vec![serde_json::from_value(other)?],
    };
    let source = sources
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("{path} has no source"))?;

    let engine = Engine::new(Config {
        storage_dir: std::env::var_os("LEGADO_STORAGE").map(PathBuf::from),
        ..Default::default()
    })?;

    // Optional credentials for sources that log in from jsLib: keep them out of
    // the source JSON (and out of shell history) via the environment.
    if let (Ok(user), Ok(pass)) = (
        std::env::var("LEGADO_USER"),
        std::env::var("LEGADO_PASS"),
    ) {
        let script = format!(
            "source.put('wenku8_user',{});source.put('wenku8_pass',{});",
            serde_json::to_string(&user)?,
            serde_json::to_string(&pass)?
        );
        engine.eval(&source, &script, "", &source.book_source_url)?;
    }

    match action.as_str() {
        "search" => {
            let key = args.next().unwrap_or_else(|| "我的".to_string());
            for book in engine.search(&source, &key, 1)? {
                println!("{}\t{}\t{}\t{}", book.name, book.author, book.book_url, book.last_chapter.as_deref().unwrap_or(""));
            }
        }
        "explore" => {
            let url = args.next().ok_or_else(|| anyhow!("explore needs a url"))?;
            for book in engine.explore(&source, &url)? {
                println!("{}\t{}\t{}", book.name, book.author, book.book_url);
            }
        }
        "info" => {
            let url = args.next().ok_or_else(|| anyhow!("info needs a bookUrl"))?;
            let book = engine.info(&source, &url)?;
            println!("{}", serde_json::to_string_pretty(&book)?);
        }
        "toc" => {
            let url = args.next().ok_or_else(|| anyhow!("toc needs a bookUrl"))?;
            let book = engine.info(&source, &url)?;
            let chapters = engine.toc(&source, &book)?;
            println!("{} chapters", chapters.len());
            for chapter in chapters.iter().take(5) {
                println!("{}\t{}", chapter.title, chapter.url);
            }
            if let Some(last) = chapters.last() {
                println!("…\t{}", last.url);
            }
        }
        "eval" => {
            let script = args.next().ok_or_else(|| anyhow!("eval needs a script"))?;
            println!("{}", engine.eval(&source, &script, "", &source.book_source_url)?);
        }
        "content" => {
            let book_url = args.next().ok_or_else(|| anyhow!("content needs a bookUrl"))?;
            let chapter_url = args.next().ok_or_else(|| anyhow!("content needs a chapterUrl"))?;
            let book = engine.info(&source, &book_url)?;
            let chapter = BookChapter {
                title: String::new(),
                url: chapter_url,
                ..Default::default()
            };
            let text = engine.content(&source, &book, &chapter)?;
            println!("{} bytes", text.len());
            println!("{}", text.chars().take(400).collect::<String>());
        }
        "download" => {
            let book_url = args.next().ok_or_else(|| anyhow!("download needs a bookUrl"))?;
            let dir = args.next().map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let book: Book = engine.info(&source, &book_url)?;
            let raw = book
                .download_urls
                .as_deref()
                .ok_or_else(|| anyhow!("ruleBookInfo.downloadUrls produced nothing"))?;
            for line in raw.lines().filter(|line| !line.trim().is_empty()) {
                let (name, url) = line
                    .split_once("::")
                    .map(|(name, url)| (name.trim(), url.trim()))
                    .unwrap_or(("download", line.trim()));
                let ext = url.rsplit('.').next().unwrap_or("bin");
                let dest = dir.join(format!("{}.{ext}", name.replace(['/', '\\'], "_")));
                let done = engine.download(&source, url, &dest)?;
                println!("{}", serde_json::to_string(&done)?);
            }
        }
        other => return Err(anyhow!("unknown action {other}")),
    }
    Ok(())
}
