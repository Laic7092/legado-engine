# 示例书源

两个文件都是 Legado 书源 JSON，直接用 `legado_open` 的宿主导入即可。

| 文件 | 类型 | 说明 |
| --- | --- | --- |
| `wenku8.json` | 在线 + 下载（`bookSourceType: 0`） | 轻小说文库。搜索/分类需登录，详情/目录/正文免登录；详情页同时给出 GBK/UTF-8 TXT 下载链接 |
| `zxcs.json` | 文件（`bookSourceType: 3`） | 知轩藏书。TXT 直链，用 `legado_download` 落盘 |

## wenku8 登录

搜索和发现页需要账号。凭据存在**书源变量**里，不会写进 JSON：

```c
/* 用 legado_eval 执行一次，storageDir 会持久化 */
legado_eval(engine, source_json,
            "source.put('wenku8_user','你的用户名');"
            "source.put('wenku8_pass','你的密码')",
            "", "https://www.wenku8.net");
```

也可以在书源的「变量」里填 JSON：`{"user":"…","pass":"…"}`，`jsLib` 会兜底读取。

`header` 里的 `Sec-Fetch-Site: none` 是过 Cloudflare 的关键（CF 对没有
`Sec-Fetch-*` 的请求直接回 `Just a moment...`），别删。

## wenku8 下载

同一本书源既能在阅读也能下载：`bookSourceType` 仍是 0，`ruleBookInfo.downloadUrls`
给出两条：

```
简体(G)::https://dl.wenku8.com/down.php?type=txt&node=1&id=<bookId>
简体(U)::https://dl.wenku8.com/down.php?type=utf8&node=1&id=<bookId>
```

下载不需要登录，但站点对连续下载限速（429）。宿主应当让用户**选一个**格式，
不要三条连着下。下载主机是 `dl.wenku8.com`，会 302 到 `dl1.wenku8.com`；引擎
已处理跨域跳转时显式 `Host` 失效的问题（否则 403）。

## zxcs

站点按 UA 反爬：非浏览器 UA 会拿到一个 Google Fonts 的诱饵页。引擎默认发
Chrome UA，所以不需要额外配置。

流程是：

```
legado_info(book_url)        → book.downloadUrls = "TXT::https://download.zxcs.zip/《…》.txt"
legado_download(url, path)   → {"path":"…","size":7172215,"charset":"gb18030"}
```

没有 `toc`/`content`，宿主识别到 `kind == 3` 时直接走下载，不要进阅读页。

## 自测

```sh
cargo run --release --example probe -- sources/zxcs.json search 我的
cargo run --release --example probe -- sources/zxcs.json info https://zxcs.zip/book/10461.html
cargo run --release --example probe -- sources/zxcs.json download https://zxcs.zip/book/10461.html /tmp

cargo run --release --example probe -- sources/wenku8.json info https://www.wenku8.net/book/1.htm
cargo run --release --example probe -- sources/wenku8.json toc https://www.wenku8.net/book/1.htm
```
