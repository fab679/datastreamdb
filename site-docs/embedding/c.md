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

## Open an instance

```c
#include <stdio.h>
#include <string.h>

#include "datastreamdb.h"

static int exec(datastreamdb_handle_t *db, const char *request) {
  uint8_t *out = NULL;
  size_t len = 0;
  int rc = datastreamdb_execute(db, (const uint8_t *)request,
                                strlen(request), &out, &len);
  if (rc != DATASTREAMDB_OK) {
    fprintf(stderr, "execute: %s\n", datastreamdb_last_error());
    return rc;
  }
  fwrite(out, 1, len, stdout);
  fputc('\n', stdout);
  datastreamdb_free_bytes(out, len);
  return DATASTREAMDB_OK;
}

int main(void) {
  datastreamdb_handle_t *db = NULL;
  if (datastreamdb_open("{}", &db) != DATASTREAMDB_OK) {
    fprintf(stderr, "open: %s\n", datastreamdb_last_error());
    return 1;
  }

  /* DQL and GraphQL envelopes go here. */

  datastreamdb_close(db);
  return 0;
}
```

`{}` opens an in-memory instance. Pass
`{"data_dir":"/var/lib/app/graph-a","tenant":"app"}` for a persistent one.

## DQL

```c
exec(db, "{\"op\":\"dql_alter\",\"schema\":\"name: string @index(exact) . friend: [uid] @reverse .\"}");

exec(db, "{\"op\":\"dql_mutate\",\"query\":\"{ set { _:a <name> \\\"Ada\\\" . _:b <name> \\\"Bob\\\" . _:a <friend> _:b . } }\"}");

exec(db, "{\"op\":\"dql_query\",\"query\":\"{ q(func: eq(name, \\\"Ada\\\")) { name friend { name } } }\"}");
```

The C ABI takes JSON envelopes. Build them with a JSON library rather than
escaping DQL by hand. DQL type declarations need one field per line, so a
multiline schema is sent as a JSON string containing `\n`.

## GraphQL

```c
exec(db, "{\"op\":\"set_graphql_schema\",\"sdl\":\"type Post { id: ID! title: String @search(by: [term]) }\"}");

exec(db, "{\"op\":\"graphql\",\"query\":\"mutation { addPost(input: [{title: \\\"Hello\\\"}]) { post { id title } numUids } }\"}");

exec(db, "{\"op\":\"graphql\",\"query\":\"{ queryPost { title } }\"}");
```

GraphQL execution errors arrive in `response.errors`, not as a failed envelope.

## Subscriptions

```c
exec(db, "{\"op\":\"graphql_subscribe\",\"query\":\"subscription { queryPost { title } }\",\"poll_ms\":100}");
/* Response: {"ok":true,"id":1,"response":{...}} */
exec(db, "{\"op\":\"graphql_next\",\"id\":1,\"wait_ms\":1000}");
exec(db, "{\"op\":\"graphql_unsubscribe\",\"id\":1}");
```

`graphql_next` returns `{changed, closed, response?}`. Close subscriptions when
done.

## Request and response shapes

| `op` | Request | Success response |
|---|---|---|
| `dql_query` | `query` | `{ok:true,data:...}` |
| `dql_mutate` | `query` | `{ok:true,uids:{...}}` |
| `dql_alter` | `schema` | `{ok:true}` |
| `set_graphql_schema` | `sdl` | `{ok:true}` |
| `graphql` | `query`, `operation?`, `variables?`, `headers?` | `{ok:true,response:{data,errors}}` |
| `graphql_subscribe` | `query`, `variables?`, `headers?`, `poll_ms?` | `{ok:true,id,response}` |
| `graphql_next` | `id`, `wait_ms?` | `{ok:true,changed,closed,response?}` |
| `graphql_unsubscribe` | `id` | `{ok:true,removed}` |

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

## Limitations in 0.2.1

- Embedded graph mode is single-node; clustering, ACL, and namespaces require
  the standalone servers.
- DQL alters are additive/updating; drop operations and upserts are not exposed.
- GraphQL subscriptions are poll-based; WebSocket push is not exposed.

See [DQL and GraphQL](queries.md) for pattern filters, facets, and subscription
shapes. The separate [low-level key/value API](key-value.md) is available for
hosts that need raw storage. A complete reference consumer lives at
`bindings/c/smoke.c`.
