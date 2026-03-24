# Aeglos OS — Architectural Specification v1.0

## Project Identity

- **Name**: Aeglos OS
- **Developer**: Aeglos Systems LLC
- **Classification**: AI-Native Operating System
- **Philosophy**: The AI is not an application. The AI is the operating system.

---

## Design Principles

1. **Simplicity** — Minimal, clean architecture. Every component earns its place.
2. **AI Performance** — Local inference speed is a first-class optimization target. The kernel is designed around tensor operations as a core primitive.
3. **AI-Native** — AI is not bolted on. Intelligence is woven into the kernel, scheduler, memory system, and user interface from day one.
4. **No Legacy** — This is not a fork, not a derivative. Novel kernel, novel architecture, novel interaction model.

---

## Target Platform

- **Primary**: ARM / Apple Silicon (AArch64) — development and first boot target is Apple M4
- **Virtualization**: QEMU aarch64-virt machine for development testing
- **Future**: x86_64, RISC-V

---

## System Architecture

```
┌──────────────────────────────────────────────┐
│              AI Shell (Aska)                  │  User-facing intent interface
│         + Minimal Fallback GUI               │  Traditional UI when needed
├──────────────────────────────────────────────┤
│           Semantic Memory Layer              │  Unified knowledge graph
│        (replaces traditional filesystem)     │  Content-addressable + tagged
├────────────────────┬─────────────────────────┤
│    AI Runtime      │    App Sandbox          │  Local inference engine
│   (Numenor)        │    (WASM-based)         │  Isolated third-party apps
├────────────────────┴─────────────────────────┤
│              Aeglos Microkernel               │  Message-passing IPC
│     Memory / Scheduler / AI Syscalls         │  Capability-based security
├──────────────────────────────────────────────┤
│         Hardware Abstraction Layer            │  ARM64 / Apple Silicon
│        (Framebuffer, Storage, Input)         │  GPU/NPU acceleration
└──────────────────────────────────────────────┘
```

---

## Component Specifications

### 1. Aeglos Microkernel

**Language**: Rust (`no_std`, bare metal)
**Architecture**: Microkernel with message-passing IPC

The kernel is intentionally minimal. It manages:

- **Memory**: Virtual memory management, page allocation, memory-mapped I/O
- **Processes**: Lightweight task scheduling with AI-aware priority
- **IPC**: Asynchronous message-passing between all system services
- **AI Syscalls**: First-class system calls for tensor operations and inference requests
- **Capabilities**: Capability-based security model (no root/admin, only capabilities)

#### AI Syscalls (Novel)

These are kernel-level primitives, not userspace libraries:

| Syscall | Description |
|---------|-------------|
| `ai_infer(model, input)` | Run inference on a loaded model |
| `ai_load(model_path)` | Load a model into kernel-managed memory |
| `ai_unload(model_id)` | Release model resources |
| `ai_embed(data)` | Generate embedding vector for semantic operations |
| `ai_query(context, prompt)` | High-level query against the semantic memory |
| `ai_schedule_hint(task, priority)` | AI-informed scheduling hint |

#### Scheduler

The process scheduler is itself a lightweight learned model:

- Observes task patterns (CPU burst length, I/O waits, user interaction timing)
- Predicts optimal scheduling decisions rather than using fixed algorithms
- Falls back to round-robin if the learned model is unavailable or uncertain
- Prioritizes AI inference tasks when the user is actively interacting with the AI shell

#### Boot Sequence

1. Firmware/bootloader hands off to kernel entry point
2. Kernel initializes memory management (page tables for AArch64)
3. Kernel initializes interrupt handling (GIC on ARM)
4. Kernel starts IPC subsystem
5. Kernel launches AI Runtime service (Numenor)
6. Numenor loads base model into memory
7. Kernel launches Semantic Memory service
8. Kernel launches AI Shell (Aska)
9. System is ready — user sees Aska interface

---

### 2. Numenor — AI Runtime

**Purpose**: The system's inference engine, running as a privileged kernel service.

- Executes local LLM inference (GGUF/GGML format models)
- Manages GPU/NPU acceleration on Apple Silicon (Metal/ANE when available)
- Exposes inference to all system components via IPC
- Manages model loading, unloading, and memory allocation for model weights
- Supports multiple concurrent model contexts

#### Model Tiers

| Tier | Size | Purpose |
|------|------|---------|
| System | ~1-3B params | Always loaded. Handles shell commands, quick classification, routing |
| Standard | ~7-13B params | On-demand. Complex reasoning, content generation, analysis |
| Extended | External API | Optional cloud fallback for tasks exceeding local capability |

With 16GB unified memory on M4, budget approximately:
- 4GB for kernel + system services
- 8GB for AI models (fits a Q4-quantized 7B comfortably, or a 3B always-resident + 7B on-demand)
- 4GB for apps and user data

---

### 3. Semantic Memory Layer

**Purpose**: Replaces the traditional hierarchical filesystem with a knowledge-aware storage system.

Every piece of data in Aeglos OS is:
- **Content-addressable**: Identified by hash, not path
- **Semantically tagged**: Automatically embedded and indexed by meaning
- **Queryable by intent**: "Find the document I was working on about tactical gear pricing" works as a query
- **Versioned**: Every change is tracked, every state is recoverable

#### Structure

```
┌─────────────────────────────────┐
│        Query Interface          │  Natural language + structured queries
├─────────────────────────────────┤
│      Semantic Index             │  Vector embeddings of all content
├─────────────────────────────────┤
│      Metadata Store             │  Tags, timestamps, relationships, types
├─────────────────────────────────┤
│      Content Store              │  Content-addressable blob storage
├─────────────────────────────────┤
│      Block Device Driver        │  Raw storage interface
└─────────────────────────────────┘
```

#### Compatibility

- A POSIX-like translation layer allows traditional file paths to map into the semantic store
- Apps that expect `/home/user/documents/report.pdf` can still function
- But native Aeglos apps use semantic queries instead of paths

---

### 4. Aska — AI Shell

**Name meaning**: Aska (Old Norse: "ash tree" — the world tree)

**Purpose**: The primary user interface. Not a terminal emulator. Not a desktop. A conversational, intent-based interaction layer.

#### Interaction Model

The user expresses intent. Aska decomposes it into actions.

```
User: "Draft a quote for the MLok rail covers, 500 units, and send it to the manufacturer"

Aska:
  1. Queries Semantic Memory for MLok rail cover specs and pricing
  2. Generates quote document using template patterns
  3. Identifies manufacturer contact from relationship graph
  4. Prepares email draft
  5. Presents to user for review before sending
```

#### Visual Design

- **Default**: Full-screen conversational interface with minimal chrome
- **Fallback GUI**: Triggered by user preference or when visual content requires spatial layout
  - Lightweight tiling window manager
  - No desktop icons, no taskbar, no start menu
  - Windows appear when needed and are managed by Aska
- **Always visible**: Subtle system status bar (battery, network, time, AI model status)

#### Shell Commands (Natural Language)

No command syntax to memorize. But power users can use structured commands:

```
> open [semantic query]          — find and open content
> create [type] [description]    — generate new content
> connect [service/device]       — network and peripheral management
> system [status/config/update]  — system management
> help [topic]                   — contextual help
```

---

### 5. App Sandbox (WASM)

**Purpose**: Run third-party applications safely.

- All third-party code runs in WebAssembly sandboxes
- Apps receive capabilities explicitly granted by the user via Aska
- No app can access hardware, network, or storage without a capability grant
- Native Aeglos apps can use AI syscalls; sandboxed apps get a filtered IPC interface

---

## Development Phases

### Phase 1: Boot (Milestone: "First Light")
- [x] Rust bare-metal project structure for AArch64
- [x] Boot on QEMU virt machine
- [x] Initialize UART for serial output
- [x] Display "Aeglos OS" on framebuffer
- [x] Basic memory management (page allocator)
- [x] Interrupt handling (ARM GIC)

### Phase 2: Kernel Core (Milestone: "Foundation")
- [ ] Virtual memory with page tables
- [ ] Process/task management
- [ ] Context switching
- [ ] IPC message-passing system
- [ ] Basic syscall interface
- [ ] Timer and preemptive scheduling

### Phase 3: AI Runtime (Milestone: "Awakening")
- [ ] Integrate lightweight inference engine
- [ ] AI syscall interface
- [ ] Load and run a small model (~1B parameter)
- [ ] Embedding generation for semantic indexing
- [ ] Memory management for model weights

### Phase 4: Semantic Memory (Milestone: "Memory")
- [x] Content-addressable storage on virtio-blk
- [x] Metadata indexing
- [ ] Vector store for embeddings
- [x] Natural language query interface
- [ ] POSIX compatibility shim

### Phase 5: AI Shell (Milestone: "Voice")
- [ ] GPU framebuffer rendering
- [ ] Text rendering and input handling
- [ ] Conversational UI layout
- [ ] Intent parsing and action decomposition
- [ ] Fallback tiling window manager

### Phase 6: Integration (Milestone: "Aeglos 0.1")
- [ ] All components communicating via IPC
- [ ] Full boot-to-shell pipeline
- [ ] WASM sandbox for third-party apps
- [ ] Basic networking (virtio-net)
- [ ] System runs a complete user interaction loop

---

## File Structure

```
aeglos/
├── kernel/
│   ├── src/
│   │   ├── main.rs              — Entry point
│   │   ├── boot.rs              — AArch64 boot sequence
│   │   ├── memory/              — Page allocator, virtual memory
│   │   ├── process/             — Task management, scheduler
│   │   ├── ipc/                 — Message passing
│   │   ├── syscall/             — Syscall handlers (including AI syscalls)
│   │   ├── drivers/             — UART, framebuffer, virtio
│   │   └── arch/                — AArch64-specific code
│   ├── Cargo.toml
│   └── aarch64.json             — Custom target spec
├── numenor/                     — AI Runtime service
│   ├── src/
│   │   ├── engine.rs            — Inference engine
│   │   ├── model.rs             — Model loading/management
│   │   └── ipc.rs               — Kernel communication
│   └── Cargo.toml
├── semantic/                    — Semantic Memory service
│   ├── src/
│   │   ├── store.rs             — Content-addressable storage
│   │   ├── index.rs             — Vector index
│   │   └── query.rs             — Natural language query engine
│   └── Cargo.toml
├── aska/                        — AI Shell
│   ├── src/
│   │   ├── shell.rs             — Main shell logic
│   │   ├── render.rs            — Framebuffer rendering
│   │   ├── intent.rs            — Intent parser
│   │   └── gui.rs               — Fallback tiling WM
│   └── Cargo.toml
├── docs/
│   └── SPEC.md                  — This document
└── tools/
    └── build.sh                 — Build and run in QEMU
```

---

## Naming Convention

All major components use Tolkien-inspired names consistent with the Aeglos brand:

| Component | Name | Reference |
|-----------|------|-----------|
| Kernel | **Aeglos** | The spear of Gil-galad ("snow-point") |
| AI Runtime | **Numenor** | The great island of Men — the AI's domain |
| Process/Task Manager | **Remoboth** | Sindarin "netted stars" — a constellation of tasks |
| Semantic Memory | **Ithildin** | Moon-letters — hidden knowledge revealed by the right query |
| AI Shell | **Aska** | Old Norse for ash tree (Yggdrasil) |

---

## Build & Run

```bash
# Build the kernel
cd aeglos/kernel
cargo build --target aarch64.json --release

# Run in QEMU
qemu-system-aarch64 \
  -machine virt \
  -cpu cortex-a72 \
  -m 512M \
  -nographic \
  -kernel target/aarch64/release/aeglos
```

---

## Claude Code Usage

When starting a Claude Code session for Aeglos OS development:

1. Always reference this spec: `cat docs/SPEC.md`
2. State which phase/milestone you're working on
3. Ask Claude Code to implement specific components from the file structure
4. Test every change in QEMU before moving to the next component
5. Commit working states frequently with `git commit`

Example session opener:
```
"I'm working on Aeglos OS. Read docs/SPEC.md for the full architecture.
Current phase: Phase 1 (Boot). Implement the AArch64 boot sequence
that initializes UART and prints 'Aeglos OS' to serial output.
Target: QEMU virt machine."
```

---

*Aeglos OS — An AI-native operating system by Aeglos Systems LLC*
*"Not an AI on an OS. The AI IS the OS."*
