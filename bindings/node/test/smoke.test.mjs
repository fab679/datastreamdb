import { describe, it } from 'node:test'
import assert from 'node:assert/strict'

let addon
try {
  addon = (await import('../index.js')).default ?? (await import('../index.js'))
} catch (err) {
  console.log('SKIP: build the addon first (npx napi build --platform):', err.message)
  process.exit(0)
}
const { Instance, version } = addon

describe('datastreamdb-node', () => {
  it('reports a version', () => {
    assert.match(version(), /^\d+\.\d+\.\d+/)
  })

  it('roundtrips put/get through two isolated instances', () => {
    const a = new Instance('{}')
    const b = new Instance('{}')
    assert.notEqual(a.id(), b.id())
    a.put('name', 'Ada')
    assert.equal(a.get('name'), 'Ada')
    assert.equal(b.get('name'), null)
  })

  it('executes the JSON envelope', () => {
    const db = new Instance('{}')
    const res = JSON.parse(db.execute('{"op":"ping"}'))
    assert.equal(res.ok, true)
    assert.equal(res.instance, db.id())
  })

  it('runs embedded DQL relationship queries', () => {
    const db = new Instance('{}')
    db.dqlAlter('name: string @index(exact) .\nfriend: [uid] @reverse .\ntype Person {\n name\n friend\n}')
    db.dqlMutate('{ set { _:a <name> "Ada" .\n_:b <name> "Bob" .\n_:a <friend> _:b .\n} }')
    const result = db.dqlQuery('{ people(func: eq(name, "Ada")) { name friend { name } } }')
    assert.match(result, /Ada/)
    assert.match(result, /Bob/)
  })

  it('runs embedded GraphQL and subscriptions', () => {
    const db = new Instance('{}')
    db.setGraphqlSchema(`
      type Post @withSubscription {
        id: ID!
        title: String @search(by: [term])
      }
    `)
    const added = JSON.parse(db.graphql('mutation { addPost(input: [{title: "Hello"}]) { numUids } }'))
    assert.equal(added.data.addPost.numUids, 1)

    const sub = db.graphqlSubscribe('subscription { queryPost { title } }')
    const initial = JSON.parse(db.graphqlNext(sub, 0))
    assert.equal(initial.changed, true)

    db.graphql('mutation { addPost(input: [{title: "Second"}]) { numUids } }')
    const changed = JSON.parse(db.graphqlNext(sub, 1000))
    assert.equal(changed.changed, true)
    assert.equal(db.graphqlUnsubscribe(sub), true)
  })
})
