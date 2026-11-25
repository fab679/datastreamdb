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

console.log(version()) // "0.2.1"

const memory = new Instance('{}')
const persistent = new Instance(JSON.stringify({
  data_dir: '/var/lib/app/graph-a',
  tenant: 'app',
}))

console.log(memory.id(), memory.backend())        // uuid, 'memory'
console.log(persistent.id(), persistent.backend()) // uuid, 'fjall'
```

- `{}` opens an isolated in-memory instance.
- `data_dir` selects the persistent fjall backend and takes an exclusive lock.
- `tenant` is a label, not an isolation boundary.

## DQL

```js
const db = new Instance('{}')

db.dqlAlter(`
  name: string @index(exact) .
  friend: [uid] @reverse .
  type Person {
    name
    friend
  }
`)

db.dqlMutate(`
  { set {
      _:ada <name> "Ada" .
      _:bob <name> "Bob" .
      _:ada <friend> _:bob .
  } }
`)

const result = JSON.parse(db.dqlQuery(`
  { people(func: eq(name, "Ada")) { name friend { name } } }
`))
console.log(result)
```

`dqlMutate` returns a JSON string of blank-node assignments. `dqlQuery`
returns JSON text; parse it before reading.

## GraphQL

```js
db.setGraphqlSchema(`
  type Post {
    id: ID!
    title: String @search(by: [term])
    author: Author @hasInverse(field: posts)
  }
  type Author {
    id: ID!
    name: String @id @search(by: [hash])
    posts: [Post]
  }
`)

const response = JSON.parse(db.graphql(`
  mutation {
    addPost(input: [{title: "Hello", author: {name: "Ann"}}]) {
      post { id title author { name } }
      numUids
    }
  }
`))
console.log(response)
```

GraphQL execution errors arrive in `response.errors`; invalid SDL or transport
failures throw a JS `Error`.

## Subscriptions

```js
const id = db.graphqlSubscribe(`
  subscription { queryPost { title } }
`)

let event = JSON.parse(db.graphqlNext(id, 0)) // initial response
db.graphql('mutation { addPost(input: [{title: "Hello again"}]) { numUids } }')
event = JSON.parse(db.graphqlNext(id, 1000))  // changed after the mutation
db.graphqlUnsubscribe(id)
```

`graphqlNext(id, waitMs)` blocks up to `waitMs` and returns
`{ changed, closed, response? }`. Close subscriptions when done.

## Errors

The binding throws a JS `Error` for malformed requests, invalid SDL, locked
directories, and engine failures:

```js
try {
  db.dqlQuery('{ q(func: has(name)) { name } }')
} catch (err) {
  console.error('datastreamdb:', err.message)
}
```

## Threading

Blocking engine calls run off the JS thread. Use one instance per JS
thread/worker; open a separate `Instance` inside each worker rather than
sharing a native object across isolates.

## Limitations in 0.2.1

- Embedded graph mode is single-node; clustering, ACL, and namespaces require
  the standalone servers.
- DQL alters are additive/updating; drop operations and upserts are not exposed.
- GraphQL subscriptions are poll-based; WebSocket push is not exposed.

See [DQL and GraphQL](queries.md) for pattern filters, facets, and subscription
shapes. The separate [low-level key/value API](key-value.md) is available for
hosts that need raw storage.
