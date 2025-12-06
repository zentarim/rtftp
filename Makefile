SHELL := /bin/bash
CPU_CORES := $(shell nproc)
MAX_TEST_THREADS := 10
TEST_THREADS := $(shell echo $$(( $(CPU_CORES) < $(MAX_TEST_THREADS) ? $(CPU_CORES) : $(MAX_TEST_THREADS) )))
.DEFAULT_GOAL := all

all: lint test release

clean:
	cargo clean

debug:
	cargo build

release:
	cargo build --release

test:
	cargo test --release -- --test-threads ${TEST_THREADS}

lint:
	cargo clippy -- -D warnings

format:
	cargo fmt
