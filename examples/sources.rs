//! Run `search` against the enabled sources of a Legado source file.
//!
//! usage: cargo run --release --example sources -- <sources.json> [js|plain]

use anyhow::{anyhow, Result};
use legado_engine::model::book_source::BookSource;
use legado_engine::pipeline::{Config, Engine};

fn has_js(rule: Option<&str>) -> bool {
    rule.is_some_and(|rule| rule.contains("@js:") || rule.contains("<js>"))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow!("usage: sources <sources.json> [js|plain]"))?;
    let filter = args.next().unwrap_or_default();
    let sources: Vec<BookSource> = serde_json::from_str(&std::fs::read_to_string(&path)?)?;

    let engine = Engine::new(Config::default())?;
    let (mut tried, mut ok) = (0usize, 0usize);

    for source in sources.iter().filter(|source| source.is_enabled()) {
        if source.book_source_type.unwrap_or(0) != 0 {
            continue;
        }
        let search_url = source.search_url.as_deref().unwrap_or_default();
        let list_rule = source
            .rule_search
            .as_ref()
            .and_then(|rule| rule.book_list.as_deref());
        let js = has_js(Some(search_url)) || has_js(list_rule);
        if (filter == "js" && !js) || (filter == "plain" && js) {
            continue;
        }
        if search_url.trim().is_empty() || list_rule.is_none_or(|rule| rule.trim().is_empty()) {
            continue;
        }

        tried += 1;
        match engine.search(source, "我的", 1) {
            Ok(books) => {
                if !books.is_empty() {
                    ok += 1;
                }
                println!("{}: {} books", source.book_source_name, books.len());
            }
            Err(error) => println!("{}: {error:#}", source.book_source_name),
        }
    }

    println!("tried {tried}, got results from {ok}");
    Ok(())
}
