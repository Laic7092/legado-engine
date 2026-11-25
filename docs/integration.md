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

一个 handle 同一时间只在一个线程上使用，不需要加锁。`legado_search` /
`legado_toc` / `legado_content` 会发 HTTP 并执行 JS，不要放在 UI 线程。

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

## 登录

`java.startBrowser*` / `webView` 未实现。需要在 WebView 完成登录，再把 Cookie
注入引擎：

```asuka
legado_set_cookie(engine, sourceJson, sourceUrl, cookieHeaderFromWebView)
```

封面等资源如果需要登录态，用 `legado_cookie_header` 取 Cookie 后用调用方自己的
HTTP 客户端请求，或用 `legado_eval` 中的 `java.ajax` 走引擎的 client。
