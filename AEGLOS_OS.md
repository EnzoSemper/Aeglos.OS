# Aeglos OS — System Overview

**Developer:** Aeglos Systems LLC
**Version:** 0.1.0
**Architecture:** AArch64 (Apple Silicon · ARM v8 · QEMU)
**Language:** Rust (`no_std`) + C++ (inference engine)
**Classification:** AI-Native Operating System

---

## The Problem

Every operating system in use today was designed before large language models existed. AI has been retrofitted — as an application, as an API call, as a cloud service — onto an OS substrate that has no concept of inference, no concept of intent, and no concept of semantic meaning. The result is a stack of abstractions pointed in the wrong direction: you talk to an app, the app talks to an API, the API talks to a model, and eventually an answer finds its way back to you through the same pipe in reverse.

Aeglos OS removes the intermediaries. The AI is not an application running on the OS. The AI is the OS.

---

## Core Philosophy

**Intelligence as a kernel primitive.**
`ai_infer()`, `ai_embed()`, and `ai_schedule_hint()` are syscalls — the same category as `read()`, `write()`, and `fork()`. Inference is a first-class operation the kernel understands, schedules, and optimises.

**Intent over commands.**
The user interface (Aska) does not expose a command syntax. It exposes a conversation. Aska decomposes natural language intent into discrete kernel-level actions — file operations, network calls, inference requests — and executes them on behalf of the user.

**Semantic memory over hierarchical storage.**
Files are identified by content hash, not path. Every object stored in the system is automatically embedded and indexed by meaning. You query the memory layer with intent: *"the document about the supplier contract from last week"* — not `/home/user/documents/contracts/2026-03/supplier_v2_final_FINAL.pdf`.

**No legacy.**
This is not a Linux fork. Not a BSD derivative. Not a microkernel experiment bolted onto existing infrastructure. Every subsystem — kernel, network stack, filesystem, AI runtime, shell — was written from scratch.

---

## System Architecture

### Layer overview

```
╔═══════════════════════════════════════════════════════╗
║                  Aska — AI Shell                      ║
║  Conversational interface · Streaming token display   ║
║  Intent decomposition · Tiling compositor             ║
╠═══════════════════════════════════════════════════════╣
║             Ithildin — Semantic Memory                ║
║  SHA-256 content addressing · Vector embedding index  ║
║  Natural language queries · Metadata graph            ║
╠═════════════════════════╦═════════════════════════════╣
║  Numenor — AI Runtime   ║   WASM App Sandbox          ║
║  llama.cpp + Rust IPC   ║   Capability-gated          ║
║  Streaming inference    ║   Third-party isolation     ║
║  Dual-model routing     ║                             ║
╠═════════════════════════╩═════════════════════════════╣
║                Aeglos Microkernel                     ║
║  AI syscalls · Message-passing IPC                    ║
║  SMP scheduler · Capability security                  ║
║  TCP/IPv4/IPv6 · TLS 1.3 · WebSocket                  ║
║  FAT32 · VFS · Memory management · ASLR               ║
╠═══════════════════════════════════════════════════════╣
║         Hardware Abstraction — AArch64                ║
║  MMU · GIC/AIC · VirtIO · E1000 · NVMe · HDA · PCIe  ║
╚═══════════════════════════════════════════════════════╝
```

---

## The Kernel

### What it is

Aeglos is a custom AArch64 microkernel written entirely in Rust (`no_std`, no `unsafe` beyond hardware-necessary boundary crossings). It targets the ARM v8 / v8.5 instruction set with Apple Silicon (M1–M4) as the primary silicon target and QEMU `virt` as the development environment.

### What it does

The kernel manages six core concerns:

**1. Memory**
4-level AArch64 page tables with a TTBR0/TTBR1 virtual address split. User processes run in low VA space (TTBR0); the kernel occupies the high VA range (0xFFFF000000000000+). Each process has an independent TTBR0 table — no shared mappings, no Spectre-susceptible shared kernel mappings. `mmap`/`munmap` syscalls provide EL0 access to on-demand physical pages.

**2. Processes**
Preemptive SMP scheduler with per-CPU run queues and three-phase work stealing (pinned → migratable → idle steal). Numenor (the AI runtime) is pinned to CPU 1 to prevent inference preemption. `fork`/`clone` spawn EL0 threads sharing the caller's TTBR0, with attenuated capabilities. Full ASLR for PIE ELF binaries (14,336 position slots, ChaCha20-DRBG seeded).

**3. IPC**
Asynchronous message-passing is the only inter-process communication mechanism. No shared memory segments, no file descriptor passing (beyond pipes). Every system service communicates via typed IPC messages. This is the same mechanism used by Aska to talk to Numenor, Numenor to talk to the kernel's AI subsystem, and all user applications to access system resources.

**4. AI Syscalls**
AI inference is a syscall, not a library call:

| Syscall | Function |
|---------|----------|
| `ai_infer(model, input)` | Run inference on a loaded model |
| `ai_load(path)` | Load a GGUF model into kernel-managed memory |
| `ai_unload(model_id)` | Release model memory |
| `ai_embed(data)` | Generate an embedding vector |
| `ai_query(ctx, prompt)` | High-level query against the semantic memory layer |
| `ai_schedule_hint(task, priority)` | Signal the scheduler that this task is AI-bound |

**5. Capabilities**
No root. No admin. No setuid. Every task holds a capability bitmask. Capabilities are inherited at `fork`/`exec` — but a child process can never hold a capability its parent did not hold. Capabilities can be granted between processes (`CAP_GRANT`), but only from the grantor's own set. Revocation is instantaneous.

| Capability | Grants access to |
|------------|-----------------|
| `CAP_IO` | Block device read/write |
| `CAP_NET` | All network syscalls |
| `CAP_FB` | Framebuffer and input |
| `CAP_AI` | AI inference syscalls |
| `CAP_LOG` | Kernel log/introspection |
| `CAP_ALL` | Everything (kernel tasks only) |

**6. Security subsystem**
Each task also carries a 128-bit syscall filter bitmap. Any syscall not set in the bitmap is rejected before capability gating. This provides a seccomp-equivalent per-process syscall allowlist configurable from userspace (`SYS_FILTER_SET`).

### Network stack

The kernel includes a complete TCP/IPv4/IPv6 implementation — not lwIP, not a vendor library. Written from scratch:

- Full dual-stack: IPv4 and IPv6 on the same socket interface
- DHCP client, SLAAC (Stateless Address Autoconfiguration), NDP (Neighbour Discovery Protocol)
- TCP with connection state machine, retransmission, flow control
- TLS 1.3: X25519 key exchange, AES-256-GCM, HMAC-SHA256, HKDF, X.509 leaf certificate verification (CN/SAN, RFC 2818/6125, wildcard support)
- HTTP/1.1 (GET, POST), WebSocket (RFC 6455 with SHA-1/Base64 handshake)
- DNS A/AAAA resolution, ICMP, ICMPv6 echo

EL0 processes access the network stack via a typed syscall interface (`SYS_TCP_CONNECT`, `SYS_TCP_LISTEN`, `SYS_TCP_ACCEPT`, `SYS_TCP_READ`, `SYS_TCP_WRITE`, `SYS_TCP_CLOSE`), gated by `CAP_NET`.

---

## Numenor — The AI Runtime

Numenor is a privileged EL0 service that owns all model memory and all inference execution. It runs as an isolated process with `CAP_AI` and receives inference requests from any other process via IPC.

### Architecture

```
Aska (user intent)
     │  IPC message: AI_OP_INFER {prompt, session_id}
     ▼
Numenor (EL0, CAP_AI)
     │
     ├── is_simple_query(prompt)?
     │       YES → llm_fast_infer_streaming (Qwen3-0.6B)
     │       NO  → llm_infer_streaming (Qwen3-8B)
     │
     └── per-token IPC: AI_OP_TOKEN {text, done}
              │
              ▼
         Aska renders live
```

### Models

| Model | Size | Purpose |
|-------|------|---------|
| Qwen3-8B Q4_K_M | ~4.7 GB | Complex reasoning, multi-turn conversation, tool use |
| Qwen3-0.6B | ~400 MB | Fast-path routing — short queries, completions, classification |

The fast model remains resident at all times. The main model is loaded on demand. Fast-path detection uses heuristics on prompt length (≤100 chars) and absence of structural complexity markers.

### Tool calling

Numenor implements structured tool calling using a `[[TOOL:args]]` output syntax. When the model generates a tool invocation, Numenor intercepts it, executes the tool, injects the result into the conversation context, and continues inference. The process is transparent to the user and runs in a 3-round feedback loop.

Built-in tools:

| Tool | Action |
|------|--------|
| `FETCH <url>` | HTTP GET via kernel network stack |
| `POST <url> <body>` | HTTPS POST via TLS stack |
| `DNS <hostname>` | Resolve A/AAAA records |
| `PING <host>` | ICMP echo |
| `LS <path>` | List FAT32 directory |
| `CAT <path>` | Read file content |
| `SAVE <path> <data>` | Write file atomically |
| `MEM_STORE <key> <val>` | Store in semantic memory |
| `MEM_RECALL <query>` | Query semantic memory |
| `STATS` | System resource snapshot |
| `SPEAK <text>` | TTS output via HDA driver |

### Conversation history

Numenor maintains a 4KB rolling conversation buffer in the C++ layer. Every call to `llm_infer_streaming` or `llm_fast_infer_streaming` includes the full history. `AI_OP_RESET` clears it. This enables genuine multi-turn dialogue without any application-level session management.

---

## Ithildin — Semantic Memory

Traditional filesystems store data at paths. Ithildin stores data by meaning.

### Structure

```
Query Interface      ← "find the supplier contract from March"
      │
Semantic Index       ← 384-dimensional embedding vectors (L2 search)
      │
Metadata Store       ← tags, timestamps, content-type, relationships
      │
Content Store        ← SHA-256-addressed blobs
      │
Block Device         ← FAT32-backed, on NVMe or VirtIO block
```

Every object written to Ithildin is:
1. Hashed (SHA-256) — the hash is the canonical identifier
2. Embedded — Numenor generates a 384-dim vector at write time
3. Tagged — content-type, creation time, source process, user-assigned tags
4. Indexed — added to the vector index for L2 nearest-neighbour retrieval

### Querying

Queries can be:
- **Exact:** `mem_get(sha256_hash)` — direct content retrieval
- **Tag filter:** `mem_query(tags=["invoice", "march-2026"])`
- **Semantic:** `mem_query(text="supplier contract last month")` — embedding search

The POSIX compatibility shim maps traditional file paths to semantic store entries. Applications that expect `/home/user/documents/contract.pdf` continue to work. Native Aeglos applications bypass the path shim and query by intent.

---

## Aska — The AI Shell

Aska (Old Norse: *ash tree*, Yggdrasil) is the primary and only user interface of Aeglos OS. It is not a terminal emulator. It is not a desktop environment. It is a conversational operating environment built around a local LLM with direct access to every kernel capability.

### Interaction model

```
User: "Draft a quote for the rail covers, 500 units, send to the manufacturer"

Aska:
  [TOOL: MEM_RECALL supplier contact for rail covers]
  → Contact: Apex Machining, apex@manufacturer.com
  [TOOL: MEM_RECALL rail cover pricing template]
  → Template: loaded
  [Generate: quote document, Qwen3-8B]
  → Draft rendered, presented for review
  [Awaiting confirmation before sending]
```

Commands are intents. Aska decomposes them into sequences of tool calls, file operations, and inference steps — presenting the result, not the machinery.

### Structured command syntax

Power users can bypass the intent layer with explicit commands:

```
> open <semantic query>          Find and open content
> create <type> <description>    Generate new content
> connect <service>              Network and peripheral management
> system status                  OS resource snapshot
> system update                  Pull package updates
> jobs                           List background tasks
> wait <tid>                     Wait for background task
```

### Shell features

- Live per-token LLM output — tokens appear as they are generated, not after
- Full readline: history (up/down), cursor movement (left/right, Home/End, Ctrl-A/E/K/U), tab completion against all built-in commands
- Pipes (`cmd1 | cmd2`), background jobs (`cmd &`), output redirection
- Environment variables (`export`, `unset`, `$VAR` expansion in all commands)
- 30+ built-in commands covering filesystem, network, AI, system management

### Compositor

When visual content requires spatial layout, Aska activates its built-in tiling window manager:

- Multi-window GPU compositor with pre-clipped blit rendering and dirty-flag early-exit
- Window operations: create, resize, raise, lower (syscalls 67–70)
- Drag-and-drop: drop a file icon onto the terminal to execute it; drop onto the AI panel to describe it
- Clipboard: Ctrl+C copies terminal selection; Ctrl+V pastes into active input
- Vector/TrueType font rendering at 1–8× integer scale
- App launcher: click a directory entry to launch the ELF binary with capability inheritance

---

## Security Model

### No root

Root does not exist in Aeglos OS. There is no superuser, no setuid, no privilege escalation path. Every process — including system services — holds a bounded capability set. The kernel holds `CAP_ALL`; everything else holds a subset, attenuated at `exec`/`fork` time.

### Isolation layers

| Layer | Mechanism |
|-------|-----------|
| Process memory | Per-process TTBR0 page tables; no shared kernel VA mappings |
| Syscall surface | Per-task 128-bit filter bitmap; unchecked syscalls are immediately rejected |
| Capability gating | Every privileged operation requires an explicit `CAP_*` bit |
| Application sandboxing | Third-party code runs in WASM interpreter with explicit capability grants |
| ASLR | PIE binaries randomised across 14,336 slots, seeded by hardware CSPRNG |

### Cryptography

| Primitive | Implementation |
|-----------|----------------|
| RNG | ChaCha20-DRBG, seeded by ARMv8.5 `RNDR` (hardware) or timer-jitter LFSR fallback |
| TLS 1.3 | X25519 key exchange, AES-256-GCM, HMAC-SHA256, HKDF-SHA256 |
| Content addressing | SHA-256 (semantic store) |
| ELF loading | SHA-256 binary verification before execution |

---

## Drivers

| Driver | Device | Lines |
|--------|--------|-------|
| UART | PL011 / NS16550 | 105 |
| Framebuffer | Flat framebuffer (QEMU SimpleFB) | 127 |
| VirtIO GPU | Paravirtualised GPU | 508 |
| VirtIO Net | Paravirtualised NIC | 354 |
| VirtIO Input | Paravirtualised keyboard/mouse | 276 |
| VirtIO (transport) | VirtIO ring protocol | 375 |
| E1000 | Intel 82540EM GbE NIC | 495 |
| NVMe | NVMe SSD controller | 491 |
| HDA | Intel High Definition Audio (TTS) | 702 |
| PCIe | PCI Express bus enumeration | 293 |
| Compositor | Multi-window GPU compositor | 246 |
| USB PD | USB Power Delivery negotiation | 227 |

Platform detection (MIDR_EL1) selects GIC (generic AArch64) or AIC (Apple Silicon) at boot.

---

## Build System

The kernel is built as a standard Rust crate targeting a custom AArch64 bare-metal spec (`aarch64-unknown-none.json`). Numenor's C++ inference core is compiled via `build.rs` using the `cc` crate and linked as a static library.

```bash
# Full build + QEMU run
./tools/build.sh

# Kernel only
cd kernel && cargo build --target aarch64-unknown-none.json --release

# QEMU invocation
qemu-system-aarch64 -machine virt -cpu cortex-a72 -m 512M \
  -nographic -kernel kernel/target/aarch64-unknown-none/release/aeglos
```

The build script creates an 8GB FAT32 disk image with:
- Compiled binaries in the first partition
- Qwen3-8B model at offset 512 MB
- Qwen3-0.6B model at offset 6144 MB
- Semantic store at offset 7168 MB

---

## Development Status

**Phase 0: Complete**

All core subsystems are functional and communicating over IPC. The system boots to the Aska shell, runs streaming inference, executes tool calls, reads and writes the semantic store, and handles multi-window compositor input — all on QEMU virt and Apple Silicon hardware.

**Next milestones:**
- x86_64 architecture port
- UEFI bootloader (in progress, `userspace/bootloader/`)
- Package and application distribution infrastructure
- Aeglos Linux distribution (device-agnostic deployment path)

---

## Roadmap

### Aeglos OS (bare metal)
The bare-metal AArch64 build remains the platform for controlled hardware targets: custom Aeglos Systems devices, ARM server deployments, embedded intelligence applications. x86_64 and RISC-V ports are next.

### Aeglos Linux
A Linux-based distribution shipping Numenor, Aska, and Ithildin as the complete user-facing stack — on top of a Linux LTS kernel for broad hardware compatibility. This is the path to general-purpose desktop and server deployment. The AI-native syscalls pioneered in the bare-metal kernel will be implemented as a kernel subsystem in the Linux fork (Phase 2).

---

*Aeglos Systems LLC*
*"Not an AI on an OS. The AI is the OS."*
