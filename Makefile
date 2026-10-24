SHELL := /bin/bash
CPU_CORES := $(shell nproc)
MAX_TEST_THREADS := 10
TEST_THREADS := $(shell echo $$(( $(CPU_CORES) < $(MAX_TEST_THREADS) ? $(CPU_CORES) : $(MAX_TEST_THREADS) )))
.DEFAULT_GOAL := all

CLOUD_HYPERVISOR_REPO ?= git@github.com:zentarim/cloud-hypervisor.git
CLOUD_HYPERVISOR_REF ?= main
CLOUD_HYPERVISOR_SRC := third_party/cloud-hypervisor
RTFTP_TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
CH_TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(CLOUD_HYPERVISOR_SRC)/target)
CLOUD_HYPERVISOR_BIN := $(RTFTP_TARGET_DIR)/release/cloud-hypervisor
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

test: $(CLOUD_HYPERVISOR_BIN) $(INITRAMFS)
	cargo test --release --workspace -- --test-threads ${TEST_THREADS}

bench-download: $(CLOUD_HYPERVISOR_BIN) $(INITRAMFS)
	cargo test --release --test test_download_time -- --ignored --test-threads 1 --nocapture

lint:
	cargo clippy -- -D warnings

format:
	cargo fmt

cloud-hypervisor:
	mkdir -p third_party "$(dir $(CLOUD_HYPERVISOR_BIN))"
	if [ ! -d "$(CLOUD_HYPERVISOR_SRC)/.git" ]; then \
		git clone --branch "$(CLOUD_HYPERVISOR_REF)" "$(CLOUD_HYPERVISOR_REPO)" "$(CLOUD_HYPERVISOR_SRC)"; \
	fi
	git -C "$(CLOUD_HYPERVISOR_SRC)" fetch origin "$(CLOUD_HYPERVISOR_REF)"
	git -C "$(CLOUD_HYPERVISOR_SRC)" checkout --detach FETCH_HEAD
	cargo build --release --manifest-path "$(CLOUD_HYPERVISOR_SRC)/Cargo.toml" -p cloud-hypervisor
	if [ "$$(readlink -f "$(CH_TARGET_DIR)/release/cloud-hypervisor")" != "$$(readlink -f "$(CLOUD_HYPERVISOR_BIN)")" ]; then \
		cp "$(CH_TARGET_DIR)/release/cloud-hypervisor" "$(CLOUD_HYPERVISOR_BIN)"; \
	fi

$(CLOUD_HYPERVISOR_BIN):
	$(MAKE) cloud-hypervisor

storekeeper:
	rustup target add $(MUSL_TARGET)
	cargo build --release --target $(MUSL_TARGET) -p rtftp-storekeeper

$(STOREKEEPER_BIN):
	$(MAKE) storekeeper

$(INITRAMFS): scripts/mk-initramfs.sh $(STOREKEEPER_BIN)
	mkdir -p "$(dir $(INITRAMFS))"
	STOREKEEPER="$(STOREKEEPER_BIN)" OUTPUT="$(INITRAMFS)" bash scripts/mk-initramfs.sh

.PHONY: all clean debug release test bench-download lint format cloud-hypervisor storekeeper
