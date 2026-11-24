# TypeScript embedding

```sh
npm install @datastreamdb/core
```

```js
// ESM
import { Instance, version } from '@datastreamdb/core'

// CommonJS
// const { Instance, version } = require('@datastreamdb/core')
```

## Open an instance

```js
import { Instance, version } from '@datastreamdb/core'

console.log(version()) // "0.1.0"

const memory = new Instance('{}')
const persistent = new Instance(JSON.stringify({
  data_dir: '/var/lib/app/graph-a',
  tenant: 'app',
}))

console.log(memory.id(), memory.backend())      // uuid, 'memory'
console.log(persistent.id(), persistent.backend()) // uuid, 'fjall'
```

- `{}` opens an isolated in-memory instance.
- `data_dir` selects the persistent fjall backend and takes an exclusive lock.
- `tenant` is a label, not an isolation boundary.

## Supported types

| API | Type | Notes |
|---|---|---|
| `put(key, value)` | `string` | UTF-8 key and value. |
| `get(key)` | `string \| null` | `null` when missing. |
| `execute(request)` | JSON string | Envelope values are strings only. |
| `scan` result | `{ key, value }[]` | Values are UTF-8 strings. |
| JSON document | `JSON.stringify` / `JSON.parse` | No native object values. |
| Binary | base64 string | The native API does not accept `Buffer` values. |

## Store and read a JSON document

```js
const user = { name: 'Ada', age: 36, active: true }

db.put('user:1', JSON.stringify(user))

const raw = db.get('user:1')
const decoded = raw === null ? null : JSON.parse(raw)
console.log(decoded.name, decoded.age) // Ada 36
```

Use a key prefix for collections: `user:1`, `user:2`, `post:1`.

## Query with prefix scans

```js
const response = JSON.parse(db.execute(JSON.stringify({
  op: 'scan',
  prefix: 'user:',
  limit: 100,
})))

for (const { key, value } of response.pairs) {
  const user = JSON.parse(value)
  console.log(key, user.name)
}
```

- Keys come back in lexicographic order.
- `limit` defaults to 1000 and is clamped to 100,000.
- There is no cursor; use a narrower prefix or a larger limit to page.
- Zero-pad numeric ids when order matters (`user:0001`).

## Atomic writes

```js
const result = JSON.parse(db.execute(JSON.stringify({
  op: 'batch',
  puts: [
    { key: 'user:1', value: JSON.stringify({ name: 'Ada' }) },
    { key: 'user:2', value: JSON.stringify({ name: 'Bob' }) },
  ],
  deletes: ['user:3'],
})))

console.log(result.applied) // 3
```

The batch is atomic. `delete` is idempotent.

## Delete and flush

```js
db.execute(JSON.stringify({ op: 'delete', key: 'user:1' }))
db.execute('{"op":"flush"}')
```

`flush` makes prior writes durable. Dropping the last reference also flushes,
but call `flush` explicitly when the result matters.

## Execute the envelope

All operations are available through `execute`:

```js
const ping = JSON.parse(db.execute('{"op":"ping"}'))
console.log(ping.ok, ping.backend) // true 'memory'

const value = JSON.parse(db.execute(JSON.stringify({
  op: 'get',
  key: 'user:1',
})))
console.log(value.value) // string or null
```

Success responses:

| `op` | Response |
|---|---|
| `ping` | `{ ok, instance, tenant, backend }` |
| `put`, `delete`, `flush` | `{ ok: true }` |
| `get` | `{ ok: true, value: string \| null }` |
| `scan` | `{ ok: true, pairs: [{ key, value }] }` |
| `batch` | `{ ok: true, applied: number }` |

## Errors

The binding throws a JS `Error` for malformed requests, unknown ops, invalid
configs, locked directories, and storage failures:

```js
try {
  db.execute('{"op":"nope"}')
} catch (err) {
  console.error('datastreamdb:', err.message)
}
```

## Threading

Blocking engine calls run off the JS thread. Use one instance per JS
thread/worker; open a separate `Instance` inside each worker rather than
sharing a native object across isolates.

## Limitations in 0.1.0

- Embedded mode is key/value only. DQL and GraphQL run against the standalone
  servers.
- Values are strings; store binary data as base64.
- Scans are bounded and have no continuation cursor.
