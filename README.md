# legado-engine

Legado（阅读 3.0）书源规范的 Rust 实现，带 C ABI。

给定书源 JSON 与 URL，引擎发请求、跑规则、执行 JS，返回结果。它不含书架、
阅读界面或数据库——那些属于宿主。

```rust
use legado_engine::model::book_source::BookSource;
use legado_engine::pipeline::{Config, Engine};

let engine = Engine::new(Config::default())?;
let source: BookSource = serde_json::from_str(&json)?;
let books = engine.search(&source, "剑来", 1)?;
let book = engine.info(&source, &books[0].book_url)?;
let toc = engine.toc(&source, &book)?;
let text = engine.content(&source, &book, &toc[0])?;   // 原文，未净化
```

## 功能

- 规则：CSS、XPath、JSONPath、正则、模板，`&&` / `||` / `%%` 连接，`##` 替换
- JavaScript：`@js:`、`<js>`、`{{…}}`，QuickJS 运行时；`java.*`、`source.*`、
  `book.*`、`chapter.*`、`cookie.*`、`cache.*`、`org.jsoup.*`
- HTTP：每源独立 client、cookie jar、变量存储、代理；超时、重试、
  `concurrentRate` 限速、`Retry-After`
- 取消：任意线程可叫停进行中的调用
- 导入：分享链接、URL、base64、gzip、裸 JSON，可混合书源 / 订阅源 / 净化规则
- 净化：替换净化（`order` / `scope`），`@js:` 规则在沙盒执行
- 订阅源：文章列表、`ruleNextPage` 分页、单篇正文
- 下载：以书源的 header / cookie / 代理原样落盘

## 构建与测试

```sh
cargo build --release --lib    # target/release/liblegado_engine.a
cargo rustc --release --lib -- --print native-static-libs   # 需要的系统库

make lint     # cargo fmt --check + cargo clippy -D warnings
make test     # 单元测试 + 本地 HTTP 全流程测试
make test-c   # 用 C 程序调用一遍 C ABI
```

## C ABI

声明见 [`legado.h`](legado.h)；接入细节见 [`docs/integration.md`](docs/integration.md)。

| 类别 | 函数 |
| --- | --- |
| 生命周期 | `legado_open`、`legado_close`、`legado_cancel`、`legado_version` |
| 书源 | `legado_search`、`legado_explore`、`legado_explore_page`、`legado_info`、`legado_toc`、`legado_content` |
| 订阅源 | `legado_rss_sources`、`legado_rss_articles`、`legado_rss_content` |
| 导入 / 净化 | `legado_import`、`legado_replace` |
| 下载 / 会话 | `legado_download`、`legado_cookie_header`、`legado_set_cookie` |
| 调试 / 元数据 | `legado_eval`、`legado_eval_rule`、`legado_sources`、`legado_last_error` |

约定：

- 返回的 `const char *` 是 UTF-8，归引擎所有，在下一次同线程调用前有效。
- 失败返回 `NULL`，原因见 `legado_last_error(engine)`（`legado_open` 失败时传 `NULL`）。
- 被取消的调用返回 `NULL`，`legado_last_error` 恰为 `LEGADO_CANCELLED`。

## 配置

`legado_open(NULL)` 或：

```json
{"storageDir": "/path/to/data", "proxy": null, "timeoutSecs": 15, "userNs": "app", "direct": false}
```

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `storageDir` | 无（仅内存） | cookie jar、`source.getVariable()` 变量、`java.cacheFile` |
| `userNs` | `legado-engine` | 存储文件的命名空间 |
| `proxy` | 系统代理 | `http://` / `https://` |
| `direct` | `false` | 忽略系统代理 |
| `timeoutSecs` | 15 | 单个请求的总超时 |

超时详情：建连上限 10 秒；读超时 60 秒仅对下载有效；`legado_download` 固定
30 分钟总超时，连接停滞 60 秒即判定断开。

## 线程与取消

同一 handle 可多线程并发调用，调用方无需加锁（JS 上下文、当前书源、
请求上下文为 thread-local；cookie / HTTP / JS 存储加锁）。返回值与错误按
调用线程各存一份。搜索、目录、正文会发 HTTP 并执行 JS，勿放 UI 线程。

`legado_cancel(engine)`（Rust：`engine.cancel()` / `engine.canceller()`）叫停
该 handle 上正在跑的调用：

- 只影响进行中的调用，之后的调用照常，无需重置。
- 检查点：HTTP 发送、读 body、重试退避、`concurrentRate` 排队、书源脚本、
  下载落盘前。取消不写文件。
- 一个 handle 上并发多个调用时全部叫停。需要按任务隔离时用多个 handle。

## 已知边界

- HTTP 走 reqwest，不经宿主网络栈。系统代理支持 Windows 注册表、macOS
  `networksetup`、GNOME `gsettings`，其余平台读 `HTTPS_PROXY` 等环境变量。
- TLS 不校验证书与主机名（与 Legado 一致）。
- `java.startBrowser*` / `webView` 未实现。登录态用 `legado_set_cookie` 注入，
  或在 `jsLib` 里用 `java.post` 登录；二者共用同一 cookie jar。
- 下载型书源（`bookSourceType` 3）无章节：`legado_info` 的 `downloadUrls` 每行
  `名称::URL`，交由 `legado_download`；`charset` 按响应头、BOM、UTF-8、GB18030
  依次嗅探。文本型源（类型 0）可同时带 `downloadUrls`。
- `exploreUrl` 分类 JSON 由宿主解析。翻页用 `legado_explore_page`（填 `{{key}}`
  / `{{page}}`）；`legado_explore` 等价于 `key=""`、`page=1`。`sortUrl` 同理。
- 净化为独立步骤：`legado_content` / `legado_rss_content` 返回原文，宿主用
  `legado_replace` 施加规则；缓存存原文。
- 净化 `pattern` 按 Java 正则处理：Rust 正则走快路径，后向引用 / 前后向断言
  退回 QuickJS，`\Q…\E`、`(?x)` 报错并保持原样。被 `scope` 过滤的规则进 `skipped`。
- 导入支持 JSON / base64 / gzip，不支持 zip；`src` 指向网页时提示改用 raw 地址。
- 订阅源分页仅走 `ruleNextPage`，`sourceUrl` 中的 `{{page}}` 不自增。
- 净化脚本沙盒只有 `result`，无 `java.*` / `source.*`。
- 书源脚本硬上限 120 秒（兜死循环）。
- 仅在 Linux 上本地验证，其他目标由 CI 构建。

## 目录

| 路径 | 内容 |
| --- | --- |
| `src/model/` | 书源、订阅源、书、章节、规则、净化规则 |
| `src/parser/` | 规则引擎、JS 运行时、Java 兼容层、净化 |
| `src/crawler/` | HTTP、cookie jar、URL 规则、限速 |
| `src/cancel.rs` | 取消令牌与作用域 |
| `src/import.rs` | 分享链接 / base64 / URL → 规则数组 |
| `src/pipeline.rs` | 搜索、详情、目录、正文、下载、订阅源、导入 |
| `src/abi.rs` | C ABI |
| `src/service/` | 浏览器登录接口（未实现） |
