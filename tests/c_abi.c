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
    legado_close(engine);
    return 0;
}
