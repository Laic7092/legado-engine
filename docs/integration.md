# 接入

## 链接

`cargo build --release --lib` 产出 `liblegado_engine.a`。Linux 需 `-lpthread -ldl -lm`；
其他目标用 `cargo rustc --release --lib -- --print native-static-libs` 查。

## 生命周期

```c
LegadoEngine *engine = legado_open("{\"storageDir\":\"/path/to/legado\"}");
if (!engine) { /* legado_last_error(NULL) */ }

const char *json = legado_search(engine, source_json, key, 1);
if (!json) { /* legado_last_error(engine) */ }

legado_close(engine);
```

一个进程一个 handle 即可。`storageDir` 存放 cookie jar、`source.getVariable()` 的
变量与 `java.cacheFile` 的缓存；省略则只存在内存中。

## 内存与线程

- 返回的 `const char *` 归引擎所有，在下一次同线程调用前有效；跨调用保留需复制。
  `legado_sources` 没有 handle，其指针在下一次 `legado_sources` 调用前有效。
- 同一 handle 可从多线程并发调用，无需加锁。JS 上下文、当前书源、请求上下文是
  thread-local；cookie / HTTP / JS 存储加锁。返回值与 `legado_last_error` 按线程
  各存一份。
- `legado_search` / `legado_toc` / `legado_content` 会发 HTTP 并执行 JS，不要放在
  UI 线程；多个书源可直接并发调用同一 handle。

## 取消

```c
legado_cancel(engine);   /* 任意线程 */
```

被取消的调用返回 `NULL`，`legado_last_error` 恰为 `LEGADO_CANCELLED`
（`"cancelled"`）：

```c
if (!json) {
    const char *error = legado_last_error(engine);
    if (strcmp(error, LEGADO_CANCELLED) == 0) { /* 宿主自己叫停，不是失败 */ }
    else { /* 真的失败 */ }
}
```

- 只影响进行中的调用；之后的调用照常，无需重置。
- 同一 handle 上的并发调用会被一起叫停。按任务隔离时使用多个 handle。
- 检查点：HTTP 发送、读 body、重试退避、`concurrentRate` 排队、书源脚本、
  下载落盘前（取消时不写文件）。
- 已写出的字节不会回滚；落盘之后的取消不影响文件完整性。

典型接法：worker 线程持有 handle，UI 线程只调 `legado_cancel`。

## 错误

失败返回 `NULL`，原因见 `legado_last_error(engine)`（`legado_open` 失败时传
`NULL`）。错误为英文短句，仅供展示；唯一有稳定值的是 `LEGADO_CANCELLED`，
其余请按"失败"统一处理，不要匹配文案。

## 超时与重试

| 项 | 值 |
| --- | --- |
| 单请求总超时 | `timeoutSecs`，默认 15 秒 |
| 建连超时 | 10 秒 |
| 读超时（停滞判定） | 60 秒，仅对下载生效 |
| 下载总超时 | 30 分钟 |
| 重试 | 默认 2 次，退避 200ms × 次数 |
| `429` / `503` | 按 `Retry-After` 等待，上限 10 秒 |

- 非 2xx 也重试：部分站点首次请求返回 403 + 挑战 Cookie，重发即 200。
- 书源 `concurrentRate`（`"1/300"` = 每 300ms 一次，`"3"` = 串行且间隔 3 秒）对
  搜索 / 详情 / 目录 / 正文 / 下载生效，跨线程共享窗口。
- 书源脚本硬上限 120 秒，用于终止死循环。

## 输入格式

- `source_json`：单个 Legado 书源对象。
- `book_json`：`legado_info` 的返回值或同字段对象。
- `chapter_json`：`BookChapter` 对象，`url` 必填。
- `article_json`：`legado_rss_articles` 返回的数组元素。
- `rules_json`：替换规则数组；`context_json`：`{"bookName","bookUrl","sourceUrl"}`。

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

## 下载

文件型书源（`bookSourceType: 3`）无章节，下载链接在 `ruleBookInfo.downloadUrls`，
每行一条（裸 URL 亦可）：

```
TXT::https://download.example/books/1.txt
```

```c
const char *done = legado_download(engine, source_json, url, "/tmp/book.txt");
/* {"path":"/tmp/book.txt","size":7172215,"contentType":"text/plain",
 *  "charset":"gb18030","url":"…"} */
```

父目录自动创建；先写 `dest.part` 再 rename。字节原样落盘，`charset` 依次按响应头、
BOM、UTF-8 合法性、GB18030 嗅探。请求使用书源的 header / cookie / 代理。

## 登录

1. 书源自登：在 `jsLib` 里用 `java.post` 提交表单，Cookie 与规则请求共用同一
   per-source jar。示例见 [`sources/wenku8.json`](../sources/wenku8.json)。
2. 宿主注入：WebView 登录后把 Cookie 交给引擎：

```c
legado_set_cookie(engine, source_json, source_url, cookie_header);
```

需要登录态的图片等资源，用 `legado_cookie_header` 取 Cookie 后自行请求。

## 发现页翻页

```json
"exploreUrl": "[{\"title\":\"全部\",\"url\":\"/list?page={{page}}\"}]"
```

`legado_explore_page(engine, source_json, category_url, key, page)` 填入 `{{page}}`
与 `{{key}}`（`key` 可为 `NULL`）；`legado_explore` 等价于 `key=""`、`page=1`。
引擎不记分页状态，宿主每次 +1。

## 导入

`legado_import` 接受 `legado://import/…` 分享链接、裸 URL、`base64:`、data URL、
gzip、裸 JSON，以及夹在文案中的链接。

```c
const char *json = legado_import(engine,
    "新合集 legado://import/bookSource?src=https://example.com/all.json 收好");
/* {"declaredKind":"bookSource","url":"https://example.com/all.json","origin":"url",
 *  "bookSources":[…],"rssSources":[…],"replaceRules":[…],"unknown":0} */
```

- `engine` 可为 `NULL`（内嵌内容无需下载）；`src` 指向网络时需要 handle。
- 分类按内容形状判定，不按链接声明的类型；`declaredKind` 仅表示链接意图。
- 返回原始 JSON 对象，未知字段不丢。
- `unknown` 为无法识别的对象个数，非零时提示用户。
- 去重键：书源 `bookSourceUrl`，订阅源 `sourceUrl`。去重与合并策略由宿主决定。

## 净化

`legado_content` / `legado_rss_content` 返回原文；净化由宿主调用：

```c
const char *out = legado_replace(rules_json, raw_text, ctx_json, "content");
/* {"text":"…","applied":[{"id":7,"name":"删本章说","changed":true}],
 *  "skipped":[{"id":9,"reason":"scope"}],"errors":[{"id":3,"error":"…"}]} */
```

- `target` 传 `"title"` 才作用于章节标题（规则还需 `scopeTitle: true`）。
- `skipped[].reason`：`disabled` / `scope` / `excludeScope` / `title` / `content` /
  `emptyPattern`；`applied[].changed` 区分"执行了"与"真的改动了"。
- `pattern` 按 Java 正则处理；`\Q…\E`、`(?x)` 报错并保持原文。
- 替换保护 `<img>` / `[newpage]` / 内联样式脚本。

## 订阅源

订阅源是另一份 JSON（`sourceUrl` + `ruleArticles`），规则写法与书源通用。

```c
const char *list = legado_rss_articles(engine, rss_json, NULL /* 或 sortUrl 分类 */);
/* {"articles":[{"title":…,"link":…,"pubDate":…,"description":…,"image":…,"order":0}],
 *  "nextUrl":"https://…/page/2"} */

const char *body = legado_rss_content(engine, rss_json, article_json);
```

- `nextUrl` 为空表示没有下一页；自动加载时再喂回 `legado_rss_articles`。
- `singleUrl: true` 或未写 `ruleArticles` 表示 `sourceUrl` 即一篇文章。
- `legado_rss_sources` 返回摘要：`{name, url, group, enabled, comment, sortUrl,
  singleUrl, hasJs}`；`sortUrl` 原样返回，分类 JSON 由宿主解析。

## 语言绑定

```asuka
extern type LegadoEngine with release legado_close

extern fn legado_open(config: CString) -> LegadoEngine?
extern fn legado_close(engine: LegadoEngine)
extern fn legado_cancel(engine: LegadoEngine)
extern fn legado_last_error(engine: LegadoEngine) -> CString

extern fn legado_sources(json: CString) -> CString?

extern fn legado_search(engine: LegadoEngine, source: CString, key: CString, page: Int32) -> CString?
extern fn legado_explore(engine: LegadoEngine, source: CString, url: CString) -> CString?
extern fn legado_explore_page(engine: LegadoEngine, source: CString, url: CString, key: CString?, page: Int32) -> CString?
extern fn legado_info(engine: LegadoEngine, source: CString, bookUrl: CString) -> CString?
extern fn legado_toc(engine: LegadoEngine, source: CString, bookJson: CString) -> CString?
extern fn legado_content(engine: LegadoEngine, source: CString, bookJson: CString, chapterJson: CString) -> CString?

extern fn legado_cookie_header(engine: LegadoEngine, source: CString, url: CString) -> CString?
extern fn legado_set_cookie(engine: LegadoEngine, source: CString, url: CString, cookie: CString) -> CString?

extern fn legado_eval(engine: LegadoEngine, source: CString, script: CString, input: CString, baseUrl: CString) -> CString?
extern fn legado_eval_rule(engine: LegadoEngine, source: CString, rule: CString, body: CString, baseUrl: CString) -> CString?

extern fn legado_import(engine: LegadoEngine?, text: CString) -> CString?
extern fn legado_replace(rules: CString, text: CString, context: CString?, target: CString?) -> CString?

extern fn legado_rss_sources(json: CString) -> CString?
extern fn legado_rss_articles(engine: LegadoEngine, source: CString, sortUrl: CString?) -> CString?
extern fn legado_rss_content(engine: LegadoEngine, source: CString, article: CString) -> CString?
```

调用方把 handle 交给 worker 持有，页面切换时不关闭。

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
