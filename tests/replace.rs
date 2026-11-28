//! 替换净化（`legado_replace`）：过滤、范围、脚本、保护、报错。不需要网络。

use std::ffi::{CStr, CString};

use legado_engine::abi::{legado_last_error, legado_replace};

fn replace(
    rules: &str,
    text: &str,
    context: Option<&str>,
    target: Option<&str>,
) -> serde_json::Value {
    let rules = CString::new(rules).unwrap();
    let text = CString::new(text).unwrap();
    let context = context.map(|value| CString::new(value).unwrap());
    let target = target.map(|value| CString::new(value).unwrap());
    let out = legado_replace(
        rules.as_ptr(),
        text.as_ptr(),
        context
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr()),
        target
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr()),
    );
    if out.is_null() {
        let error = legado_last_error(std::ptr::null());
        panic!(
            "legado_replace failed: {}",
            unsafe { CStr::from_ptr(error) }.to_string_lossy()
        );
    }
    serde_json::from_str(unsafe { CStr::from_ptr(out) }.to_str().unwrap()).unwrap()
}

#[test]
fn ads_are_cleaned_and_the_report_says_what_ran() {
    let value = replace(
        r#"[{"id":7,"name":"删本章说","pattern":"（本章完）","replacement":""}]"#,
        "正文（本章完）",
        None,
        None,
    );
    assert_eq!(value["text"], "正文");
    assert_eq!(value["applied"][0]["id"], 7);
    assert_eq!(value["applied"][0]["name"], "删本章说");
    assert_eq!(value["skipped"].as_array().unwrap().len(), 0);
}

#[test]
fn a_disabled_rule_is_reported_with_its_reason() {
    let value = replace(
        r#"[{"id":1,"name":"关了的","pattern":"广告","replacement":"","isEnabled":false}]"#,
        "广告",
        None,
        None,
    );
    assert_eq!(value["text"], "广告");
    assert_eq!(value["skipped"][0]["reason"], "disabled");
}

#[test]
fn a_rule_scoped_to_another_book_is_skipped() {
    let rules =
        r#"[{"id":1,"name":"只对某本","pattern":"广告","replacement":"","scope":"某本书"}]"#;
    let value = replace(rules, "广告", Some(r#"{"bookName":"另一本书"}"#), None);
    assert_eq!(value["text"], "广告");
    assert_eq!(value["skipped"][0]["reason"], "scope");

    let value = replace(rules, "广告", Some(r#"{"bookName":"某本书"}"#), None);
    assert_eq!(value["text"], "");
}

#[test]
fn a_title_rule_only_runs_when_the_rule_allows_titles() {
    let rules =
        r#"[{"id":1,"name":"标题","pattern":"第","replacement":"Chapter ","scopeTitle":true}]"#;
    let value = replace(rules, "第1章", None, Some("title"));
    assert_eq!(value["text"], "Chapter 1章");

    // 反过来：正文开关关掉的规则不该碰正文，但要说明为什么没碰。
    let only_title = r#"[{"id":2,"name":"只改标题","pattern":"第","replacement":"X","scopeTitle":true,"scopeContent":false}]"#;
    let value = replace(only_title, "第1章", None, None);
    assert_eq!(value["text"], "第1章");
    assert_eq!(value["skipped"][0]["reason"], "content");
}

#[test]
fn a_script_rule_runs_in_the_sandbox() {
    let rules = r#"[{"id":1,"name":"全角转半角","pattern":"[０-９]","replacement":"@js:R = result; String.fromCharCode(R.charCodeAt(0) - 0xFEE0)","isRegex":true}]"#;
    assert_eq!(replace(rules, "１２", None, None)["text"], "12");
}

#[test]
fn an_image_survives_a_rule_that_scrubs_ad_urls() {
    let rules =
        r#"[{"id":1,"name":"删域名","pattern":"[a-z]+\\.com","replacement":"","isRegex":true}]"#;
    let value = replace(
        rules,
        r#"<img src="http://ad.com/x.png">看广告 ad.com"#,
        None,
        None,
    );
    assert_eq!(value["text"], r#"<img src="http://ad.com/x.png">看广告 "#);
}

#[test]
fn a_broken_pattern_is_an_error_not_a_crash() {
    let rules =
        r#"[{"id":1,"name":"写错了","pattern":"\\Qa\\E","replacement":"x","isRegex":true}]"#;
    let value = replace(rules, "aaa", None, None);
    assert_eq!(value["text"], "aaa");
    assert_eq!(value["errors"][0]["id"], 1);
    assert!(value["errors"][0]["error"]
        .as_str()
        .unwrap()
        .contains("\\Q"));
}

#[test]
fn an_unknown_target_is_rejected() {
    let rules = CString::new(r#"[]"#).unwrap();
    let text = CString::new("x").unwrap();
    let target = CString::new("cover").unwrap();
    let out = legado_replace(
        rules.as_ptr(),
        text.as_ptr(),
        std::ptr::null(),
        target.as_ptr(),
    );
    assert!(out.is_null());
    let error = legado_last_error(std::ptr::null());
    let error = unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .to_string();
    assert!(error.contains("target"), "{error}");
}

#[test]
fn an_empty_context_string_is_the_same_as_null() {
    let rules = r#"[{"id":1,"name":"r","pattern":"广告","replacement":""}]"#;
    let with_null = replace(rules, "广告", None, None);
    let with_empty = replace(rules, "广告", Some(""), None);
    assert_eq!(with_null["text"], "");
    assert_eq!(with_empty["text"], "");
}

#[test]
fn an_empty_rule_list_changes_nothing() {
    let value = replace("[]", "正文", None, None);
    assert_eq!(value["text"], "正文");
    assert_eq!(value["applied"].as_array().unwrap().len(), 0);
}

#[test]
fn a_literal_rule_does_not_expand_group_references() {
    // 非正则规则的 replacement 是字面量（Java String.replace 语义），`$1` 不该展开。
    let value = replace(r#"[{"pattern":"a","replacement":"[$1]"}]"#, "a", None, None);
    assert_eq!(value["text"], "[$1]");
}

#[test]
fn scope_prefixes_select_the_source_or_the_book() {
    let by_source =
        r#"[{"id":1,"name":"按源","pattern":"广告","replacement":"","scope":"source:site.test"}]"#;
    let outcome = replace(
        by_source,
        "广告",
        Some(r#"{"sourceUrl":"https://site.test"}"#),
        None,
    );
    assert_eq!(outcome["text"], "");

    let by_book =
        r#"[{"id":2,"name":"按书","pattern":"广告","replacement":"","scope":"book:/book/7"}]"#;
    let outcome = replace(
        by_book,
        "广告",
        Some(r#"{"bookUrl":"https://site.test/book/7"}"#),
        None,
    );
    assert_eq!(outcome["text"], "");
}

#[test]
fn an_excluded_book_keeps_its_ads() {
    let rules =
        r#"[{"id":1,"name":"排除","pattern":"广告","replacement":"","excludeScope":"追更的书"}]"#;
    let outcome = replace(rules, "广告", Some(r#"{"bookName":"追更的书"}"#), None);
    assert_eq!(outcome["text"], "广告");
    assert_eq!(outcome["skipped"][0]["reason"], "excludeScope");
}

#[test]
fn a_runaway_script_times_out_and_keeps_the_text() {
    let rules = r#"[{"id":1,"name":"死循环","pattern":"a","replacement":"@js:while(true){}","isRegex":true,"timeoutMillisecond":200}]"#;
    let value = replace(rules, "aaa", None, None);
    assert_eq!(value["text"], "aaa");
    let error = value["errors"][0]["error"].as_str().unwrap();
    // 提示里要说人话：200 毫秒不能被四舍五入成 "0 秒"。
    assert!(error.contains("毫秒"), "{error}");
}

#[test]
fn the_same_handle_answer_survives_several_threads() {
    use std::thread;

    let handles: Vec<_> = (0..4)
        .map(|index| {
            thread::spawn(move || {
                let rules = format!(
                    r#"[{{"id":{index},"name":"t{index}","pattern":"广告","replacement":"x"}}]"#
                );
                let value = replace(&rules, "广告", None, None);
                (index, value["text"].as_str().unwrap().to_string())
            })
        })
        .collect();
    for handle in handles {
        let (index, text) = handle.join().unwrap();
        assert_eq!(text, "x", "thread {index}");
    }
}
