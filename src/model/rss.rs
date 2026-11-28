use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RssSource {
    pub source_url: String,
    pub source_name: String,
    pub source_icon: Option<String>,
    pub source_group: Option<String>,
    pub source_comment: Option<String>,
    pub enabled: Option<bool>,
    pub enabled_cookie_jar: Option<bool>,
    pub concurrent_rate: Option<String>,
    pub header: Option<String>,
    pub login_url: Option<String>,
    pub login_check_js: Option<String>,
    pub sort_url: Option<String>,
    pub single_url: Option<bool>,
    pub article_style: Option<i32>,
    pub rule_articles: Option<String>,
    pub rule_next_page: Option<String>,
    pub rule_title: Option<String>,
    pub rule_pub_date: Option<String>,
    pub rule_description: Option<String>,
    pub rule_image: Option<String>,
    pub rule_link: Option<String>,
    pub rule_content: Option<String>,
    pub style: Option<String>,
    pub enable_js: Option<bool>,
    pub load_with_base_url: Option<bool>,
    pub custom_order: Option<i32>,
    pub last_update_time: Option<i64>,
}

impl RssSource {
    pub fn is_enabled(&self) -> bool {
        self.enabled != Some(false)
    }

    /// 订阅源地址；`singleUrl` 为 true 时它本身就是一篇文章。
    pub fn home_url(&self) -> &str {
        self.source_url.trim()
    }

    /// `sortUrl` 里的分类列表。字段是一个 JSON 字符串（`[{"title":…,"url":…}]`），
    /// 与 `exploreUrl` 一样由调用方解析。
    pub fn sort_kinds(&self) -> Vec<Value> {
        let raw = self.sort_url.as_deref().unwrap_or_default().trim();
        if raw.is_empty() {
            return Vec::new();
        }
        serde_json::from_str::<Value>(raw)
            .ok()
            .map(|value| match value {
                Value::Array(items) => items,
                other => vec![other],
            })
            .unwrap_or_default()
    }
}

/// 从任意 JSON 值还原订阅源。
///
/// 只做字段改名，不做结构迁移：订阅源从 2.x 到 3.0 只改过几个键名，而且
/// 老存档（`rssSource.json`）里 `enable` 与 `enabled` 两种写法都出现过。
pub fn rss_source_from_value(mut value: Value) -> serde_json::Result<RssSource> {
    if let Some(obj) = value.as_object_mut() {
        for (old, new) in [
            ("enable", "enabled"),
            ("ruleArticleList", "ruleArticles"),
            ("ruleArticleDate", "rulePubDate"),
            ("ruleArticleTitle", "ruleTitle"),
            ("ruleArticleLink", "ruleLink"),
            ("ruleArticleContent", "ruleContent"),
        ] {
            if !obj.contains_key(new) {
                if let Some(raw) = obj.remove(old) {
                    obj.insert(new.to_string(), raw);
                }
            }
        }
    }
    serde_json::from_value(value)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RssArticle {
    pub origin: String,
    pub sort: String,
    pub title: String,
    pub order: i64,
    pub link: String,
    pub pub_date: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub image: Option<String>,
    pub read: Option<bool>,
    pub variable: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_keys_are_migrated() {
        let source = rss_source_from_value(json!({
            "sourceName": "旧订阅",
            "sourceUrl": "https://rss.test/feed",
            "enable": false,
            "ruleArticleList": "div.item",
            "ruleArticleTitle": "a@text",
            "ruleArticleLink": "a@href",
            "ruleArticleDate": "time@text",
            "ruleArticleContent": "#content@html"
        }))
        .unwrap();
        assert!(!source.is_enabled());
        assert_eq!(source.rule_articles.as_deref(), Some("div.item"));
        assert_eq!(source.rule_title.as_deref(), Some("a@text"));
        assert_eq!(source.rule_link.as_deref(), Some("a@href"));
        assert_eq!(source.rule_pub_date.as_deref(), Some("time@text"));
        assert_eq!(source.rule_content.as_deref(), Some("#content@html"));
    }

    #[test]
    fn modern_keys_win_over_legacy_ones() {
        let source = rss_source_from_value(json!({
            "sourceUrl": "https://rss.test/feed",
            "enabled": true,
            "enable": false,
            "ruleArticles": "new",
            "ruleArticleList": "old"
        }))
        .unwrap();
        assert!(source.is_enabled());
        assert_eq!(source.rule_articles.as_deref(), Some("new"));
    }

    #[test]
    fn sort_kinds_reads_the_category_json() {
        let source = rss_source_from_value(json!({
            "sourceUrl": "https://rss.test/feed",
            "sortUrl": "[{\"title\":\"全部\",\"url\":\"/all\"}]"
        }))
        .unwrap();
        assert_eq!(source.sort_kinds().len(), 1);
        assert_eq!(source.sort_kinds()[0]["title"], "全部");
    }
}
