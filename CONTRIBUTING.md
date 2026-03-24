# Contributing to Aeglos OS

Thank you for your interest in Aeglos OS. This document covers everything you need to set up a development environment, build the system, and submit changes.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [Getting the source](#getting-the-source)
- [Fetching vendor dependencies](#fetching-vendor-dependencies)
- [Building](#building)
- [Running in QEMU](#running-in-qemu)
- [Project structure](#project-structure)
- [Code style](#code-style)
- [Submitting changes](#submitting-changes)
- [Component ownership](#component-ownership)

---

## Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| Rust (nightly) | see `rust-toolchain.toml` | Kernel + all Rust components |
| `rust-src` component | — | `build-std` for `no_std` kernel |
| `llvm-tools` component | — | `llvm-objcopy`, `llvm-ar` |
| QEMU | ≥ 7.0 | Virtualised development target |
| Clang / LLVM | ≥ 16 | C++ compilation for llama.cpp (Numenor) |
| Python 3 | ≥ 3.10 | Build tooling scripts |
| `hdiutil` / `asr` | macOS only | FAT32 disk image creation |
| `mkfs.fat` + `mtools` | Linux only | FAT32 disk image creation |

### macOS

```bash
brew install qemu llvm python3
rustup toolchain install nightly
rustup component add rust-src llvm-tools --toolchain nightly
```

### Linux (Debian/Ubuntu)

```bash
sudo apt install qemu-system-aarch64 clang llvm mtools dosfstools python3
rustup toolchain install nightly
rustup component add rust-src llvm-tools --toolchain nightly
```

---

## Getting the source

```bash
git clone https://github.com/EnzoSemper/Aeglos.OS.git
cd Aeglos.OS
```

---

## Fetching vendor dependencies

The AI inference engine (Numenor) depends on two vendored libraries that are too large to store in the repository. After cloning, fetch them manually:

```bash
# llama.cpp — GGML inference engine
git clone --depth 1 https://github.com/ggml-org/llama.cpp vendor/llama.cpp

# LLVM libc++ headers — required for C++20 compilation in no_std context
# Only the libc++ headers are used; you do not need the full LLVM build
git clone --depth 1 --filter=blob:none --sparse \
    https://github.com/llvm/llvm-project vendor/llvm-project
cd vendor/llvm-project
git sparse-checkout set libcxx/include
cd ../..
```

After fetching, the `numenor/build.rs` script will locate the vendor sources automatically at `../vendor/llama.cpp` and `../vendor/llvm-project`.

> **Note:** You do not need the vendor libraries to work on the kernel, aska, semantic, or userspace packages. Only Numenor requires them.

---

## Building

All build operations are orchestrated by `tools/build.sh` or the top-level `Makefile`:

```bash
# Full build (kernel + Ash + Installer)
make build
# or: ./tools/build.sh build

# Build only the kernel
cd kernel
cargo build --target aarch64-unknown-none.json --release \
    -Z build-std=core,alloc \
    -Z build-std-features=compiler-builtins-mem
```

### Build outputs

| File | Description |
|------|-------------|
| `kernel/target/aarch64-unknown-none/release/aeglos` | Kernel ELF (bootable) |
| `kernel/target/aarch64-unknown-none/release/aeglos.bin` | Flat binary (for bare-metal flash) |
| `userspace/ash/target/.../ash` | Ash shell EL0 binary (embedded in kernel) |

---

## Running in QEMU

```bash
# Text/serial mode (no GPU)
make run

# Framebuffer GUI mode (VirtIO GPU + audio)
make display

# Build and boot a UEFI ISO
make iso-run
```

QEMU invocation for manual testing:

```bash
qemu-system-aarch64 \
  -machine virt \
  -cpu cortex-a72 \
  -m 512M \
  -nographic \
  -kernel kernel/target/aarch64-unknown-none/release/aeglos \
  -drive file=drive.img,format=raw,if=virtio
```

A pre-built `drive.img` (8 GB) is required for full functionality including model inference. See the README for the disk layout. The `model.gguf` (Qwen3-8B) and `model_fast.gguf` (Qwen3-0.6B) must be embedded in the image via `tools/build.sh build`.

---

## Project structure

```
Aeglos.OS/
├── kernel/          Microkernel — AArch64, Rust no_std
│   └── src/
│       ├── arch/    CPU-specific: MMU, GIC, AIC, exceptions, timer
│       ├── drivers/ Hardware drivers: VirtIO, E1000, NVMe, HDA, PCIe, FB
│       ├── net/     Network stack: TCP, TLS 1.3, HTTP, WebSocket, DNS
│       ├── fs/      FAT32 + VFS
│       ├── process/ Scheduler, ELF loader, task management
│       ├── ipc/     Message-passing subsystem
│       ├── wasm/    WebAssembly interpreter
│       └── syscall/ All 85+ syscall handlers
├── numenor/         AI runtime — llama.cpp wrapper (Rust + C++)
├── aska/            AI shell library — intent parser, renderer, GUI
├── semantic/        Semantic memory — SHA-256 store, vector index
├── userspace/
│   ├── ash/         Ash shell EL0 binary (links aska)
│   ├── bootloader/  UEFI bootloader (aarch64-unknown-uefi)
│   └── installer/   System installer EL0 binary
├── docs/            Architecture spec, phase status
├── tools/           Build scripts, package server, utilities
└── website/         Marketing and business documents
```

---

## Code style

- **Rust:** standard `rustfmt` formatting. Run `cargo fmt` before committing.
- **Unsafe:** document every `unsafe` block with a comment explaining why it is sound.
- **No std:** all kernel code is `#![no_std]`. Do not add `std` dependencies to kernel, numenor, aska, or semantic packages.
- **Panics:** all `panic!` paths must produce a useful UART message before halting. The kernel uses `panic = "abort"`.
- **Naming:** follow the Tolkien naming convention for new major components (see README for the existing table).

---

## Submitting changes

1. Fork the repository and create a feature branch from `main`.
2. Make your changes. Keep commits focused — one logical change per commit.
3. Ensure your changes build and boot in QEMU before submitting.
4. Open a pull request against `main` with a description of what changed and why.
5. Reference any related issues in the PR description.

For security vulnerabilities, see [`SECURITY.md`](SECURITY.md) (do not open a public issue).

---

## Component ownership

| Component | Primary language | Key files |
|-----------|-----------------|-----------|
| Kernel | Rust (no_std) | `kernel/src/main.rs`, `kernel/src/syscall/mod.rs` |
| AI Runtime | Rust + C++ | `numenor/src/engine.rs`, `numenor/src/cpp/llm_engine.cpp` |
| AI Shell | Rust | `aska/src/shell.rs`, `aska/src/intent.rs` |
| Semantic Memory | Rust | `semantic/src/store.rs`, `semantic/src/index.rs` |
| Ash (EL0 binary) | Rust | `userspace/ash/src/main.rs` |
| Network stack | Rust | `kernel/src/net/tcp.rs`, `kernel/src/net/tls.rs` |
| Drivers | Rust | `kernel/src/drivers/` |

---

*Aeglos Systems LLC — aeglos.systems*
