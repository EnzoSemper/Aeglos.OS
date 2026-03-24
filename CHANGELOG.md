# Changelog

All notable changes to Aeglos OS are documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [0.1.0] — 2026-03-23

Initial Phase 0 release. All core subsystems implemented and communicating over IPC. The system boots to the Aska shell, runs streaming LLM inference, executes structured tool calls, reads/writes the semantic store, and handles multi-window compositor input on QEMU virt and Apple Silicon hardware.

### Kernel

- Custom AArch64 microkernel (Rust `no_std`) with 85+ syscalls
- 4-level page tables with TTBR0/TTBR1 virtual address split (kernel at 0xFFFF000000000000+)
- Per-process TTBR0 isolation; no shared kernel VA mappings
- Preemptive SMP scheduler with three-phase work stealing (pinned → migratable → idle steal)
- AI-aware task pinning: Numenor pinned to CPU 1 to prevent inference preemption
- Message-passing IPC subsystem (async, typed messages)
- Capability-based security model: `CAP_IO`, `CAP_NET`, `CAP_FB`, `CAP_AI`, `CAP_LOG`, `CAP_ALL`
- Per-task 128-bit syscall filter bitmap (seccomp equivalent) via `SYS_FILTER_SET`/`SYS_FILTER_GET`
- Capability inheritance: child process cannot exceed parent's capability set
- ASLR: 14,336 position slots for PIE ELF binaries, seeded by ChaCha20-DRBG
- ChaCha20-DRBG CSPRNG seeded from ARMv8.5 `RNDR` hardware entropy (timer-jitter fallback)
- `fork`/`clone` (EL0 thread spawning with shared TTBR0)
- `mmap`/`munmap` (on-demand physical page mapping for EL0 processes)
- Pipes (`SYS_PIPE`, 4 KiB ring buffers), signals (`SYS_SIGACTION`, `SYS_KILL`, `SYS_SIGMASK`)
- Environment variables (32-slot store with `export`/`unset`, `$VAR` expansion)
- Persistent key-value configuration store with atomic write-rename (`SYS_ATOMIC_WRITE`)
- WASM interpreter for third-party application sandboxing
- Platform detection via MIDR_EL1: selects GIC (generic AArch64) or AIC (Apple Silicon)
- ELF binary loader with ASLR and capability inheritance at exec

### Network stack

- Full TCP/IPv4/IPv6 dual-stack implementation (written from scratch)
- DHCP client, SLAAC, NDP (NS/NA/RS/RA), ICMPv6, ARP
- TLS 1.3: X25519 key exchange, AES-256-GCM, HMAC-SHA256, HKDF-SHA256
- X.509 leaf certificate verification: CN/SAN hostname match, validity window, RFC 2818/6125 wildcards
- HTTP/1.1 GET and POST; HTTPS via TLS
- WebSocket (RFC 6455) with SHA-1/Base64 handshake and `ws_send_text`
- DNS A/AAAA resolution; ICMP/ICMPv6 echo
- EL0 TCP socket syscalls: `SYS_TCP_CONNECT`, `SYS_TCP_LISTEN`, `SYS_TCP_ACCEPT`, `SYS_TCP_READ`, `SYS_TCP_WRITE`, `SYS_TCP_CLOSE`

### Drivers

- UART: PL011 / NS16550 serial output
- Framebuffer: flat framebuffer (QEMU SimpleFB) and VirtIO GPU (paravirtualised)
- Multi-window GPU compositor with pre-clipped blit, dirty-flag early-exit
- VirtIO Net (paravirtualised NIC), VirtIO Input (keyboard/mouse)
- Intel E1000 GbE NIC
- NVMe SSD controller
- Intel HDA audio codec with two-formant TTS synthesis
- PCI Express bus enumeration
- USB Power Delivery negotiation
- Font rendering: vector/TrueType at 1–8× integer scale

### Filesystem

- FAT32 with full subdirectory support via `resolve()` path walker
- VFS unification: `/proc/`, `/mem/`, FAT32 under one fd namespace
- `procfs`: meminfo, version, uptime, net/ip, tasks, stats, cpuinfo
- Atomic file writes via write-to-temp + rename (`SYS_ATOMIC_WRITE`, `SYS_RENAME`, `SYS_UNLINK`)

### Numenor — AI Runtime

- llama.cpp C++ core wrapped in Rust IPC layer
- Dual-model routing: Qwen3-0.6B (fast path, always resident) + Qwen3-8B (main, on-demand)
- Fast-path detection: prompts ≤100 chars without structural complexity markers → 0.6B model
- Streaming per-token inference via IPC (`AI_OP_TOKEN` messages)
- 4 KiB rolling conversation history; `AI_OP_RESET` clears context
- Structured tool calling: `[[TOOL:args]]` output syntax with 3-round feedback loop
- 11 built-in tools: `FETCH`, `POST`, `DNS`, `PING`, `LS`, `CAT`, `SAVE`, `MEM_STORE`, `MEM_RECALL`, `STATS`, `SPEAK`
- Embedding generation via C++ FFI; embedding model hot-reload without restart (`AI_OP_RELOAD_EMB`)
- TTS voice output routed through `SYS_SPEAK` → HDA driver
- `__dso_handle` placement in `.rodata` for llama.cpp global constructor compatibility across TTBR1 VA split

### Ithildin — Semantic Memory

- SHA-256 content-addressable blob store
- Tag-based metadata store (content-type, timestamps, relationships, user tags)
- 384-dimension vector embedding index for semantic search
- Natural language query interface: keyword, tag filter, and L2 nearest-neighbour retrieval
- Block device abstraction backed by FAT32 partition
- Embedding dimension auto-detected at runtime via `llm_get_embedding_dim()` C++ FFI
- On-disk embedding vectors clipped to 384 dimensions for stable serialisation

### Aska — AI Shell + Compositor

- Conversational intent-based shell (not a terminal emulator)
- Live per-token LLM rendering: each `AI_OP_TOKEN` IPC message updates the chat panel mid-frame
- Multi-window GPU compositor: window create/resize/raise/lower via syscalls 67–70
- Drag-and-drop: file icon → terminal inserts `exec /name`; file icon → AI panel dispatches `describe /name`
- Clipboard: Ctrl+C copies `term_buffer`; Ctrl+V pastes into active input
- App launcher: ELF binary execution with ELF magic check and capability inheritance
- Full readline: up/down history, left/right cursor, Home/End, Ctrl-A/E/K/U, Tab completion
- Shell pipes (`cmd1 | cmd2`), background jobs (`cmd &`), `jobs`/`wait` builtins
- 30+ built-in commands: filesystem, network (`nc`, `connect`, `listen`, `post`), AI, system management
- `SYS_SPEAK(71)` — TTS output from Aska via HDA audio

### Userspace

- `ash` — EL0 shell binary linking the `aska` library; loads at VA 0x48000000
- `installer` — EL0 system installer binary with graphical UI via `aska`
- `bootloader` — UEFI bootloader (`aarch64-unknown-uefi`); in-progress

### Build system

- `tools/build.sh` — full build and QEMU run orchestration (macOS and Linux)
- FAT32 disk image creation with embedded model weights at fixed offsets
- Disk layout: FAT32 [0–512 MB] | Qwen3-8B [512–6144 MB] | Qwen3-0.6B [6144–6544 MB] | Semantic store [7168–7178 MB]
- GRUB ISO generation (Linux) and native UEFI bootloader path (macOS)
- UTM VM bundle generation for macOS

### Documentation

- `README.md` — full project overview, architecture, build instructions
- `AEGLOS_OS.md` — detailed OS system document
- `docs/SPEC.md` — architectural specification v1.0
- `docs/PHASE0.md` — Phase 0 implementation status table
- `CONTRIBUTING.md` — development setup, build guide, contribution process
- `CHANGELOG.md` — this file

---

*Aeglos Systems LLC — aeglos.systems*
