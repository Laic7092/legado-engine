# 接入

## 链接

`cargo build --release --lib` 产出 `liblegado_engine.a`。Linux 链接需要
`-lpthread -ldl -lm`；其他目标需要的系统库见
`cargo rustc --release --lib -- --print native-static-libs`。

## 生命周期

```c
LegadoEngine *engine = legado_open("{\"storageDir\":\"/path/to/legado\"}");
if (!engine) { /* legado_last_error(NULL) */ }

const char *json = legado_search(engine, source_json, key, 1);
if (!json) { /* legado_last_error(engine) */ }

legado_close(engine);
```

一个进程打开一个 handle。JS 存储和 HTTP 客户端是进程级的，与 handle 无关。

## 内存

`legado_*` 返回的 `const char *` 归引擎所有，在下一次调用前有效。要跨调用保留
就先复制。`legado_sources` 没有 handle，它的指针在下一次 `legado_sources`
调用前有效。

## 线程

一个 handle 可以从多个线程同时调用，不需要在调用方加锁。JS 上下文、当前书源
和请求上下文是 thread-local，cookie / HTTP / JS 存储有锁；`legado_search` 的
返回值和 `legado_last_error` 也按调用线程各存一份。`legado_search` /
`legado_toc` / `legado_content` 会发 HTTP 并执行 JS，不要放在 UI 线程；多个
书源可以并发调用同一个 handle。

## 错误

失败时返回 `NULL`，`legado_last_error(engine)` 给出原因（`legado_open` 失败时
用 `legado_last_error(NULL)`）。错误字符串同样是下一次调用前有效。

## 输入格式

- `source_json`：单个 Legado 书源对象。
- `book_json`：`legado_info` 返回的 JSON，或同字段的对象。
- `chapter_json`：`BookChapter` 对象（`url` 必须有值）。
- `legado_sources`：书源合集（数组或单个对象），返回
  `{name, url, group, kind, enabled, comment, searchUrl, exploreUrl, hasJs}`。

## 示例（C）

```c
#include <stdio.h>
#include "legado.h"

int main(void) {
    LegadoEngine *engine = legado_open("{\"direct\":true}");
    if (!engine) {
        fprintf(stderr, "%s\n", legado_last_error(NULL));
        return 1;
    }
    const char *books = legado_search(engine, source_json, "我的", 1);
    if (!books) {
        fprintf(stderr, "%s\n", legado_last_error(engine));
    } else {
        puts(books);
    }
    legado_close(engine);
}
```

## Asuka 绑定

```asuka
extern type LegadoEngine with release legado_close

extern fn legado_open(config: CString) -> LegadoEngine?
extern fn legado_close(engine: LegadoEngine)
extern fn legado_last_error(engine: LegadoEngine) -> CString

extern fn legado_sources(json: CString) -> CString?

extern fn legado_search(engine: LegadoEngine, source: CString, key: CString, page: Int32) -> CString?
extern fn legado_explore(engine: LegadoEngine, source: CString, url: CString) -> CString?
extern fn legado_info(engine: LegadoEngine, source: CString, bookUrl: CString) -> CString?
extern fn legado_toc(engine: LegadoEngine, source: CString, bookJson: CString) -> CString?
extern fn legado_content(engine: LegadoEngine, source: CString, bookJson: CString, chapterJson: CString) -> CString?

extern fn legado_cookie_header(engine: LegadoEngine, source: CString, url: CString) -> CString?
extern fn legado_set_cookie(engine: LegadoEngine, source: CString, url: CString, cookie: CString) -> CString?

extern fn legado_eval(engine: LegadoEngine, source: CString, script: CString, input: CString, baseUrl: CString) -> CString?
extern fn legado_eval_rule(engine: LegadoEngine, source: CString, rule: CString, body: CString, baseUrl: CString) -> CString?
```

调用方把 handle 放进一个 worker 持有，页面切换时不关闭。

```asuka
worker Legado {
  var engine: LegadoEngine? = none

  pub fn open(storage: string) -> bool {
    if engine != none { return true }
    engine = legado_open("{\"storageDir\":\"{storage}\",\"userNs\":\"app\"}")
    return engine != none
  }

  pub fn search(source: string, key: string, page: int) -> Result {
    if let e = engine {
      if let text = legado_search(e, source, key, Int32(page)) {
        return Result(ok: true, text: text)
      }
      return Result(ok: false, text: legado_last_error(e))
    }
    return Result(ok: false, text: "engine is not open")
  }
}
```

## 下载

文件型书源（如知轩藏书，`bookSourceType: 3`）没有章节，把下载链接放在
`ruleBookInfo.downloadUrls` 里，每行一条：

```
TXT::https://download.example/books/1.txt
EPUB::https://download.example/books/1.epub
```

裸 URL 也可以（名称由宿主决定）。然后：

```c
const char *done = legado_download(engine, source_json, url, "/tmp/book.txt");
/* {"path":"/tmp/book.txt","size":7172215,"contentType":"text/plain",
 *  "charset":"gb18030","url":"…"} */
```

- 父目录会自动创建，写入先落到 `dest.part` 再 rename，失败不会留下半截文件。
- 用书源的 header / cookie / 代理请求，能过和详情页同一层反爬（Cloudflare、
  按 UA 投毒等）。
- 字节原样落盘；`charset` 依次看响应头、BOM、UTF-8 合法性，最后回退
  `gb18030`（中文 TXT 常见）。

## 登录

两条路：

1. **书源自己登**（纯 HTTP，无 WebView）：在 `jsLib` 里用 `java.post` 提交登录表单，
   把账号密码放在书源变量（`source.put`）里。登录拿到的 Cookie 与
   `search/info/toc/content` 共用同一个 per-source cookie jar。示例见
   [`sources/wenku8.json`](../sources/wenku8.json)。
2. **宿主注入**：需要在 WebView 完成登录（验证码、扫码等）时，把 Cookie 交给引擎：

```asuka
legado_set_cookie(engine, sourceJson, sourceUrl, cookieHeaderFromWebView)
```

封面等资源如果需要登录态，用 `legado_cookie_header` 取 Cookie 后用调用方自己的
HTTP 客户端请求，或用 `legado_eval` 中的 `java.ajax` 走引擎的 client。
