# Aeglos OS — top-level build wrapper
# Delegates to tools/build.sh for all kernel/userspace compilation.
# Run `make help` to see available targets.

SHELL := /bin/bash
BUILD := ./tools/build.sh

.PHONY: all build run display iso iso-run clean check help

all: build

## Build the kernel and all userspace binaries
build:
	@$(BUILD) build

## Build and launch in QEMU (serial/text mode)
run:
	@$(BUILD) run

## Build and launch with VirtIO GPU framebuffer + audio
display:
	@$(BUILD) display

## Build a bootable ISO image
iso:
	@$(BUILD) iso

## Build ISO and immediately boot it in QEMU via UEFI
iso-run:
	@$(BUILD) iso_run

## Remove all build artefacts
clean:
	@cargo clean --manifest-path kernel/Cargo.toml      2>/dev/null || true
	@cargo clean --manifest-path numenor/Cargo.toml     2>/dev/null || true
	@cargo clean --manifest-path userspace/ash/Cargo.toml 2>/dev/null || true
	@cargo clean --manifest-path userspace/installer/Cargo.toml 2>/dev/null || true
	@cargo clean --manifest-path userspace/bootloader/Cargo.toml 2>/dev/null || true

## Run cargo check across the workspace (requires vendor deps — see CONTRIBUTING.md)
check:
	@cargo check --target kernel/aarch64-unknown-none.json \
	    -Z build-std=core,alloc \
	    -Z build-std-features=compiler-builtins-mem

## Print this help message
help:
	@echo ""
	@echo "  Aeglos OS Build System"
	@echo ""
	@echo "  Usage: make <target>"
	@echo ""
	@grep -E '^## ' Makefile | sed 's/## /    /'
	@echo ""
	@echo "  See tools/build.sh and CONTRIBUTING.md for full build documentation."
	@echo ""
