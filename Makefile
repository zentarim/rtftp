SHELL := /bin/bash
CPU_CORES := $(shell nproc)
.DEFAULT_GOAL := all

all: lint test release

clean:
	cargo clean

debug:
	cargo build

release:
	cargo build --release

test:
	cargo test --release -- --test-threads ${CPU_CORES}

lint:
	cargo clippy -- -D warnings

format:
	cargo fmt
