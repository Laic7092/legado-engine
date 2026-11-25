use legado_engine::model::book_source::BookSource;
use legado_engine::parser::js::{eval_js, eval_js_with_bindings, with_source};
use legado_engine::parser::rule_engine::RuleEngine;

fn main() {
    let source: BookSource = serde_json::from_str(
        r##"{
      "bookSourceName": "local-http",
      "bookSourceUrl": "http://127.0.0.1:8765",
      "searchUrl": "/search?q={{key}}",
      "ruleSearch": {
        "bookList": "div.row",
        "name": "@js:JSON.parse(java.ajax('/api/book/1')).title",
        "author": "@js:java.getString('a@text')",
        "intro": "@js:JSON.parse(java.ajax('/api/book/1')).author",
        "bookUrl": "a@href"
      },
      "ruleBookInfo": {
        "init": "@js:java.setContent(result); result",
        "name": "#t@text",
        "intro": "@js:java.getString('#intro@text')"
      },
      "ruleContent": {"content": "@js:result.trim()"}
    }"##,
    )
    .unwrap();
    let engine = RuleEngine::new().unwrap();

    let out = with_source(&source, || {
        eval_js_with_bindings(
            "JSON.parse(java.ajax('/api/book/1')).title + '|' + java.getString('a@text')",
            "<div class='row'><a href='/book/1'>One</a></div>",
            "http://127.0.0.1:8765/search",
            &std::collections::HashMap::new(),
        )
    });
    println!("host js: {:?}", out);

    let html = r#"<div class="row"><a href="/book/1">One</a><em>Ana</em></div>"#;
    let books = engine.search_books(&source, html, "http://127.0.0.1:8765/search?q=x");
    println!("search: {}", serde_json::to_string_pretty(&books).unwrap());

    let detail = engine.book_info(
        &source,
        "<h1 id='t'>Detail Title</h1><div id='intro'>An intro</div>",
        "http://127.0.0.1:8765/book/1",
        "http://127.0.0.1:8765/book/1",
    );
    println!("info: {}", serde_json::to_string_pretty(&detail).unwrap());

    let content = engine.content(
        &source,
        "  hello content  ",
        "http://127.0.0.1:8765/c/1",
        Some("http://127.0.0.1:8765/book/1"),
    );
    println!("content: {content:?}");

    let _ = eval_js("1+1", "", "");
}
