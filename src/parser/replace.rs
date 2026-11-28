//! 替换净化：把一组用户规则按顺序作用到文本上。
//!
//! 职责三件：按 `isEnabled` / `scope` / `scopeTitle` / `scopeContent` / `order`
//! 挑规则；执行字符串、Java 正则或 `@js:` 替换；报告 `applied` / `skipped` /
//! `errors`。被跳过的规则必须上报——净化不生效通常就是 scope 或标题开关写错了。
//!
//! 独立于 `legado_content`：本地 TXT / EPUB 没有书源；改规则后要能重刷已缓存
//! 章节而不重发 HTTP；缓存存原文。

use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::model::replace_rule::ReplaceRule;
use crate::parser::replace_rule_js::{apply_rules_with_timeout, ReplaceRuleStep};
use crate::parser::special_content_protector::protect_for_rules;

/// 规则作用的对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// 章节标题。
    Title,
    /// 正文。
    Content,
}

/// 判定 `scope` 用的上下文。字段都是子串匹配的原料，不需要全填。
///
/// 没有 `chapterTitle`：手机端的 scope 只匹配书名与书源地址，多一个"传了但没用"的
/// 字段只会让人以为按章节净化可行。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReplaceContext {
    pub book_name: String,
    pub book_url: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleRef {
    pub id: i64,
    pub name: String,
    /// 这条规则是否真的改动了文本。
    ///
    /// `applied` 只表示"执行过、没出错"：一条规则跑了但一个字符都没匹配到时，
    /// 它照样在 `applied` 里，只是 `changed` 为 false。排查"我加的规则没反应"
    /// 时，这个区分就是全部答案——是没执行，还是执行了但没匹配上。
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedRule {
    pub id: i64,
    pub name: String,
    /// `disabled` / `scope` / `excludeScope` / `title` / `content` / `emptyPattern`
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedRule {
    pub id: i64,
    pub name: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceOutcome {
    pub text: String,
    pub applied: Vec<RuleRef>,
    pub skipped: Vec<SkippedRule>,
    pub errors: Vec<FailedRule>,
}

/// 按顺序跑完整条规则链。
pub fn apply(
    text: &str,
    rules: &[ReplaceRule],
    context: &ReplaceContext,
    target: Target,
) -> ReplaceOutcome {
    let mut outcome = ReplaceOutcome {
        text: text.to_string(),
        ..Default::default()
    };
    if text.is_empty() || rules.is_empty() {
        return outcome;
    }

    // `order` 相同的老规则保持传入顺序：用户调顺序时看到的就是稳定行为。
    let mut ordered: Vec<&ReplaceRule> = rules.iter().collect();
    ordered.sort_by_key(|rule| rule.order);

    let mut selected: Vec<&ReplaceRule> = Vec::new();
    for rule in ordered {
        match skip_reason(rule, context, target) {
            Some(reason) => outcome.skipped.push(SkippedRule {
                id: rule.id,
                name: rule.name.clone(),
                reason,
            }),
            None => selected.push(rule),
        }
    }
    if selected.is_empty() {
        return outcome;
    }

    // 保护 `<img>` / `[newpage]` / 内联样式脚本：净化规则里 `.*` 之类的宽匹配会把
    // 插图整段吃掉。整条链只需保护一次，链尾再还原。
    let protected = protect_for_rules(text, selected.iter().map(|rule| rule.pattern.as_str()));
    let mut current = protected.content().to_string();

    for rule in selected {
        match run_rule(&current, rule) {
            Ok(next) => {
                let changed = next != current;
                current = next;
                outcome.applied.push(RuleRef {
                    id: rule.id,
                    name: rule.name.clone(),
                    changed,
                });
            }
            // 一条规则写错不该让整章变成空白：跳过它，把原因交给宿主显示。
            Err(error) => outcome.errors.push(FailedRule {
                id: rule.id,
                name: rule.name.clone(),
                error,
            }),
        }
    }

    outcome.text = protected.restore(&current);
    outcome
}

fn skip_reason(
    rule: &ReplaceRule,
    context: &ReplaceContext,
    target: Target,
) -> Option<&'static str> {
    if !rule.is_enabled {
        return Some("disabled");
    }
    if rule.pattern.trim().is_empty() {
        return Some("emptyPattern");
    }
    match target {
        Target::Title if !rule.scope_title => return Some("title"),
        Target::Content if !rule.scope_content => return Some("content"),
        _ => {}
    }
    if let Some(scope) = rule
        .scope
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        if !scope_matches(scope, context) {
            return Some("scope");
        }
    }
    if let Some(scope) = rule
        .exclude_scope
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        if scope_matches(scope, context) {
            return Some("excludeScope");
        }
    }
    None
}

/// `scope` 语义与手机端一致：`*` / 空表示全部，其余按**子串**匹配。
///
/// 另外保留桌面端写过的 `book:` / `source:` 前缀写法。
fn scope_matches(scope: &str, context: &ReplaceContext) -> bool {
    let scope = scope.trim().to_ascii_lowercase();
    if scope.is_empty() || scope == "*" {
        return true;
    }
    let book_name = context.book_name.to_ascii_lowercase();
    let book_url = context.book_url.to_ascii_lowercase();
    let source_url = context.source_url.to_ascii_lowercase();
    if let Some(rest) = scope.strip_prefix("book:") {
        let rest = rest.trim();
        return !rest.is_empty() && (book_url.contains(rest) || book_name.contains(rest));
    }
    if let Some(rest) = scope.strip_prefix("source:") {
        let rest = rest.trim();
        return !rest.is_empty() && source_url.contains(rest);
    }
    let hit = |value: &str| !value.is_empty() && value.contains(scope.as_str());
    hit(&book_name) || hit(&book_url) || hit(&source_url)
}

fn run_rule(text: &str, rule: &ReplaceRule) -> Result<String, String> {
    if let Some(code) = script_body(&rule.replacement) {
        let (pattern, flags) = js_pattern(&rule.pattern)?;
        let step = ReplaceRuleStep {
            pattern,
            flags,
            replacement: code.to_string(),
            is_script: true,
        };
        return apply_rules_with_timeout(text, &[step], rule_timeout(rule));
    }

    if !rule.is_regex {
        // 非正则就是 Java `String.replace` 的字面量替换：`$1` 在这里没有特殊含义，
        // 用 `str::replace` 才对得上。
        return Ok(text.replace(rule.pattern.as_str(), rule.replacement.as_str()));
    }

    match Regex::new(&rust_pattern(&rule.pattern)) {
        Ok(regex) => Ok(regex
            .replace_all(text, rule.replacement.as_str())
            .into_owned()),
        // Rust 正则不支持后向引用、前后向断言这些。退回 QuickJS 的 `RegExp`：
        // 净化的正则来自 Java，而 JS 的方言反而离它更近。
        Err(rust_error) => {
            let (pattern, flags) = js_pattern(&rule.pattern).map_err(|why| {
                format!("unsupported pattern ({rust_error}); the JavaScript fallback also failed: {why}")
            })?;
            let step = ReplaceRuleStep {
                pattern,
                flags,
                replacement: rule.replacement.clone(),
                is_script: false,
            };
            apply_rules_with_timeout(text, &[step], rule_timeout(rule))
        }
    }
}

fn rule_timeout(rule: &ReplaceRule) -> Duration {
    Duration::from_millis(rule.valid_timeout_ms().max(1) as u64)
}

/// `@js:` / `<js>…</js>` 开头的替换是脚本，不是字面量。
fn script_body(replacement: &str) -> Option<&str> {
    let trimmed = replacement.trim();
    if let Some(code) = trimmed.strip_prefix("@js:") {
        return Some(code);
    }
    let trimmed = trimmed.strip_prefix("<js>")?;
    Some(trimmed.strip_suffix("</js>").unwrap_or(trimmed))
}

/// Rust 正则的 `\p{…}` 不认 Java 的 `Is` 前缀（`\p{IsHan}` → `\p{Han}`）。
fn rust_pattern(pattern: &str) -> String {
    pattern.replace("\\p{Is", "\\p{")
}

/// 把 Java 正则降级成 JS 能吃的形状。
///
/// 只做**确定等价**的转换，其余返回错误让宿主显示——静默改坏一条正则比直接报错
/// 更难查。转换的内容：
///
/// - 开头的 `(?i)` / `(?m)` / `(?s)`：JS 没有内联开关，挪到 flags 上
/// - `\p{IsXxx}`：JS 要写成 `\p{Script=Xxx}`，且必须带 `u` flag
///
/// 明确不支持的：`(?x)`（忽略空白）、`(?U)`（反转贪婪）、`\A` / `\z` / `\G`、`\Q…\E`。
fn js_pattern(pattern: &str) -> Result<(String, String), String> {
    for token in ["\\A", "\\z", "\\Z", "\\G", "\\Q", "\\E"] {
        if pattern.contains(token) {
            return Err(format!(
                "`{token}` has no JavaScript equivalent; rewrite the pattern with `^` / `$`"
            ));
        }
    }
    let (flags, rest) = split_inline_flags(pattern)?;
    let mut body = String::new();
    let mut rest = rest.as_str();
    let mut unicode = false;
    while let Some(index) = rest.find("\\p{Is") {
        let start = index + "\\p{Is".len();
        let Some(end) = rest[start..].find('}') else {
            return Err("unterminated \\p{…} in the pattern".to_string());
        };
        body.push_str(&rest[..index]);
        body.push_str("\\p{Script=");
        body.push_str(&rest[start..start + end]);
        body.push('}');
        rest = &rest[start + end + 1..];
        unicode = true;
    }
    body.push_str(rest);
    let mut all = String::from("g");
    all.push_str(&flags);
    if unicode {
        all.push('u');
    }
    Ok((body, all))
}

fn split_inline_flags(pattern: &str) -> Result<(String, String), String> {
    let mut flags = String::new();
    let mut rest = pattern;
    // `break` 时不推进 `rest`：前缀不是内联开关（例如 `(?:…)`）时要把整段原样留下。
    while let Some(after) = rest.strip_prefix("(?") {
        let Some(end) = after.find(')') else { break };
        let body = &after[..end];
        if body.is_empty()
            || !body
                .chars()
                .all(|ch| matches!(ch, 'i' | 'm' | 's' | 'x' | 'U'))
        {
            break;
        }
        for ch in body.chars() {
            match ch {
                'i' | 'm' | 's' => {
                    if !flags.contains(ch) {
                        flags.push(ch);
                    }
                }
                other => {
                    return Err(format!(
                        "the inline flag `(?{other})` has no JavaScript equivalent"
                    ))
                }
            }
        }
        rest = &after[end + 1..];
    }
    Ok((flags, rest.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, replacement: &str) -> ReplaceRule {
        ReplaceRule {
            id: 1,
            name: "r".to_string(),
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            ..Default::default()
        }
    }

    fn run(text: &str, rules: &[ReplaceRule]) -> String {
        apply(text, rules, &ReplaceContext::default(), Target::Content).text
    }

    #[test]
    fn a_rule_that_matches_nothing_says_so() {
        let outcome = apply(
            "正文",
            &[rule("广告", "")],
            &ReplaceContext::default(),
            Target::Content,
        );
        assert_eq!(outcome.text, "正文");
        assert_eq!(outcome.applied.len(), 1);
        assert!(!outcome.applied[0].changed);
    }

    #[test]
    fn ads_are_removed_by_a_plain_rule() {
        assert_eq!(run("正文（本章完）", &[rule("（本章完）", "")]), "正文");
    }

    #[test]
    fn a_regex_rule_can_keep_groups() {
        let mut r = rule(r"第(\d+)章", "Chapter $1");
        r.is_regex = true;
        assert_eq!(run("第12章", &[r]), "Chapter 12");
    }

    #[test]
    fn rules_run_in_order() {
        let mut first = rule("aaa", "b");
        first.order = 1;
        let mut second = rule("b+", "c");
        second.order = 2;
        second.is_regex = true;
        assert_eq!(run("aaa aaa", &[second, first]), "c c");
    }

    #[test]
    fn a_disabled_rule_is_reported_not_applied() {
        let mut r = rule("广告", "");
        r.is_enabled = false;
        let outcome = apply(
            "广告正文",
            &[r],
            &ReplaceContext::default(),
            Target::Content,
        );
        assert_eq!(outcome.text, "广告正文");
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].reason, "disabled");
    }

    #[test]
    fn a_scope_that_does_not_match_is_skipped() {
        let mut r = rule("广告", "");
        r.scope = Some("某本书".to_string());
        let context = ReplaceContext {
            book_name: "另一本书".to_string(),
            ..Default::default()
        };
        let outcome = apply("广告", &[r], &context, Target::Content);
        assert_eq!(outcome.text, "广告");
        assert_eq!(outcome.skipped[0].reason, "scope");
    }

    #[test]
    fn a_scope_match_applies() {
        let mut r = rule("广告", "");
        r.scope = Some("某本书".to_string());
        let context = ReplaceContext {
            book_name: "某本书".to_string(),
            ..Default::default()
        };
        assert_eq!(apply("广告", &[r], &context, Target::Content).text, "");
    }

    #[test]
    fn titles_are_only_touched_when_the_rule_says_so() {
        let r = rule("第", "Chapter ");
        let outcome = apply(
            "第1章",
            std::slice::from_ref(&r),
            &ReplaceContext::default(),
            Target::Title,
        );
        assert_eq!(outcome.text, "第1章");
        assert_eq!(outcome.skipped[0].reason, "title");

        let mut titled = r;
        titled.scope_title = true;
        assert_eq!(
            apply(
                "第1章",
                &[titled],
                &ReplaceContext::default(),
                Target::Title
            )
            .text,
            "Chapter 1章"
        );
    }

    #[test]
    fn images_survive_a_rule_that_scrubs_urls() {
        // 删广告域名的规则如果不保护图片，`src` 会被挖成 `http:///x.png`，
        // 表现为“开了净化之后图片全挂了”。
        let mut r = rule(r"[a-z]+\.com", "");
        r.is_regex = true;
        let text = r#"<img src="http://ad.com/x.png">看广告 ad.com"#;
        assert_eq!(run(text, &[r]), r#"<img src="http://ad.com/x.png">看广告 "#);
    }

    #[test]
    fn an_image_is_kept_when_the_rule_targets_the_ad_text() {
        let mut r = rule(r"（本章完）", "");
        r.is_regex = true;
        let text = r#"<img src="a.png">正文（本章完）"#;
        assert_eq!(run(text, &[r]), r#"<img src="a.png">正文"#);
    }

    #[test]
    fn a_script_rule_sees_the_matched_text_as_result() {
        let mut r = rule(
            "[０-９]",
            r"@js:R = result; String.fromCharCode(R.charCodeAt(0) - 0xFEE0)",
        );
        r.is_regex = true;
        assert_eq!(run("１２", &[r]), "12");
    }

    #[test]
    fn a_lookbehind_falls_back_to_javascript() {
        let mut r = rule("(?<=【)[^】]+", "广告");
        r.is_regex = true;
        assert_eq!(run("【正文】", &[r]), "【广告】");
    }

    #[test]
    fn a_java_unicode_class_is_promoted() {
        let mut r = rule(r"\p{IsHan}+", "X");
        r.is_regex = true;
        assert_eq!(run("abc 汉字", &[r]), "abc X");
    }

    #[test]
    fn an_impossible_pattern_is_an_error_not_a_silent_no_op() {
        let mut r = rule(r"\Qa\E", "x");
        r.is_regex = true;
        let outcome = apply("aaa", &[r], &ReplaceContext::default(), Target::Content);
        assert_eq!(outcome.text, "aaa");
        assert_eq!(outcome.errors.len(), 1);
    }
}
