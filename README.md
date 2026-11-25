# legado-engine

Legado（阅读3.0）书源规范的 Rust 实现，带 C ABI。

- **规则**：CSS、XPath、JSONPath、正则、模板，`&&` / `||` / `%%` 连接，`##` 替换
- **JavaScript**：`@js:`、`<js>`、`{{…}}`，运行在 QuickJS（rquickjs）上
- **宿主 API**：`java.*`、`source.*`、`book.*`、`chapter.*`、`cookie.*`、`cache.*`、
  `result`、`Packages.*`、`JavaImporter`、`org.jsoup.*`
- **HTTP**：每源独立的 client、cookie jar、变量存储、代理

## 构建

```sh
cargo build --release --lib   # target/release/liblegado_engine.a
```

Linux 链接需要 `pthread dl m`；其他目标需要的系统库见
`cargo rustc --release --lib -- --print native-static-libs`。

```sh
make test     # 单元测试 + 本地 HTTP 全流程测试
make test-c   # 用 C 程序调用 open / version / close
```

## C ABI

声明见 [`legado.h`](legado.h)。一个 handle 拥有 HTTP 客户端、cookie jar、
规则引擎和 JS 运行时。

| 类别 | 函数 |
| --- | --- |
| 生命周期 | `legado_open`、`legado_close`、`legado_version` |
| 书源 | `legado_search`、`legado_explore`、`legado_info`、`legado_toc`、`legado_content` |
| 会话 | `legado_cookie_header`、`legado_set_cookie` |
| 调试 | `legado_eval`、`legado_eval_rule` |
| 元数据 | `legado_sources` |

返回值约定：

- 返回的 `const char *` 是 UTF-8，归引擎所有，在下一次调用前有效；需要保留就先复制。
- 失败返回 `NULL`，原因用 `legado_last_error(engine)` 读取，指针同样在下一次调用前有效。
- `legado_sources` 不需要 handle，结果在下一次 `legado_sources` 调用前有效。

## 配置

`legado_open` 的 `config_json` 可以为 `NULL`，或：

```json
{
  "storageDir": "/path/to/app-data/legado",
  "proxy": null,
  "timeoutSecs": 30,
  "userNs": "app",
  "direct": false
}
```

`storageDir` 保存每个书源的 cookie jar 和 `source.getVariable()` 写入的变量，
省略时只存在内存中。`direct` 忽略系统代理。

## 线程

一个 handle 可以从多个线程同时使用：JS 上下文、当前书源和请求上下文都是
thread-local，cookie / HTTP / JS 存储有锁。每个线程的返回值和错误各有一份，
指针在下一次同一线程调用前有效。`search` / `toc` / `content` 会发 HTTP 并执行
JS，调用方应放在工作线程；多个书源搜索可以直接并发调用同一个 handle。

## 限制

- HTTP 使用 reqwest，不经过宿主的网络栈。系统代理只覆盖 Windows（注册表）、
  macOS（`networksetup`）和 GNOME（`gsettings`）；其他平台读 `HTTPS_PROXY`
  等环境变量。需要时用 `config.proxy` 指定。
- 按 Legado 的行为，TLS 不校验证书和主机名。
- `java.startBrowser*` / `webView` 未实现；登录后的 Cookie 用
  `legado_set_cookie` 注入。
- `exploreUrl` 的分类 JSON 由调用方解析；`legado_explore` 处理单个分类 URL。
- 只在 Linux 上本地验证，其他目标由 CI 构建。

## 目录

| 路径 | 内容 |
| --- | --- |
| `src/model/` | 书源、书、章节、规则 |
| `src/parser/` | 规则引擎、JS 运行时、Java 兼容层 |
| `src/crawler/` | HTTP、cookie jar、URL 规则 |
| `src/pipeline.rs` | 搜索、详情、目录、正文 |
| `src/abi.rs` | C ABI |
| `src/service/` | 浏览器登录接口（未实现） |
| `examples/`、`tests/` | 示例与测试 |

## 文档

- [`docs/integration.md`](docs/integration.md)：接入、内存与线程约定
- [`UPSTREAM.md`](UPSTREAM.md)：来源与许可证

## License

MIT，见 [`LICENSE`](LICENSE)。
