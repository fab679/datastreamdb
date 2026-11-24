# TypeScript embedding

```sh
npm install @datastreamdb/core
```

```js
import { Instance, version } from '@datastreamdb/core'

console.log(version()) // "0.1.0"

const a = new Instance('{}')
const b = new Instance('{"tenant": "b"}')
console.log(a.id() !== b.id()) // true

a.put('name', 'Ada')
console.log(a.get('name'))     // 'Ada'
console.log(b.get('name'))     // null — isolated

const res = JSON.parse(a.execute('{"op":"ping"}'))
console.log(res.ok, res.backend) // true 'memory'
```

## API

- `new Instance(configJson)` — throws on invalid config or backend failure.
- `id(): string` — unique id assigned at open.
- `backend(): string` — `"memory"` or `"fjall"`.
- `execute(request: string): string` — JSON envelope, JSON response.
- `put(key, value)` / `get(key): string | null` — raw key access.
- `version(): string` — engine version.

Blocking engine calls run off the JS thread. Config is the same JSON
as [Embedding overview](index.md#configuration): `{}` for memory,
`{"data_dir": "..."}` for a persistent instance.
