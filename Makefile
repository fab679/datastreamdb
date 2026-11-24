# datastreamdb developer entry points (mirrors datastreamdb's Makefile workflow).
.PHONY: build test lint fmt clippy doc bindings conformance clean help

help:
	@echo "build       cargo build --workspace"
	@echo "test        cargo test --workspace"
	@echo "lint        fmt --check + clippy -D warnings"
	@echo "doc         cargo doc --workspace --no-deps"
	@echo "bindings    build node (napi) + python (pyo3) artifacts"
	@echo "conformance embedded conformance suite (core + ffi)"
	@echo "clean       cargo clean"

build:
	cargo build --workspace

test:
	cargo test --workspace

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

lint: fmt clippy

doc:
	cargo doc --workspace --no-deps

conformance:
	cargo test -p datastreamdb-core -p datastreamdb-ffi -p datastreamdb-store

bindings:
	cargo build -p datastreamdb-node
	cargo build -p datastreamdb-py

clean:
	cargo clean
