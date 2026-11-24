# C embedding

Download the release asset (static lib + `datastreamdb.h`, also checked in
at `crates/datastreamdb-ffi/datastreamdb.h`) and link it:

```sh
cc app.c -ldatastreamdb_ffi -o app
```

```c
#include <stdio.h>
#include <string.h>
#include "datastreamdb.h"

int main(void) {
  printf("datastreamdb version: %s\n", datastreamdb_version());

  datastreamdb_handle_t *a = NULL, *b = NULL;
  if (datastreamdb_open("{}", &a) != DATASTREAMDB_OK) {
    fprintf(stderr, "open: %s\n", datastreamdb_last_error());
    return 1;
  }
  datastreamdb_open("{\"tenant\": \"b\"}", &b);

  uint8_t *out = NULL;
  size_t len = 0;
  const char *ping = "{\"op\":\"ping\"}";
  if (datastreamdb_execute(a, (const uint8_t *)ping, strlen(ping), &out, &len) == DATASTREAMDB_OK) {
    printf("%.*s\n", (int)len, out);
    datastreamdb_free_bytes(out, len);
  }
  datastreamdb_close(a);
  datastreamdb_close(b);
  return 0;
}
```

## ABI

```c
typedef struct datastreamdb_handle datastreamdb_handle_t;
int32_t datastreamdb_open(const char *config_json, datastreamdb_handle_t **out);
int32_t datastreamdb_execute(const datastreamdb_handle_t *h, const uint8_t *req,
                       size_t req_len, uint8_t **out, size_t *out_len);
int32_t datastreamdb_close(datastreamdb_handle_t *h);
void datastreamdb_free_bytes(uint8_t *ptr, size_t len);
const char *datastreamdb_last_error(void);
const char *datastreamdb_version(void);
```

- Status `0` (`DATASTREAMDB_OK`) is success; `3` is an engine error, `4` a
  caught panic — details via `datastreamdb_last_error()`.
- `datastreamdb_execute` allocates the response; the caller releases it with
  `datastreamdb_free_bytes`.
- Handles are thread-safe; closing is per-instance.

A complete reference consumer lives at `bindings/c/smoke.c` in the repo.
