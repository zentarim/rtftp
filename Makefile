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
DEB_VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
DEB_ARCH := $(shell dpkg --print-architecture)

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
	git -C "$(CLOUD_HYPERVISOR_SRC)" remote set-url origin "$(CLOUD_HYPERVISOR_REPO)"
	if [ -n "$$(git -C "$(CLOUD_HYPERVISOR_SRC)" status --porcelain)" ]; then \
		echo "$(CLOUD_HYPERVISOR_SRC) has local changes; commit or discard them" >&2; \
		exit 1; \
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

deb: cloud-hypervisor storekeeper release
	rm -rf target/deb
	mkdir -p target/deb/DEBIAN target/deb/usr/bin target/deb/usr/libexec/rtftp
	cp "$(RTFTP_TARGET_DIR)/release/rtftp" target/deb/usr/bin/rtftp
	cp "$(CLOUD_HYPERVISOR_BIN)" target/deb/usr/libexec/rtftp/cloud-hypervisor
	cp "$(STOREKEEPER_BIN)" target/deb/usr/libexec/rtftp/rtftp-storekeeper
	cp scripts/mk-initramfs.sh target/deb/usr/libexec/rtftp/mk-initramfs.sh
	chmod 755 target/deb/usr/bin/rtftp target/deb/usr/libexec/rtftp/cloud-hypervisor \
		target/deb/usr/libexec/rtftp/rtftp-storekeeper target/deb/usr/libexec/rtftp/mk-initramfs.sh
	printf '%s\n' \
		'Package: rtftp' \
		'Version: $(DEB_VERSION)' \
		'Section: net' \
		'Priority: optional' \
		'Architecture: $(DEB_ARCH)' \
		'Maintainer: Konstantin Nigmatullin <rangolit@gmail.com>' \
		'Depends: cpio, zstd, xz-utils' \
		'Description: TFTP server for NBD disks' \
		' RTFTP boots a cloud-hypervisor VM per client and serves files over vsock.' \
		> target/deb/DEBIAN/control
	dpkg-deb --root-owner-group --build target/deb "target/rtftp_$(DEB_VERSION)_$(DEB_ARCH).deb"

.PHONY: all clean debug release test bench-download lint format cloud-hypervisor storekeeper deb
