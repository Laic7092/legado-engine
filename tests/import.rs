//! 导入：分享链接、base64、裸 JSON，以及订阅源的列表摘要。全部离线。

use std::ffi::{CStr, CString};

use base64::Engine as _;
use legado_engine::abi::{legado_import, legado_last_error, legado_rss_sources, legado_sources};

/// 调一次不带 handle 的 ABI，返回 `Ok(JSON)` 或 `Err(错误信息)`。
fn call<F>(f: F, input: &str) -> Result<String, String>
where
    F: FnOnce(*const std::ffi::c_char) -> *const std::ffi::c_char,
{
    let input = CString::new(input).unwrap();
    let out = f(input.as_ptr());
    if out.is_null() {
        let error = legado_last_error(std::ptr::null());
        let text = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .to_string();
        return Err(text);
    }
    Ok(unsafe { CStr::from_ptr(out) }.to_string_lossy().to_string())
}

fn import(text: &str) -> Result<serde_json::Value, String> {
    call(|ptr| legado_import(std::ptr::null_mut(), ptr), text)
        .map(|json| serde_json::from_str(&json).unwrap())
}

#[test]
fn inline_json_needs_no_engine() {
    let value = import(
        r#"[{"bookSourceUrl":"https://a.test","bookSourceName":"A"},
            {"sourceUrl":"https://b.test","sourceName":"B","ruleArticles":".x"},
            {"name":"删广告","pattern":"广告","replacement":""}]"#,
    )
    .unwrap();
    assert_eq!(value["origin"], "inline");
    assert_eq!(value["bookSources"].as_array().unwrap().len(), 1);
    assert_eq!(value["rssSources"].as_array().unwrap().len(), 1);
    assert_eq!(value["replaceRules"].as_array().unwrap().len(), 1);
    assert_eq!(value["unknown"], 0);
}

#[test]
fn a_share_link_with_base64_needs_no_engine_either() {
    let payload = r#"[{"bookSourceUrl":"https://a.test"}]"#;
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let value = import(&format!("legado://import/bookSource?src=base64:{encoded}")).unwrap();
    assert_eq!(value["declaredKind"], "bookSource");
    assert_eq!(value["origin"], "base64");
    assert_eq!(value["bookSources"].as_array().unwrap().len(), 1);
    // 原始对象原样返回，未知字段不丢
    assert_eq!(value["bookSources"][0]["bookSourceUrl"], "https://a.test");
}

#[test]
fn a_collection_wrapper_is_opened() {
    let value =
        import(r#"{"bookSources":[{"bookSourceUrl":"https://a.test"}],"other":1}"#).unwrap();
    assert_eq!(value["bookSources"].as_array().unwrap().len(), 1);
    // 外壳不是"未识别"：它是包装，里面的东西已经找到了。
    assert_eq!(value["unknown"], 0);
}

#[test]
fn an_unrecognised_rule_is_counted() {
    let value =
        import(r#"[{"bookSourceUrl":"https://a.test"},{"name":"字典","urlRule":"$.x"}]"#).unwrap();
    assert_eq!(value["bookSources"].as_array().unwrap().len(), 1);
    assert_eq!(value["unknown"], 1);
}

#[test]
fn something_unrecognisable_says_what_is_wrong() {
    let error = import(r#"[{"name":"x","rule":".a"}]"#).unwrap_err();
    assert!(error.contains("no book sources"), "{error}");
}

#[test]
fn a_garbled_payload_does_not_panic() {
    let error = import("这不是链接也不是 JSON").unwrap_err();
    assert!(error.contains("neither JSON"), "{error}");
}

#[test]
fn a_data_url_is_base64_too() {
    let payload = r#"[{"bookSourceUrl":"https://a.test"}]"#;
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let value = import(&format!("data:application/json;base64,{encoded}")).unwrap();
    assert_eq!(value["origin"], "base64");
    assert_eq!(value["bookSources"].as_array().unwrap().len(), 1);
}

#[test]
fn the_scheme_and_the_kind_are_case_insensitive() {
    let payload = r#"[{"bookSourceUrl":"https://a.test"}]"#;
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let value = import(&format!("LEGADO://import/BookSource?src=BASE64:{encoded}")).unwrap();
    assert_eq!(value["declaredKind"], "BookSource");
    assert_eq!(value["bookSources"].as_array().unwrap().len(), 1);
}

#[test]
fn an_escaped_src_is_decoded_before_use() {
    // 百分号转义过的地址：没有 handle 时应该报"需要 handle"，说明地址解出来了。
    let error = import("legado://import/bookSource?src=https%3A%2F%2Fa.test%2Fb.json").unwrap_err();
    assert!(error.contains("https://a.test/b.json"), "{error}");
    assert!(error.contains("engine handle"), "{error}");
}

#[test]
fn a_prose_message_with_a_bare_link_says_it_needs_the_engine() {
    let error = import("看这个 https://a.test/b.json 。收好").unwrap_err();
    assert!(error.contains("https://a.test/b.json"), "{error}");
}

#[test]
fn gzip_and_gbk_payloads_decode() {
    use legado_engine::import::decode_bytes;
    let plain = br#"[{"bookSourceUrl":"https://a.test"}]"#;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut encoder, plain).unwrap();
    let gz = encoder.finish().unwrap();
    assert_eq!(decode_bytes(&gz, None).unwrap().as_bytes(), plain);

    // 老站点直接吐 GBK：不能按 UTF-8 有损解，否则中文源名全变问号。
    let text = r#"[{"bookSourceName":"中文源","bookSourceUrl":"https://a.test"}]"#;
    let (gbk, _, _) = encoding_rs::GBK.encode(text);
    assert_eq!(decode_bytes(&gbk, None).unwrap(), text);
}

#[test]
fn rss_sources_become_summaries() {
    let json = call(
        |ptr| legado_rss_sources(ptr),
        r#"[{"sourceName":"Feed","sourceUrl":"https://b.test",
             "sourceGroup":"科技","sortUrl":"[{\"title\":\"全部\",\"url\":\"/all\"}]",
             "ruleArticles":"div.item@js:result"}]"#,
    )
    .unwrap();
    let values: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["name"], "Feed");
    assert_eq!(values[0]["group"], "科技");
    assert_eq!(values[0]["enabled"], true);
    assert_eq!(values[0]["hasJs"], true);
    assert!(values[0]["sortUrl"].as_str().unwrap().contains("全部"));
}

#[test]
fn a_book_source_is_not_an_rss_source() {
    let error = call(
        |ptr| legado_rss_sources(ptr),
        r#"[{"bookSourceUrl":"https://a.test"}]"#,
    )
    .unwrap_err();
    assert!(error.contains("no RSS sources"), "{error}");
}

#[test]
fn a_legacy_collection_still_imports() {
    // 2.x 的字段名（`enable` / `ruleSearchUrl` / `ruleChapterList` / `ruleArticleList` …）
    // 在今天下载到的合集里仍然到处可见。
    let value = import(include_str!("fixtures/legacy-collection.json")).unwrap();
    assert_eq!(value["bookSources"].as_array().unwrap().len(), 2);
    assert_eq!(value["rssSources"].as_array().unwrap().len(), 1);
    assert_eq!(value["replaceRules"].as_array().unwrap().len(), 1);
    assert_eq!(value["unknown"], 0);

    // 迁移后的字段要真的落到新键名上，否则源进得去、用不了。
    let books = serde_json::to_string(&value["bookSources"]).unwrap();
    let summaries = call(|ptr| legado_sources(ptr), &books).unwrap();
    let summaries: Vec<serde_json::Value> = serde_json::from_str(&summaries).unwrap();
    assert_eq!(summaries[0]["url"], "https://old.test");
    assert_eq!(summaries[0]["enabled"], false);
    assert_eq!(summaries[0]["exploreUrl"], "/find");
    assert_eq!(summaries[0]["searchUrl"], "/search?q={{key}}&page={{page}}");
    assert_eq!(summaries[1]["hasJs"], true);
}
