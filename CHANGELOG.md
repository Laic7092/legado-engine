# Changelog

## 0.4.0

新增的 ABI 函数均为追加；`legado_explore` 签名未变，老宿主无需改动。

### 新增

- `legado_import(engine, text)`：导入分享链接、URL、`base64:`、data URL、gzip 与
  裸 JSON，也识别夹在文案中的链接。按内容形状分类为书源 / 订阅源 / 净化规则，返回
  原始 JSON 对象（未知字段保留）。`engine` 可为 `NULL`。
- `legado_replace(rules_json, text, context_json, target)`：执行替换净化，按
  `order` / `isEnabled` / `scope` / `excludeScope` / `scopeTitle` / `scopeContent`
  过滤；纯正则走 Rust 正则，后向引用与前后向断言退回 QuickJS。逐条报告
  `applied`（含 `changed`）/ `skipped`（含 `reason`）/ `errors`。
- `legado_rss_sources` / `legado_rss_articles` / `legado_rss_content`：订阅源列表、
  文章列表（`ruleNextPage` 翻页，返回 `nextUrl`）、单篇正文。
- `legado_explore_page(engine, source_json, url, key, page)`：填入 `{{key}}` /
  `{{page}}`，用于发现页翻页。
- `legado_cancel(engine)`：任意线程叫停进行中的调用；被取消的调用错误为
  `LEGADO_CANCELLED`，取消不粘手。Rust 侧为 `engine.cancel()` /
  `engine.canceller()`。
- `legado_version` 的版本号取自 `Cargo.toml`。

### 修复

- 2.x 书源导入后规则为空：规则对象内的旧键名（`ruleSearch.ruleSearchList` 等）
  未被迁移；现在两处都认，嵌套优先。
- 发现页分类无法翻页：请求固定 `page=1`、`key=""`。
- `sources/wenku8.json` 的 `author` / `kind` 缺 `@js:` 前缀，始终为空。
- 净化 `context_json` 传空串时报 JSON 解析错误；现与 `NULL` 等价。
- 净化脚本超时提示显示为"超过 0 秒"。
- 导入指向网页（GitHub blob 等）时只报 serde 行列号；现提示改用 raw 地址。
- 导入带 UTF-8 BOM 的正文解析失败。
- `timeoutSecs` 未生效：总超时为 `max(60, timeoutSecs)`。现按配置值执行，默认 15 秒。
- 下载受单请求超时限制被截断：改为独立的 30 分钟总超时，停滞 60 秒判死。
- `concurrentRate` 仅约束脚本内请求：搜索 / 详情 / 目录 / 正文 / 下载均纳入限速，
  跨线程共享窗口；`java.ajax` 的超时也改用同一默认值。
- `429` / `503` 的 `Retry-After` 被忽略；现按它退避，上限 10 秒。
- 书源脚本无上限，死循环会挂住线程；现由 QuickJS 中断路径在 120 秒终止。

### 内部

- 新增 `src/cancel.rs`、`src/import.rs`、`src/parser/replace.rs`；`src/model/rss.rs`
  补齐解析入口（含 `enable` / `ruleArticleList` 等旧键名迁移）。
- 取消检查点：HTTP 发送、读 body、重试退避、限速排队、书源脚本、下载落盘前。
- CI 增加 `cargo fmt --check`、`cargo clippy -D warnings`、`--test sources` /
  `--test import` / `--test replace` / `--test cancel`。
- 清理 clippy 全部告警与 `cargo fmt` 差异；移除未使用依赖 `serde_with`、`quick-xml`；
  `src/error/error.rs` 合并进 `src/error/mod.rs`。
- `examples/probe` 增加 `import` / `replace` / `rss` 子命令，`explore` 支持页码。

## 0.3.0 及以前

未保留变更记录。0.3.0 含下载型书源（`legado_download`）、`jsLib` 表单登录、
按线程各自的返回值与错误、iOS 真机切片。
