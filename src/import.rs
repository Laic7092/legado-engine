//! 导入：把分享链接、URL 或内嵌内容解析成规则数组。
//!
//! 接受的形状：
//!
//! - `legado://import/bookSource?src=https://example.com/b.json`
//! - 裸 URL、`base64:…`、data URL、gzip
//! - JSON 正文
//! - 夹在上述任意一种前后的分享文案
//!
//! 输出三组**原始 JSON 对象**，宿主决定存哪些、按什么去重、是否覆盖。原样返回
//! （不经过模型重新序列化）以免丢弃新版本 Legado 才有的字段。

use std::io::Read;

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde::Serialize;
use serde_json::{Map, Value};

/// 引擎认得的三种集合。其余（字典规则、TTS、TXT 目录规则…）引擎没实现，
/// 导进来也只会是死数据，所以单独计数交给宿主提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    BookSource,
    RssSource,
    ReplaceRule,
}

/// 内容是怎么来的，宿主用来显示"从网络导入 / 从剪贴板导入"。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    /// `src` 是一个 http(s) 地址，内容由引擎下载。
    Url,
    /// 正文直接就是 JSON。
    #[default]
    Inline,
    /// base64（`base64:` 前缀、data URL 或裸串）。
    Base64,
}

/// 一条分享链接。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// 链接里声明的类别，原样保留（`bookSource` / `rssSource` / …）。
    pub kind: Option<String>,
    /// 内容所在的位置：URL、base64 或 JSON 正文。
    pub src: String,
}

/// 一次导入的结果。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    /// 链接声明的类别。它是"意图"，不保证和实际内容一致（一份合集可能同时
    /// 带书源和订阅源），所以分类永远以**内容形状**为准，这里只做参考。
    pub declared_kind: Option<String>,
    /// 下载到内容后的最终地址（跟随跳转之后），宿主存下来供"更新订阅"使用。
    pub url: Option<String>,
    pub origin: Origin,
    pub book_sources: Vec<Value>,
    pub rss_sources: Vec<Value>,
    pub replace_rules: Vec<Value>,
    /// 顶层那些"像一条规则但认不出来"的对象个数（外壳不计）。宿主应当提示
    /// "部分内容无法识别"，而不是假装全导入成功。
    pub unknown: usize,
}

impl Imported {
    fn total(&self) -> usize {
        self.book_sources.len() + self.rss_sources.len() + self.replace_rules.len()
    }
}

/// 从一段（可能是分享文案的）文本里找链接。
///
/// 只认 `legado://` 与裸的 http(s) 地址；**以 `{`/`[` 开头的文本直接判为正文**，
/// 否则 JSON 里出现的第一个 URL 会被误当成下载地址。
pub fn parse_share_text(text: &str) -> Option<Link> {
    let text = text.trim();
    if text.starts_with('{') || text.starts_with('[') {
        return None;
    }
    if let Some(link) = find_scheme_link(text) {
        return Some(link);
    }
    find_http_url(text).map(|src| Link { kind: None, src })
}

fn find_scheme_link(text: &str) -> Option<Link> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("legado://")?;
    let rest = &text[start..];
    // 整段文本以链接开头时只按空白切：`src=` 后面可能带着没转义的引号甚至整段 JSON，
    // 把 `"` 也当结束符会把内容砍在半路。链接前面还有话（分享文案）时才用完整
    // 结束符集合，否则后面的说明文字会被吞进地址。
    let at_start = start == 0;
    let end = rest
        .find(|ch: char| {
            ch.is_whitespace()
                || (!at_start && matches!(ch, '"' | '\'' | '<' | '>' | '，' | '。' | ')' | '）'))
        })
        .unwrap_or(rest.len());
    Some(parse_legado_link(&rest[..end]))
}

/// 手动拆 `legado://import/<kind>?src=…`。
///
/// 不直接丢给 `url::Url`：`src` 里的地址经常**没有**转义，`?a=1&b=2` 这种
/// 查询串会被当成链接自己的参数，地址就被截断在半路上。这里把 `src=` 之后的
/// **整段**都算地址，然后只做一次百分号解码，容错更好。
fn parse_legado_link(raw: &str) -> Link {
    let rest = &raw["legado://".len().min(raw.len())..];
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    // `legado://import/bookSource`、`legado:///import/bookSource`、`legado://bookSource`
    let kind = path
        .split('/')
        .rev()
        .find(|segment| !segment.is_empty() && !segment.eq_ignore_ascii_case("import"))
        .map(|segment| segment.trim().to_string())
        .filter(|segment| !segment.is_empty());
    let src = query
        .map(|query| {
            for key in ["src=", "url="] {
                if let Some(index) = query.to_ascii_lowercase().find(key) {
                    let raw = &query[index + key.len()..];
                    return percent_decode(raw);
                }
            }
            query.to_string()
        })
        .unwrap_or_default();
    Link { kind, src }
}

/// 只解 `%XX`。**不碰 `+`**：分享链接里 `src=` 后面常常是一段 base64，
/// 把 `+` 当空格就会解出乱码。
fn percent_decode(raw: &str) -> String {
    let raw = raw.trim();
    if !raw.contains('%') {
        return raw.to_string();
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

fn find_http_url(text: &str) -> Option<String> {
    for token in text.split(|ch: char| ch.is_whitespace() || matches!(ch, '，' | '。' | '、')) {
        let token = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '(' | '['
                    | '{'
                    | '<'
                    | '"'
                    | '\''
                    | ','
                    | '，'
                    | ')'
                    | ']'
                    | '}'
                    | '>'
                    | '。'
                    | '：'
                    | ':'
                    | '；'
                    | ';'
            )
        });
        let lower = token.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Some(token.to_string());
        }
    }
    None
}

/// 不带网络的那条路：把 `src` 里的正文解出来并分类。
pub fn import_inline(src: &str) -> Result<Imported> {
    let (body, origin) = decode_payload(src)?;
    let mut imported = split_sources(&body)?;
    imported.origin = origin;
    Ok(imported)
}

/// 分享链接里的 `src` 是不是一个需要下载的地址。
pub fn is_remote(src: &str) -> bool {
    let src = src.trim();
    src.starts_with("http://") || src.starts_with("https://")
}

///
/// 不带网络的导入：链接照样解析，内嵌内容（base64 / JSON）照样拆开，
/// 但指向网络的 `src` 会报错而不是静默变空——宿主没给 handle 时不该假装成功。
pub fn import_text(text: &str) -> Result<Imported> {
    let text = text.trim();
    if text.is_empty() {
        return Err(anyhow!("the import text is empty"));
    }
    match parse_share_text(text) {
        Some(link) if is_remote(&link.src) => Err(anyhow!(
            "this share link points at {} ; opening it needs the engine handle",
            link.src
        )),
        Some(link) => {
            let mut imported = import_inline(&link.src)?;
            imported.declared_kind = link.kind;
            Ok(imported)
        }
        None => import_inline(text),
    }
}

/// 解出正文：`base64:` / data URL / 裸 base64 / 直接就是 JSON。
pub fn decode_payload(src: &str) -> Result<(String, Origin)> {
    // 宿主从文件里读出来的正文常带 UTF-8 BOM；`\u{feff}` 不是空白，
    // `trim()` 去不掉，JSON 解析会直接报 "expected value at line 1 column 1"。
    let trimmed = src.trim().trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(anyhow!("the import link has no src"));
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Ok((trimmed.to_string(), Origin::Inline));
    }
    let encoded = trimmed
        .strip_prefix("base64:")
        .or_else(|| trimmed.strip_prefix("BASE64:"))
        .or_else(|| data_url_payload(trimmed))
        .unwrap_or(trimmed);
    let bytes = decode_base64(encoded).ok_or_else(|| {
        anyhow!(
            "the payload is neither JSON, base64 nor a URL: {}",
            preview(trimmed)
        )
    })?;
    Ok((decode_bytes(&bytes, None)?, Origin::Base64))
}

fn preview(text: &str) -> String {
    text.chars().take(40).collect()
}

/// `data:text/json;base64,…` 这种 Data URL。
fn data_url_payload(text: &str) -> Option<&str> {
    let lower = text.to_ascii_lowercase();
    if !lower.starts_with("data:") {
        return None;
    }
    let index = lower.find(";base64,")? + ";base64,".len();
    Some(&text[index..])
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.len() < 8 {
        return None;
    }
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(compact.as_bytes()) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    None
}

/// 把下载回来的字节变成文本：先看 gzip，再按 BOM/内容嗅探字符集。
///
/// 别人的合集经常是一台静态服务器直接吐出来的文件，带 `Content-Type` 但
/// 不带 charset，或者干脆是 GBK 的老站点；这里与下载走同一套嗅探规则。
pub fn decode_bytes(bytes: &[u8], content_type: Option<&str>) -> Result<String> {
    let bytes = if bytes.starts_with(&[0x1F, 0x8B]) {
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut decoded)
            .context("gunzip the import payload")?;
        decoded
    } else {
        bytes.to_vec()
    };
    if bytes.is_empty() {
        return Err(anyhow!("the import payload is empty"));
    }
    let charset = crate::pipeline::detect_charset(&bytes, content_type);
    let encoding =
        encoding_rs::Encoding::for_label(charset.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(&bytes);
    Ok(text.into_owned())
}

/// 把 JSON 正文拆成三组。分类**只看内容形状**，不看链接声明的类别。
pub fn split_sources(body: &str) -> Result<Imported> {
    let body = body.trim().trim_start_matches('\u{feff}').trim();
    // 最常见的失败不是"格式不认识"，而是 `src` 指向了网页：GitHub 的 blob 链接、
    // 源仓库的介绍页、网盘的预览页。直接说清楚怎么改，比报 serde 的行列号有用。
    if body.starts_with('<') {
        return Err(anyhow!(
            "the import payload is a web page, not JSON; use the raw file URL \
             (on GitHub that is the raw.githubusercontent.com link)"
        ));
    }
    let value: Value = serde_json::from_str(body)
        .map_err(|error| anyhow!("the import payload is not JSON: {error}"))?;
    let mut imported = Imported::default();
    collect(&value, &mut imported, 0);
    if imported.total() == 0 {
        return Err(anyhow!(
            "no book sources, RSS sources or replace rules in this payload"
        ));
    }
    Ok(imported)
}

/// 走遍整棵树。认得出的对象直接收下并**不再往下钻**，认不出的继续递归：
/// 合集常见的外壳（`{"bookSources":[…]}`、`{"data":{"list":[…]}}`）就靠这个兜住。
fn collect(value: &Value, out: &mut Imported, depth: usize) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect(item, out, depth + 1);
            }
        }
        Value::Object(map) => {
            if let Some(kind) = classify(map) {
                match kind {
                    Kind::BookSource => out.book_sources.push(value.clone()),
                    Kind::RssSource => out.rss_sources.push(value.clone()),
                    Kind::ReplaceRule => out.replace_rules.push(value.clone()),
                }
                return;
            }
            // 只数"像一条规则、却认不出来"的对象：顶层或以合集里第一层的**叶子对象**。
            // 外壳（`{"bookSources":[…]}`、`{"data":{…}}`）不算——它们只是包装，
            // 里面的东西会继续往下找，算进去会让一份完整合集也报"1 条未识别"。
            let has_nested = map
                .values()
                .any(|value| matches!(value, Value::Array(_) | Value::Object(_)));
            if depth <= 1 && !map.is_empty() && !has_nested {
                out.unknown += 1;
            }
            for inner in map.values() {
                collect(inner, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn classify(map: &Map<String, Value>) -> Option<Kind> {
    let has = |key: &str| map.contains_key(key);
    // 订阅源先认：`ruleContent` / `ruleTitle` / `ruleLink` 这几个键书源也有，
    // 反过来判断会把订阅源误收成书源。`sourceUrl` 一族是订阅源独有的。
    if has("sourceUrl")
        || has("sourceName")
        || has("ruleArticles")
        || has("ruleArticleList")
        || has("sortUrl")
    {
        return Some(Kind::RssSource);
    }
    // 书源：身份字段任取其一，再加上规则对象的键（有的老源没有 address）。
    if has("bookSourceUrl")
        || has("bookSourceName")
        || has("searchUrl")
        || has("exploreUrl")
        || has("ruleSearch")
        || has("ruleToc")
        || has("ruleBookInfo")
    {
        return Some(Kind::BookSource);
    }
    // 净化规则认「有 pattern 且有 replacement/isRegex」。`pattern` 这个键名太通用，
    // 必须带替换目标才认，否则 `{"pattern":"…"}` 这种目录规则也会被吞进来。
    if has("pattern") && (has("replacement") || has("isRegex")) {
        return Some(Kind::ReplaceRule);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_link_is_split_into_kind_and_src() {
        let link = parse_share_text(
            "今天更新啦 legado://import/rssSource?src=https://a.test/r.json 快导入",
        )
        .unwrap();
        assert_eq!(link.kind.as_deref(), Some("rssSource"));
        assert_eq!(link.src, "https://a.test/r.json");
    }

    #[test]
    fn an_unescaped_query_inside_src_survives() {
        let link = parse_share_text("legado://import/bookSource?src=https://a.test/b.json?x=1&y=2")
            .unwrap();
        assert_eq!(link.src, "https://a.test/b.json?x=1&y=2");
    }

    #[test]
    fn a_bare_link_has_no_declared_kind() {
        let link = parse_share_text("https://a.test/b.json").unwrap();
        assert_eq!(link.kind, None);
        assert_eq!(link.src, "https://a.test/b.json");
    }

    #[test]
    fn json_is_not_mistaken_for_a_link() {
        assert!(parse_share_text(r#"{"bookSourceUrl":"https://a.test"}"#).is_none());
    }

    #[test]
    fn the_kind_is_read_from_the_path() {
        assert_eq!(
            parse_legado_link("legado://import/bookSource?src=x")
                .kind
                .as_deref(),
            Some("bookSource")
        );
        assert_eq!(
            parse_legado_link("legado:///import/replaceRule?src=x")
                .kind
                .as_deref(),
            Some("replaceRule")
        );
    }

    #[test]
    fn sources_are_classified_by_shape() {
        let body = r#"[
          {"bookSourceUrl":"https://a.test","bookSourceName":"A"},
          {"sourceUrl":"https://b.test","sourceName":"Feed","ruleArticles":".item"},
          {"name":"删广告","pattern":"广告","replacement":""}
        ]"#;
        let imported = split_sources(body).unwrap();
        assert_eq!(imported.book_sources.len(), 1);
        assert_eq!(imported.rss_sources.len(), 1);
        assert_eq!(imported.replace_rules.len(), 1);
        assert_eq!(imported.unknown, 0);
    }

    #[test]
    fn a_wrapper_object_is_opened() {
        let body = r#"{"bookSources":[{"bookSourceUrl":"https://a.test"}]}"#;
        let imported = split_sources(body).unwrap();
        assert_eq!(imported.book_sources.len(), 1);
    }

    #[test]
    fn base64_payloads_decode() {
        let body = r#"[{"bookSourceUrl":"https://a.test"}]"#;
        let encoded = STANDARD.encode(body);
        let (text, origin) = decode_payload(&format!("base64:{encoded}")).unwrap();
        assert_eq!(origin, Origin::Base64);
        assert_eq!(text, body);
        // 裸 base64 也认
        let (text, _) = decode_payload(&encoded).unwrap();
        assert_eq!(text, body);
    }

    #[test]
    fn a_byte_order_mark_is_not_json_syntax() {
        // Windows 上的编辑器、以及不少分享文本都会带 BOM。
        let imported = split_sources("\u{feff}[{\"bookSourceUrl\":\"https://a.test\"}]").unwrap();
        assert_eq!(imported.book_sources.len(), 1);
        let (text, _) = decode_payload("\u{feff}[{\"bookSourceUrl\":\"https://a.test\"}]").unwrap();
        assert!(text.starts_with('['));
    }

    #[test]
    fn an_html_page_says_how_to_fix_it() {
        let error = split_sources("<!DOCTYPE html><html><body>404</body></html>").unwrap_err();
        assert!(
            error.to_string().contains("raw.githubusercontent.com"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_payload_says_so() {
        let error = split_sources(r#"[{"name":"x","rule":".a"}]"#).unwrap_err();
        assert!(error.to_string().contains("no book sources"), "{error}");
    }
}
