SHELL := /bin/bash
.DEFAULT_GOAL := all

all: lint test release

clean:
	cargo clean

debug:
	cargo build

release:
	cargo build --release

test:
	cargo test --release

lint:
	cargo clippy -- -D warnings

format:
	cargo fmt
