use legado_engine::model::book_source::BookSource;
use legado_engine::parser::js::eval_js;
use legado_engine::parser::rule_engine::RuleEngine;

fn main() {
    let source: BookSource = serde_json::from_str(r##"{
      "bookSourceName": "basic",
      "bookSourceUrl": "https://example.com",
      "searchUrl": "/search?q={{key}}",
      "ruleSearch": {"bookList": "div.row", "name": "a@text", "author": "em@text", "bookUrl": "a@href"},
      "ruleToc": {"chapterList": "#list a", "chapterName": "text", "chapterUrl": "href"},
      "ruleContent": {"content": "@js:java.md5Encode(result)"}
    }"##).unwrap();
    let engine = RuleEngine::new().unwrap();

    let html = r#"<div class="row"><a href="/book/1">One</a><em>Ana</em></div>
                  <div class="row"><a href="/book/2">Two</a><em>Bo</em></div>"#;
    let books = engine.search_books(&source, html, "https://example.com/search");
    println!("search: {}", serde_json::to_string_pretty(&books).unwrap());

    let toc = r##"<div id="list"><a href="/c/1">Chapter 1</a><a href="/c/2">Chapter 2</a></div>"##;
    let (chapters, next) = engine.chapter_list(
        &source,
        toc,
        "https://example.com/book/1",
        Some("https://example.com/book/1"),
    );
    println!(
        "toc: {} next={next:?}",
        serde_json::to_string_pretty(&chapters).unwrap()
    );

    let content = engine.content(
        &source,
        "<p>hello</p>",
        "https://example.com/c/1",
        Some("https://example.com/book/1"),
    );
    println!("content: {content}");

    let js = eval_js(
        "java.md5Encode('abc') + '|' + java.base64Encode('hello') + '|' + (1 + 2)",
        "",
        "https://example.com",
    )
    .unwrap();
    println!("js: {js}");
}
