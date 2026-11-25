"""Smoke tests for the Python bindings (needs `maturin develop`)."""

import json

import pytest

datastreamdb = pytest.importorskip("datastreamdb")


def test_version():
    assert datastreamdb.version().count(".") == 2


def test_isolated_instances():
    a = datastreamdb.Instance("{}")
    b = datastreamdb.Instance("{}")
    assert a.id() != b.id()
    a.put("name", "Ada")
    assert a.get("name") == "Ada"
    assert b.get("name") is None


def test_execute_envelope():
    db = datastreamdb.Instance("{}")
    res = json.loads(db.execute('{"op":"ping"}'))
    assert res["ok"] is True
    assert res["instance"] == db.id()


def test_dql_relationship_query():
    db = datastreamdb.Instance("{}")
    db.dql_alter("name: string @index(exact) .\nfriend: [uid] @reverse .\ntype Person {\n name\n friend\n}")
    db.dql_mutate('{ set { _:a <name> "Ada" .\n_:b <name> "Bob" .\n_:a <friend> _:b .\n} }')
    result = db.dql_query('{ people(func: eq(name, "Ada")) { name friend { name } } }')
    assert "Ada" in result
    assert "Bob" in result


def test_graphql_and_subscription():
    db = datastreamdb.Instance("{}")
    db.set_graphql_schema(
        """
        type Post @withSubscription {
          id: ID!
          title: String @search(by: [term])
        }
        """
    )
    added = json.loads(db.graphql('mutation { addPost(input: [{title: "Hello"}]) { numUids } }'))
    assert added["data"]["addPost"]["numUids"] == 1

    sub = db.graphql_subscribe("subscription { queryPost { title } }")
    initial = json.loads(db.graphql_next(sub, 0))
    assert initial["changed"] is True

    db.graphql('mutation { addPost(input: [{title: "Second"}]) { numUids } }')
    changed = json.loads(db.graphql_next(sub, 1000))
    assert changed["changed"] is True
    assert db.graphql_unsubscribe(sub) is True
