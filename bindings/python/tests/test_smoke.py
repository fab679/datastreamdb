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
