//! The online pipeline: fetch a page through the source's client, then run the
//! rule engine on it. Owns the HTTP client (cookies, proxy, storage), the rule
//! engine and the JavaScript runtime.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use encoding_rs::Encoding;

use crate::crawler::fetcher::{fetch_bytes_with_client, fetch_with_client, FetchResponse, RequestSpec};
use crate::crawler::http_client::HttpClient;
use crate::crawler::url_analyzer::analyze_url;
use crate::model::book::Book;
use crate::model::book_chapter::BookChapter;
use crate::model::book_source::BookSource;
use crate::model::search::SearchBook;
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
        let timeout = config.timeout_secs.unwrap_or(30).max(1);
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
        })
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
        let context = self.context(source, spec.proxy.as_deref())?;
        let response = self
            .rt
            .block_on(fetch_with_client(&context.client, spec))
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
    pub fn explore(&self, source: &BookSource, url: &str) -> Result<Vec<SearchBook>> {
        let spec = self.spec(source, url, "", 1)?;
        let response = self.fetch(source, spec)?;
        Ok(self
            .rules
            .explore_books(source, &response.body, &response.url))
    }

    /// A book's detail page. The returned book carries `tocUrl` when the
    /// source's `ruleBookInfo.tocUrl` says so.
    pub fn info(&self, source: &BookSource, book_url: &str) -> Result<Book> {
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

    /// Run a JavaScript rule against text with the source's bindings.
    pub fn eval(
        &self,
        source: &BookSource,
        script: &str,
        input: &str,
        base_url: &str,
    ) -> Result<String> {
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
        crate::parser::js::with_source(source, || {
            Ok(
                crate::parser::rule_engine::RuleEngine::eval_rule_on(rule, body, base_url, false)
                    .into_iter()
                    .find(|value| !value.trim().is_empty())
                    .unwrap_or_default(),
            )
        })
    }

    /// Download a source resource with the source's cookies.
    pub fn fetch_bytes(&self, source: &BookSource, url: &str) -> Result<(Vec<u8>, String)> {
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
        let spec = self.spec(source, url, "", 1)?;
        let context = self.context(source, spec.proxy.as_deref())?;
        let response = self
            .rt
            .block_on(fetch_bytes_with_client(&context.client, spec))
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
            return Err(anyhow!("the source sent an empty file for {}", response.url));
        }
        let charset = detect_charset(&bytes, response.content_type.as_deref());
        if let Some(parent) = dest.parent().filter(|parent| !parent.as_os_str().is_empty()) {
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

/// Sniff a downloaded file's charset without decoding it.
///
/// Order: declared charset, BOM, valid UTF-8, else GB18030. Chinese TXT
/// downloads have no `Content-Type: charset` and are usually GBK/GB18030;
/// treating them as UTF-8 lossy would replace every other character with U+FFFD.
fn detect_charset(bytes: &[u8], content_type: Option<&str>) -> String {
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
        key.trim().eq_ignore_ascii_case("charset").then(|| value.trim())
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
