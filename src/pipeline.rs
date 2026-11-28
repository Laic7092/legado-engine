//! The online pipeline: fetch a page through the source's client, then run the
//! rule engine on it. Owns the HTTP client (cookies, proxy, storage), the rule
//! engine and the JavaScript runtime.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use encoding_rs::Encoding;

use crate::cancel::{self, Canceller};
use crate::crawler::fetcher::{fetch_bytes_with_client, FetchResponse, RequestSpec};
use crate::crawler::http_client::HttpClient;
use crate::crawler::url_analyzer::analyze_url;
use crate::model::book::Book;
use crate::model::book_chapter::BookChapter;
use crate::model::book_source::BookSource;
use crate::model::rss::{RssArticle, RssSource};
use crate::model::search::SearchBook;
use crate::parser::js::with_source;
use crate::parser::rule_engine::RuleEngine;

/// How to open the engine. `storage_dir` holds source variables and cookie
/// jars; without it they stay in memory.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub storage_dir: Option<PathBuf>,
    pub proxy: Option<String>,
    pub timeout_secs: Option<u64>,
    /// `user_ns` scopes the per-source cookie/session files.
    pub user_ns: Option<String>,
    /// Ignore the system proxy.
    pub direct: bool,
}

pub struct Engine {
    http: HttpClient,
    rt: tokio::runtime::Runtime,
    rules: RuleEngine,
    user_ns: String,
    direct: bool,
    cancel: Canceller,
}

/// One page of an RSS feed: the articles plus the next list page, if the
/// source's `ruleNextPage` answered one.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RssPage {
    pub articles: Vec<RssArticle>,
    pub next_url: String,
}

/// A file downloaded through a source's client: the bytes land in `path`, and
/// `charset` says how to read them back (`utf-8`, `gb18030`, …).
///
/// Download-type sources (知轩藏书, 轻小说文库的 TXT) have no chapters to speak
/// of: the useful artefact is the file itself.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    pub path: String,
    pub size: u64,
    pub content_type: Option<String>,
    pub charset: String,
    /// The final URL after redirects.
    pub url: String,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self> {
        let timeout = config
            .timeout_secs
            .unwrap_or(crate::crawler::http_client::DEFAULT_TIMEOUT_SECS)
            .max(1);
        let http = HttpClient::new(timeout, config.proxy.clone())?;
        let user_ns = config
            .user_ns
            .clone()
            .unwrap_or_else(|| "legado-engine".to_string());
        http.set_user_ns(&user_ns);
        if let Some(dir) = config.storage_dir.as_ref() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create storage dir {}", dir.display()))?;
            http.set_source_storage_dir(dir.clone());
            crate::parser::js::set_js_storage_dir(dir.clone());
        }
        crate::parser::js::set_js_http_client(http.clone());

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context("build the engine runtime")?;

        Ok(Self {
            http,
            rt,
            rules: RuleEngine::new()?,
            user_ns,
            direct: config.direct,
            cancel: Canceller::new(),
        })
    }

    /// A handle that can cancel this engine's calls from another thread.
    ///
    /// Typical use: the UI thread keeps the `Canceller`, a worker thread runs
    /// [`Engine::search`], and pressing Back cancels the search. Cancelling
    /// affects the calls in flight; later calls run normally.
    pub fn canceller(&self) -> Canceller {
        self.cancel.clone()
    }

    /// Cancel every call this engine is running right now.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Attach this engine's cancel token to the calling thread for the
    /// duration of one call, so the HTTP layer, the rate limiter, the
    /// downloader and the JavaScript sandbox all see it.
    fn attach(&self) -> cancel::Scope {
        cancel::scope(self.cancel.token())
    }

    /// Fetch a URL with the source's client: its headers, cookies and proxy.
    /// Shared by the source's pages and `java.ajax`.
    fn fetch(&self, source: &BookSource, spec: RequestSpec) -> Result<FetchResponse> {
        if spec.web_view {
            return Err(anyhow!(
                "this URL needs the platform's browser (webView): {}",
                spec.url
            ));
        }
        cancel::check()?;
        let context = self.context(source, spec.proxy.as_deref())?;
        // 走 source session 而不是裸 client：书源的 `concurrentRate` 在这里生效，
        // 与 `java.ajax` 共用同一个限速窗口。
        let response = self
            .rt
            .block_on(context.session.request(
                &context.client,
                spec,
                source.concurrent_rate.as_deref(),
            ))
            .context("request the source")?;
        if response.status >= 400 {
            return Err(anyhow!(
                "the source answered {} for {}",
                response.status,
                response.url
            ));
        }
        Ok(response)
    }

    fn spec(&self, source: &BookSource, url: &str, key: &str, page: i32) -> Result<RequestSpec> {
        analyze_url(url, key, page, &source.book_source_url, source)
            .map_err(|error| anyhow!("{error}"))
    }

    /// One page of search results.
    pub fn search(&self, source: &BookSource, key: &str, page: i32) -> Result<Vec<SearchBook>> {
        let _cancel = self.attach();
        let url = source
            .search_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| anyhow!("this source has no searchUrl"))?;
        let spec = self.spec(source, url, key, page)?;
        let response = self.fetch(source, spec)?;
        Ok(self
            .rules
            .search_books(source, &response.body, &response.url))
    }

    /// One page of an explore category.
    ///
    /// `key` and `page` feed the `{{key}}` / `{{page}}` placeholders in the
    /// category URL. They used to be hardcoded to `""` and `1`, which meant a
    /// source whose explore URL is `/list?page={{page}}` stayed on page one
    /// forever — "发现页翻不了页".
    pub fn explore_page(
        &self,
        source: &BookSource,
        url: &str,
        key: &str,
        page: i32,
    ) -> Result<Vec<SearchBook>> {
        let _cancel = self.attach();
        let spec = self.spec(source, url, key, page.max(1))?;
        let response = self.fetch(source, spec)?;
        Ok(self
            .rules
            .explore_books(source, &response.body, &response.url))
    }

    /// First page of an explore category (no `{{key}}`).
    pub fn explore(&self, source: &BookSource, url: &str) -> Result<Vec<SearchBook>> {
        self.explore_page(source, url, "", 1)
    }

    /// A book's detail page. The returned book carries `tocUrl` when the
    /// source's `ruleBookInfo.tocUrl` says so.
    pub fn info(&self, source: &BookSource, book_url: &str) -> Result<Book> {
        let _cancel = self.attach();
        let spec = self.spec(source, book_url, "", 1)?;
        let response = self.fetch(source, spec)?;
        let mut book = self
            .rules
            .book_info(source, &response.body, &response.url, book_url);
        if book.book_url.trim().is_empty() {
            book.book_url = book_url.to_string();
        }
        if book.origin.trim().is_empty() {
            book.origin = source.book_source_url.clone();
        }
        if book.origin_name.is_none() {
            book.origin_name = Some(source.book_source_name.clone());
        }
        Ok(book)
    }

    /// All chapters. Follows the `nextTocUrl` pages the rule engine answers,
    /// with a cap so a rule that always says "next" cannot run forever.
    pub fn toc(&self, source: &BookSource, book: &Book) -> Result<Vec<BookChapter>> {
        let _cancel = self.attach();
        let mut next = book
            .toc_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or(book.book_url.trim())
            .to_string();
        if next.is_empty() {
            return Err(anyhow!("the book has no tocUrl or bookUrl"));
        }
        let mut chapters: Vec<BookChapter> = Vec::new();
        let mut seen_pages: HashSet<String> = HashSet::new();
        let mut seen_urls: HashSet<String> = HashSet::new();
        while !next.is_empty() && seen_pages.len() < 40 {
            if !seen_pages.insert(next.clone()) {
                break;
            }
            let spec = self.spec(source, &next, "", 1)?;
            let response = self.fetch(source, spec)?;
            let (mut page, next_urls) = self.rules.chapter_list(
                source,
                &response.body,
                &response.url,
                Some(&book.book_url),
            );
            for chapter in page.drain(..) {
                if chapter.url.trim().is_empty() || !seen_urls.insert(chapter.url.clone()) {
                    continue;
                }
                chapters.push(chapter);
            }
            next = next_urls
                .into_iter()
                .map(|url| url.trim().to_string())
                .find(|url| !url.is_empty())
                .unwrap_or_default();
        }
        for (index, chapter) in chapters.iter_mut().enumerate() {
            chapter.index = index as i32;
        }
        if chapters.is_empty() {
            return Err(anyhow!("the toc rule found no chapters"));
        }
        Ok(chapters)
    }

    /// One chapter's text, following `nextContentUrl` pages. The source's
    /// `replaceRegex` is applied by the rule engine on each page.
    pub fn content(
        &self,
        source: &BookSource,
        book: &Book,
        chapter: &BookChapter,
    ) -> Result<String> {
        let _cancel = self.attach();
        let mut next = chapter.url.trim().to_string();
        if next.is_empty() {
            return Err(anyhow!("the chapter has no url"));
        }
        let mut parts: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        while !next.is_empty() && seen.len() < 10 {
            if !seen.insert(next.clone()) {
                break;
            }
            let spec = self.spec(source, &next, "", 1)?;
            let response = self.fetch(source, spec)?;
            let text =
                self.rules
                    .content(source, &response.body, &response.url, Some(&book.book_url));
            let text = text.trim();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
            next = self
                .rules
                .next_content_url(source, &response.body, &response.url)
                .unwrap_or_default();
        }
        if parts.is_empty() {
            return Err(anyhow!("the content rule found no text"));
        }
        Ok(parts.join("\n"))
    }

    /// One page of an RSS feed's article list.
    ///
    /// `sort_url` is one of the entries in the source's `sortUrl` (the host parses
    /// that JSON itself, like `exploreUrl`); empty means the source's own
    /// `sourceUrl`. The answer carries `nextUrl` for the next list page, so a
    /// host that auto-loads more pages keeps the pagination rule on this side.
    pub fn rss_articles(&self, source: &RssSource, sort_url: &str) -> Result<RssPage> {
        let _cancel = self.attach();
        let page_url = {
            let sort_url = sort_url.trim();
            if sort_url.is_empty() {
                source.home_url()
            } else {
                sort_url
            }
        };
        if page_url.is_empty() {
            return Err(anyhow!("this RSS source has no sourceUrl"));
        }
        let book_source = rss_book_source(source);
        let spec = self.spec(&book_source, page_url, "", 1)?;
        let response = self.fetch(&book_source, spec)?;
        let base = response.url.clone();
        let list_rule = source.rule_articles.as_deref().unwrap_or_default().trim();
        // 未写 `ruleArticles`（或 `singleUrl`）时，这个地址本身就是一篇文章。
        let single = source.single_url == Some(true) || list_rule.is_empty();

        let (articles, next_url) = with_source(&book_source, || {
            let articles: Vec<RssArticle> = if single {
                vec![rss_article(source, &response.body, &base, sort_url, 0)]
            } else {
                RuleEngine::eval_rule_elements(list_rule, &response.body)
                    .iter()
                    .enumerate()
                    .map(|(index, element)| {
                        rss_article(source, &element_body(element), &base, sort_url, index)
                    })
                    .collect()
            };
            let next = first_value(
                source.rule_next_page.as_deref().unwrap_or_default(),
                &response.body,
                &base,
            );
            let next_url = if next.is_empty() {
                String::new()
            } else {
                let next_url = crate::parser::rule_engine::resolve_url(&base, &next);
                // 返回当前页等于让宿主一直翻同一页：这种规则是坏的，直接当没有下一页，
                // 免得宿主的"自动加载"变成一个死循环。
                if next_url == base {
                    String::new()
                } else {
                    next_url
                }
            };
            (articles, next_url)
        });

        if articles.is_empty() {
            return Err(anyhow!("the ruleArticles rule found no articles in {base}"));
        }
        Ok(RssPage { articles, next_url })
    }

    /// One article's body, following `ruleNextPage` across pages.
    ///
    /// Like `content`, the HTML is returned as the source wrote it (the host
    /// renders it); user replace rules are a separate step, see
    /// `crate::parser::replace`.
    pub fn rss_content(&self, source: &RssSource, article: &RssArticle) -> Result<String> {
        let _cancel = self.attach();
        let rule = source.rule_content.as_deref().unwrap_or_default().trim();
        if rule.is_empty() {
            return Err(anyhow!("this RSS source has no ruleContent"));
        }
        let mut next = if article.link.trim().is_empty() {
            source.home_url().to_string()
        } else {
            article.link.trim().to_string()
        };
        if next.is_empty() {
            return Err(anyhow!("the article has no link"));
        }
        let book_source = rss_book_source(source);
        let next_rule = source.rule_next_page.as_deref().unwrap_or_default().trim();
        let mut parts: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        while !next.is_empty() && seen.len() < 10 {
            if !seen.insert(next.clone()) {
                break;
            }
            let spec = self.spec(&book_source, &next, "", 1)?;
            let response = self.fetch(&book_source, spec)?;
            let base = response.url.clone();
            let text = with_source(&book_source, || {
                RuleEngine::eval_rule_on(rule, &response.body, &base, false)
                    .into_iter()
                    .find(|value| !value.trim().is_empty())
                    .unwrap_or_default()
            });
            let text = text.trim();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
            let next_page = with_source(&book_source, || {
                first_value(next_rule, &response.body, &base)
            });
            next = if next_page.is_empty() {
                String::new()
            } else {
                crate::parser::rule_engine::resolve_url(&base, &next_page)
            };
        }
        if parts.is_empty() {
            return Err(anyhow!("the ruleContent rule found no text"));
        }
        Ok(parts.join("\n"))
    }

    /// Run a JavaScript rule against text with the source's bindings.
    pub fn eval(
        &self,
        source: &BookSource,
        script: &str,
        input: &str,
        base_url: &str,
    ) -> Result<String> {
        let _cancel = self.attach();
        crate::parser::js::with_source(source, || {
            crate::parser::js::eval_js_with_bindings(
                script,
                input,
                base_url,
                &std::collections::HashMap::new(),
            )
        })
    }

    /// Run a full rule (CSS / XPath / JSONPath / JS) against text.
    pub fn eval_rule(
        &self,
        source: &BookSource,
        rule: &str,
        body: &str,
        base_url: &str,
    ) -> Result<String> {
        let _cancel = self.attach();
        crate::parser::js::with_source(source, || {
            Ok(
                crate::parser::rule_engine::RuleEngine::eval_rule_on(rule, body, base_url, false)
                    .into_iter()
                    .find(|value| !value.trim().is_empty())
                    .unwrap_or_default(),
            )
        })
    }

    /// Fetch a URL outside any source. Import links point at raw JSON on GitHub,
    /// 源仓库s and pastebins, so they get the engine's client (UA, proxy,
    /// the same TLS behaviour as a source request) but no source cookies.
    pub fn fetch_any(&self, url: &str) -> Result<(Vec<u8>, String, Option<String>)> {
        let _cancel = self.attach();
        let source = BookSource {
            book_source_url: "import".to_string(),
            ..Default::default()
        };
        let spec = RequestSpec {
            url: url.to_string(),
            ..Default::default()
        };
        let context = self.context(&source, None)?;
        let response = self
            .rt
            .block_on(fetch_bytes_with_client(&context.client, spec))
            .with_context(|| format!("download {url}"))?;
        if response.status >= 400 {
            return Err(anyhow!(
                "the server answered {} for {}",
                response.status,
                response.url
            ));
        }
        if response.raw_body.is_empty() {
            return Err(anyhow!("{} answered with an empty body", response.url));
        }
        Ok((response.raw_body, response.url, response.content_type))
    }

    /// Import sources, RSS sources and replace rules from whatever the user has:
    /// a `legado://import/…` share link, a URL, base64 or the JSON itself.
    ///
    /// The engine deliberately owns this and not the host: the link grammar,
    /// the base64 and gzip handling and the charset sniffing are the same on
    /// every platform, and getting one of them wrong looks like "this source is
    /// broken" to the user.
    pub fn import(&self, text: &str) -> Result<crate::import::Imported> {
        let text = text.trim();
        if text.is_empty() {
            return Err(anyhow!("the import text is empty"));
        }
        let Some(link) = crate::import::parse_share_text(text) else {
            return crate::import::import_text(text);
        };
        if !crate::import::is_remote(&link.src) {
            return crate::import::import_text(text);
        }
        let (bytes, url, content_type) = self.fetch_any(&link.src)?;
        let body = crate::import::decode_bytes(&bytes, content_type.as_deref())?;
        let mut imported = crate::import::split_sources(&body)?;
        imported.declared_kind = link.kind;
        imported.origin = crate::import::Origin::Url;
        imported.url = Some(url);
        Ok(imported)
    }

    /// Download a source resource with the source's cookies.
    pub fn fetch_bytes(&self, source: &BookSource, url: &str) -> Result<(Vec<u8>, String)> {
        let _cancel = self.attach();
        let spec = self.spec(source, url, "", 1)?;
        let response = self.fetch(source, spec)?;
        let body = if response.raw_body.is_empty() {
            response.body.into_bytes()
        } else {
            response.raw_body
        };
        Ok((body, response.url))
    }

    /// Download a file and write it to `dest`.
    ///
    /// Uses the source's headers, cookies and proxy, so a Cloudflare-fronted
    /// file host sees the same client as the pages that linked to it. The
    /// charset is sniffed from the response (declared charset, BOM, valid
    /// UTF-8, else GB18030) instead of trusting a decoded string, because the
    /// download itself must stay byte-exact.
    pub fn download(&self, source: &BookSource, url: &str, dest: &Path) -> Result<Download> {
        let _cancel = self.attach();
        let mut spec = self.spec(source, url, "", 1)?;
        // 下载用另一套超时：页面那 15 秒是为了"别让用户干等"，下载是"别把书切成半本"。
        spec.timeout = Some(std::time::Duration::from_secs(
            crate::crawler::http_client::DOWNLOAD_TIMEOUT_SECS,
        ));
        let context = self.context(source, spec.proxy.as_deref())?;
        let response = self
            .rt
            .block_on(context.session.request_raw(
                &context.client,
                spec,
                source.concurrent_rate.as_deref(),
            ))
            .context("download the source file")?;
        if response.status >= 400 {
            return Err(anyhow!(
                "the source answered {} for {}",
                response.status,
                response.url
            ));
        }
        let bytes = response.raw_body;
        if bytes.is_empty() {
            return Err(anyhow!(
                "the source sent an empty file for {}",
                response.url
            ));
        }
        // 取消时不写文件：宁可没有文件，也不留半本。
        cancel::check()?;
        let charset = detect_charset(&bytes, response.content_type.as_deref());
        if let Some(parent) = dest
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        write_atomically(dest, &bytes)?;
        Ok(Download {
            path: dest.to_string_lossy().into_owned(),
            size: bytes.len() as u64,
            content_type: response.content_type,
            charset,
            url: response.url,
        })
    }

    /// The source's `Cookie` header for a URL.
    pub fn cookie_header(&self, source: &BookSource, url: &str) -> Result<String> {
        let url = url::Url::parse(url).context("parse the cookie url")?;
        let context = self.context(source, None)?;
        Ok(context.session.cookies.get(&url))
    }

    /// Inject a `Cookie` header into the source's jar, for example the cookies
    /// a platform WebView got when the user signed in.
    pub fn set_cookie(&self, source: &BookSource, url: &str, cookie: &str) -> Result<()> {
        let url = url::Url::parse(url).context("parse the cookie url")?;
        let context = self.context(source, None)?;
        context.session.cookies.set(&url, cookie)
    }

    fn context(
        &self,
        source: &BookSource,
        proxy: Option<&str>,
    ) -> Result<crate::crawler::source_session::SourceHttpContext> {
        if self.direct {
            self.http
                .source_context_direct(&self.user_ns, &source.book_source_url)
        } else {
            self.http
                .source_context(&self.user_ns, &source.book_source_url, proxy)
        }
    }
}

/// 订阅源借用书源的一部分上下文。
///
/// 请求头、cookie jar、`loadWithBaseUrl` 以及 `@js:` 规则里的 `source.*`
/// 绑定都挂在 `BookSource` 上，而订阅源字段只是书源的一子集。
/// 造一个镜像对象比把 `RuleEngine` / `with_source` / `analyze_url`
/// 三层全部泛型化一遍安全得多。
fn rss_book_source(source: &RssSource) -> BookSource {
    BookSource {
        book_source_name: source.source_name.clone(),
        book_source_group: source.source_group.clone(),
        book_source_url: source.source_url.clone(),
        book_source_comment: source.source_comment.clone(),
        enabled: source.enabled,
        enabled_cookie_jar: source.enabled_cookie_jar,
        concurrent_rate: source.concurrent_rate.clone(),
        header: source.header.clone(),
        login_url: source.login_url.clone(),
        login_check_js: source.login_check_js.clone(),
        last_update_time: source.last_update_time,
        custom_order: source.custom_order,
        load_with_base_url: source.load_with_base_url,
        single_url: source.single_url,
        ..Default::default()
    }
}

/// 列表元素要反复当作子页面去求值字段，所以取它的 HTML。
///
/// `eval_rule_elements` 对 CSS/XPath 返回元素快照（带 `outerHtml`），对
/// JSONPath 直接返回原始节点，两种都要能当 body 用。
fn element_body(value: &serde_json::Value) -> String {
    if let Some(html) = value.get("outerHtml").and_then(serde_json::Value::as_str) {
        return html.to_string();
    }
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 一条规则取第一个非空值（管道里的 RSS 字段都是“有就取、没有就空”）。
fn first_value(rule: &str, body: &str, base_url: &str) -> String {
    if rule.trim().is_empty() {
        return String::new();
    }
    RuleEngine::eval_rule_on(rule, body, base_url, false)
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 一条文章的字段。没有对应规则的字段留空，宿主不必区分 null 与空数组。
fn rss_article(
    source: &RssSource,
    body: &str,
    base_url: &str,
    sort: &str,
    order: usize,
) -> RssArticle {
    let field = |rule: Option<&str>| first_value(rule.unwrap_or_default(), body, base_url);
    let title = field(source.rule_title.as_deref());
    let link = {
        let raw = field(source.rule_link.as_deref());
        if raw.is_empty() {
            base_url.to_string()
        } else {
            crate::parser::rule_engine::resolve_url(base_url, &raw)
        }
    };
    let image = {
        let raw = field(source.rule_image.as_deref());
        if raw.is_empty() {
            None
        } else {
            Some(crate::parser::rule_engine::resolve_url(base_url, &raw))
        }
    };
    let text_or_none = |value: String| if value.is_empty() { None } else { Some(value) };
    RssArticle {
        origin: source.source_url.clone(),
        sort: sort.trim().to_string(),
        title,
        order: order as i64,
        link,
        pub_date: text_or_none(field(source.rule_pub_date.as_deref())),
        description: text_or_none(field(source.rule_description.as_deref())),
        content: None,
        image,
        read: None,
        variable: None,
    }
}

/// Sniff a downloaded file's charset without decoding it.
///
/// Order: declared charset, BOM, valid UTF-8, else GB18030. Chinese TXT
/// downloads have no `Content-Type: charset` and are usually GBK/GB18030;
/// treating them as UTF-8 lossy would replace every other character with U+FFFD.
pub(crate) fn detect_charset(bytes: &[u8], content_type: Option<&str>) -> String {
    if let Some(label) = content_type.and_then(declared_charset) {
        if let Some(encoding) = Encoding::for_label(label.trim().as_bytes()) {
            return encoding.name().to_ascii_lowercase();
        }
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return "utf-8".to_string();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return "utf-16le".to_string();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return "utf-16be".to_string();
    }
    if std::str::from_utf8(bytes).is_ok() {
        return "utf-8".to_string();
    }
    "gb18030".to_string()
}

fn declared_charset(content_type: &str) -> Option<&str> {
    content_type.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim())
    })
}

/// Write through a sibling `.part` file so a failed download never leaves a
/// truncated file where the host expects a complete one.
fn write_atomically(dest: &Path, bytes: &[u8]) -> Result<()> {
    let mut temp = dest.as_os_str().to_os_string();
    temp.push(".part");
    let temp = PathBuf::from(temp);
    if let Err(error) = std::fs::write(&temp, bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(error).with_context(|| format!("write {}", temp.display()));
    }
    std::fs::rename(&temp, dest).with_context(|| {
        let _ = std::fs::remove_file(&temp);
        format!("move {} to {}", temp.display(), dest.display())
    })
}
