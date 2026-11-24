# C embedding

Download the release asset (`libdatastreamdb_ffi.a` + `datastreamdb.h`, also
checked in at `crates/datastreamdb-ffi/datastreamdb.h`) and link it:

```sh
cc app.c -I. -L/path/to/lib -ldatastreamdb_ffi -lpthread -ldl -lm -o app
```

If you link the shared library, make it discoverable at runtime:

```sh
LD_LIBRARY_PATH=/path/to/lib ./app
```

## Supported types

| Item | Type | Notes |
|---|---|---|
| Config | JSON string | `{}` for memory; `data_dir` for persistence. |
| Key | JSON string | UTF-8. |
| Value | JSON string | UTF-8 only in the envelope. |
| JSON document | escaped JSON string | Use a JSON library to build/parse. |
| Binary | base64 string | The envelope has no byte-array value type. |
| Response buffer | `uint8_t*` + length | Release with `datastreamdb_free_bytes`. |

## Open, put, get, scan, batch, flush

```c
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "datastreamdb.h"

static char *exec_copy(const datastreamdb_handle_t *h, const char *req) {
  uint8_t *out = NULL;
  size_t len = 0;
  if (datastreamdb_execute(h, (const uint8_t *)req, strlen(req), &out, &len)
      != DATASTREAMDB_OK) {
    return NULL;
  }
  char *copy = (char *)malloc(len + 1);
  if (copy == NULL) {
    datastreamdb_free_bytes(out, len);
    return NULL;
  }
  memcpy(copy, out, len);
  copy[len] = '\0';
  datastreamdb_free_bytes(out, len);
  return copy;
}

static int run(datastreamdb_handle_t *db, const char *req) {
  char *response = exec_copy(db, req);
  if (response == NULL) {
    fprintf(stderr, "datastreamdb: %s\n", datastreamdb_last_error());
    return -1;
  }
  printf("%s\n", response);
  free(response);
  return 0;
}

int main(void) {
  datastreamdb_handle_t *db = NULL;
  if (datastreamdb_open("{\"tenant\":\"app\"}", &db) != DATASTREAMDB_OK) {
    fprintf(stderr, "open: %s\n", datastreamdb_last_error());
    return 1;
  }

  run(db, "{\"op\":\"put\",\"key\":\"user:1\",\"value\":\"Ada\"}");
  run(db, "{\"op\":\"put\",\"key\":\"user:2\",\"value\":\"Bob\"}");

  // Prefix scan, ordered by key.
  run(db, "{\"op\":\"scan\",\"prefix\":\"user:\",\"limit\":100}");

  // Atomic multi-write: two puts + one delete.
  run(db, "{\"op\":\"batch\","
          "\"puts\":[{\"key\":\"user:3\",\"value\":\"Cara\"}],"
          "\"deletes\":[\"user:2\"]}");

  run(db, "{\"op\":\"get\",\"key\":\"user:1\"}");
  run(db, "{\"op\":\"flush\"}");

  datastreamdb_close(db);
  return 0;
}
```

## Request and response shapes

| `op` | Request | Success response |
|---|---|---|
| `ping` | `{"op":"ping"}` | `{ok, instance, tenant, backend}` |
| `put` | `key`, `value` | `{ok:true}` |
| `get` | `key` | `{ok:true, value:string-or-null}` |
| `delete` | `key` | `{ok:true}` |
| `scan` | `prefix`, `limit?` | `{ok:true, pairs:[{key,value}]}` |
| `batch` | `puts[]`, `deletes[]` | `{ok:true, applied:n}` |
| `flush` | `{"op":"flush"}` | `{ok:true}` |

`scan` defaults to 1000 pairs and clamps to 100,000. There is no cursor;
narrow the prefix or raise the limit to page.

## Error handling

`datastreamdb_execute` returns a status code. On failure, inspect the
thread-local message:

```c
if (datastreamdb_execute(db, (const uint8_t *)req, strlen(req),
                         &out, &len) != DATASTREAMDB_OK) {
  fprintf(stderr, "execute failed: %s\n", datastreamdb_last_error());
}
```

| Code | Name | Meaning |
|---|---|---|
| `0` | `DATASTREAMDB_OK` | Success |
| `1` | `DATASTREAMDB_EINVAL` | Null pointer, invalid UTF-8, malformed envelope |
| `2` | `DATASTREAMDB_ECONFIG` | Config JSON rejected |
| `3` | `DATASTREAMDB_EENGINE` | Storage/backend failure |
| `4` | `DATASTREAMDB_EPANIC` | Caught Rust panic; instance state unchanged |

`datastreamdb_last_error()` is thread-local and never returns `NULL`.

## Ownership and lifecycle

- `datastreamdb_open` allocates the handle; pass it to `datastreamdb_close`.
- `datastreamdb_execute` allocates the response buffer; release it with
  `datastreamdb_free_bytes(out, len)`.
- Closing the last handle flushes and releases the instance’s file lock.
- Handles are `Send + Sync`; calls may run on any thread.
- `datastreamdb_last_error` is per-thread.

## Limitations in 0.1.0

- Embedded mode is key/value only. DQL and GraphQL run against the standalone
  servers.
- Envelope values are strings; base64-encode binary data.
- Scans are bounded and have no continuation cursor.

A complete reference consumer lives at `bindings/c/smoke.c` in the repo.
