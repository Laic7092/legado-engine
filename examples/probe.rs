//! Drive one book source from the command line.
//!
//! usage: cargo run --release --example probe -- <sources.json> search <key>
//!        ... explore <url> [page]
//!        ... info <bookUrl>
//!        ... toc <bookUrl>
//!        ... content <bookUrl> <chapterUrl>
//!        ... download <bookUrl> [destDir]
//!        ... eval <script>
//!        import <share-link-or-url-or-json>
//!        replace <rules.json> <text-file> [title|content]
//!        rss <rss-source.json> [sortUrl] [content <index>]
//!
//! `download` uses ruleBookInfo.downloadUrls (one `name::url` per line, or a
//! bare URL), then writes the file with legado_download's engine path.
//!
//! The last three do not need a book source: `import` exercises legado_import,
//! `replace` runs replace rules over a text file, `rss` walks an RSS source.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use legado_engine::model::book::Book;
use legado_engine::model::book_chapter::BookChapter;
use legado_engine::model::book_source::BookSource;
use legado_engine::model::replace_rule::ReplaceRule;
use legado_engine::model::rss::{rss_source_from_value, RssSource};
use legado_engine::parser::replace::{apply, ReplaceContext, Target};
use legado_engine::pipeline::{Config, Engine};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow!("usage: probe <sources.json> <search|explore|info|toc|content|download|import|replace|rss> …"))?;
    if matches!(path.as_str(), "import" | "replace" | "rss") {
        return standalone(&path, args.collect());
    }
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
    if let (Ok(user), Ok(pass)) = (std::env::var("LEGADO_USER"), std::env::var("LEGADO_PASS")) {
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
                println!(
                    "{}\t{}\t{}\t{}",
                    book.name,
                    book.author,
                    book.book_url,
                    book.last_chapter.as_deref().unwrap_or("")
                );
            }
        }
        "explore" => {
            let url = args.next().ok_or_else(|| anyhow!("explore needs a url"))?;
            let page = args
                .next()
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(1);
            for book in engine.explore_page(&source, &url, "", page)? {
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
            println!(
                "{}",
                engine.eval(&source, &script, "", &source.book_source_url)?
            );
        }
        "content" => {
            let book_url = args
                .next()
                .ok_or_else(|| anyhow!("content needs a bookUrl"))?;
            let chapter_url = args
                .next()
                .ok_or_else(|| anyhow!("content needs a chapterUrl"))?;
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
            let book_url = args
                .next()
                .ok_or_else(|| anyhow!("download needs a bookUrl"))?;
            let dir = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
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

/// `import` / `replace` / `rss`: no book source involved.
fn standalone(action: &str, args: Vec<String>) -> Result<()> {
    let engine = Engine::new(Config {
        storage_dir: std::env::var_os("LEGADO_STORAGE").map(PathBuf::from),
        ..Default::default()
    })?;
    match action {
        "import" => {
            let text = args
                .first()
                .ok_or_else(|| anyhow!("import needs a share link, a URL or JSON"))?;
            let imported = engine.import(text)?;
            println!("{}", serde_json::to_string_pretty(&imported)?);
        }
        "replace" => {
            let rules_path = args
                .first()
                .ok_or_else(|| anyhow!("replace needs <rules.json> <text-file> [title|content]"))?;
            let text_path = args
                .get(1)
                .ok_or_else(|| anyhow!("replace needs <rules.json> <text-file> [title|content]"))?;
            let rules_value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(rules_path)?)?;
            let rules: Vec<ReplaceRule> = match rules_value {
                serde_json::Value::Array(items) => {
                    serde_json::from_value(serde_json::Value::Array(items))?
                }
                other => vec![serde_json::from_value(other)?],
            };
            let text = std::fs::read_to_string(text_path)?;
            let target = match args.get(2).map(String::as_str) {
                Some("title") => Target::Title,
                _ => Target::Content,
            };
            let outcome = apply(&text, &rules, &ReplaceContext::default(), target);
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
        "rss" => {
            let path = args
                .first()
                .ok_or_else(|| anyhow!("rss needs an RSS source JSON"))?;
            let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
            let source: RssSource = rss_source_from_value(match value {
                serde_json::Value::Array(mut items) if !items.is_empty() => items.remove(0),
                other => other,
            })?;
            let sort_url = args
                .get(1)
                .filter(|value| value.as_str() != "content")
                .map(String::as_str)
                .unwrap_or_default();
            let page = engine.rss_articles(&source, sort_url)?;
            for article in &page.articles {
                println!(
                    "{}\t{}\t{}",
                    article.title,
                    article.pub_date.as_deref().unwrap_or(""),
                    article.link
                );
            }
            if !page.next_url.is_empty() {
                println!("next: {}", page.next_url);
            }
            let position = args.iter().position(|value| value == "content");
            if let Some(position) = position {
                let index = args
                    .get(position + 1)
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let article = page
                    .articles
                    .get(index)
                    .ok_or_else(|| anyhow!("no article {index} on this page"))?;
                let text = engine.rss_content(&source, article)?;
                println!("{} bytes", text.len());
                println!("{}", text.chars().take(400).collect::<String>());
            }
        }
        other => return Err(anyhow!("unknown action {other}")),
    }
    Ok(())
}
