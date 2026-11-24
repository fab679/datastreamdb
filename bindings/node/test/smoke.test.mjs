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
})
