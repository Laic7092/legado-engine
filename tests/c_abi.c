/* A smoke test for the C ABI: open, version, close. Build with `make test-c`. */
#include <stdio.h>
#include <string.h>
#include "legado.h"

int main(void) {
    printf("%s\n", legado_version());
    LegadoEngine *engine = legado_open("{\"direct\":true}");
    if (!engine) {
        fprintf(stderr, "open failed: %s\n", legado_last_error(NULL));
        return 1;
    }
    if (strlen(legado_last_error(engine)) != 0) {
        fprintf(stderr, "unexpected error: %s\n", legado_last_error(engine));
        return 1;
    }
    const char *answer = legado_search(engine, "{\"bookSourceName\":\"x\",\"bookSourceUrl\":\"https://example.com\"}", "k", 1);
    if (answer) {
        fprintf(stderr, "expected a missing searchUrl error, got %s\n", answer);
        return 1;
    }
    printf("expected error: %s\n", legado_last_error(engine));

    /* 导入和净化不碰网络，NULL handle 也能跑。 */
    const char *imported = legado_import(
        NULL,
        "[{\"bookSourceUrl\":\"https://a.test\"},"
        "{\"sourceUrl\":\"https://b.test\"}]");
    if (!imported || !strstr(imported, "\"bookSources\"") ||
        !strstr(imported, "\"rssSources\"")) {
        fprintf(stderr, "legado_import failed: %s\n", legado_last_error(NULL));
        return 1;
    }
    printf("import: %s\n", imported);

    const char *cleaned = legado_replace(
        "[{\"id\":1,\"name\":\"ad\",\"pattern\":\"(本章完)\",\"replacement\":\"\"}]",
        "正文(本章完)", NULL, NULL);
    if (!cleaned || !strstr(cleaned, "\"text\":\"正文\"")) {
        fprintf(stderr, "legado_replace failed: %s\n",
                cleaned ? cleaned : legado_last_error(NULL));
        return 1;
    }
    printf("replace: %s\n", cleaned);

    /* 分享链接本身也要能认（内嵌 base64，仍然不需要 handle）。 */
    const char *inline_link = legado_import(
        NULL, "legado://import/bookSource?src=eyJib29rU291cmNlVXJsIjoiaHR0cHM6Ly9hLnRlc3QifQ==");
    if (!inline_link || !strstr(inline_link, "\"origin\":\"base64\"")) {
        fprintf(stderr, "inline import failed: %s\n", legado_last_error(NULL));
        return 1;
    }

    /* 取消: 闲着的时候叫停是空操作, 之后的调用照常。 */
    legado_cancel(engine);
    legado_cancel(NULL);
    const char *after_cancel = legado_replace(
        "[{\"id\":1,\"name\":\"ad\",\"pattern\":\"123\",\"replacement\":\"\"}]",
        "共 123 章", NULL, NULL);
    if (!after_cancel || !strstr(after_cancel, "\"text\":\"共  章\"")) {
        fprintf(stderr, "a cancel on an idle handle broke the next call: %s\n",
                after_cancel ? after_cancel : legado_last_error(NULL));
        return 1;
    }

    legado_close(engine);
    return 0;
}
