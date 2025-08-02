SHELL := /bin/bash

clean:
	cargo clean

release:
	cargo clippy -- -D warnings && cargo test --release && cargo build --release

debug:
	cargo build

test:
	cargo test

lint:
	cargo clippy

format:
	cargo fmt
