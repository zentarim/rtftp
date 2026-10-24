SHELL := /bin/bash
CPU_CORES := $(shell nproc)
MAX_TEST_THREADS := 10
TEST_THREADS := $(shell echo $$(( $(CPU_CORES) < $(MAX_TEST_THREADS) ? $(CPU_CORES) : $(MAX_TEST_THREADS) )))
.DEFAULT_GOAL := all

RTFTP_TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
MUSL_TARGET := x86_64-unknown-linux-musl
STOREKEEPER_BIN := $(RTFTP_TARGET_DIR)/$(MUSL_TARGET)/release/rtftp-storekeeper
INITRAMFS := $(RTFTP_TARGET_DIR)/initramfs.cpio

all: lint test release

clean:
	cargo clean

debug:
	cargo build

release:
	cargo build --release

test: $(INITRAMFS)
	cargo test --release --workspace -- --test-threads ${TEST_THREADS}

bench-download:
	cargo test --release --test test_download_time -- --ignored --test-threads 1 --nocapture

lint:
	cargo clippy -- -D warnings

format:
	cargo fmt

storekeeper:
	rustup target add $(MUSL_TARGET)
	cargo build --release --target $(MUSL_TARGET) -p rtftp-storekeeper

$(STOREKEEPER_BIN):
	$(MAKE) storekeeper

$(INITRAMFS): scripts/mk-initramfs.sh $(STOREKEEPER_BIN)
	mkdir -p "$(dir $(INITRAMFS))"
	STOREKEEPER="$(STOREKEEPER_BIN)" OUTPUT="$(INITRAMFS)" bash scripts/mk-initramfs.sh

.PHONY: all clean debug release test bench-download lint format storekeeper
