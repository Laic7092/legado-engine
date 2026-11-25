use std::ffi::{CStr, CString};

use legado_engine::abi::legado_sources;

fn call(json: &str) -> Option<String> {
    let json = CString::new(json).unwrap();
    let out = legado_sources(json.as_ptr());
    if out.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string())
}

#[test]
fn a_collection_becomes_source_metadata() {
    let text = call(
        r##"[
          {"bookSourceName":"A","bookSourceUrl":"https://a.test",
           "bookSourceGroup":"小说, 完本","bookSourceType":0,
           "searchUrl":"/s?q={{key}}",
           "ruleSearch":{"bookList":"div.row","name":"@js:result"}},
          {"bookSourceName":"B","bookSourceUrl":"https://b.test",
           "ruleFindUrl":"/find"},
          {"bookSourceUrl":""}
        ]"##,
    )
    .unwrap();
    let values: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["name"], "A");
    assert_eq!(values[0]["group"], "小说, 完本");
    assert_eq!(values[0]["hasJs"], true);
    assert_eq!(values[1]["name"], "B");
    assert_eq!(values[1]["exploreUrl"], "/find");
    assert_eq!(values[1]["hasJs"], false);
}

#[test]
fn a_single_source_reads() {
    let text = call(r#"{"bookSourceName":"One","bookSourceUrl":"x.test"}"#).unwrap();
    let values: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["url"], "x.test");
}

#[test]
fn no_sources_is_an_error() {
    assert!(call(r#"[{"bookSourceUrl":""}]"#).is_none());
}
