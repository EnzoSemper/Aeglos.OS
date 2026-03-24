# Aeglos OS — Phase Zero Implementation Plan

> **Phase Zero** is the completion layer that closes all known gaps before
> the OS can be considered a credible AI-native platform.  Every item here
> has been assessed against the current codebase.  Status is one of:
> ✅ Done · 🟡 Partial · ❌ Missing.

---

## 1. Kernel / OS

| # | Item | Status | Notes |
|---|------|--------|-------|
| 1.1 | SMP work stealing | ✅ | `cpu_pin` affinity field + 3-phase work-steal in `schedule_next`: pinned→migratable→idle; Numenor pinned to CPU 1 |
| 1.2 | CSPRNG | ✅ | ChaCha20-DRBG seeded from ARMv8.5 RNDR (or timer-jitter); replaces TimerRng LCG in TLS |
| 1.3 | `mmap` / `munmap` | ✅ | `sys_mmap` allocates pages, zeroes them, maps EL0 RW via `map_user_rw`; `sys_munmap` walks L3 table, frees pages, per-VA `tlbi vaae1` flush |
| 1.4 | Signals / async EL0 notifications | ✅ | `pending_signals/signal_mask/signal_handler` per task; `SYS_SIGACTION(79)` registers EL0 trampoline; `SYS_KILL(80)` delivers; delivery on syscall return redirects ELR→handler with signum in x0, saved PC in LR; `SYS_SIGMASK(82)` |
| 1.5 | Pipe / FIFO | ✅ | `PipeBuf` 4KB ring × 8 pool; `VfsFd::Pipe`; `SYS_PIPE`(45) returns [read_fd, write_fd]; read/write/close wired in VFS |
| 1.6 | `fork` / `clone` | ✅ | `SYS_CLONE(83)`: spawns EL0 thread sharing caller's TTBR0; attenuated caps; fresh kernel stack; `spawn_user_thread()` in scheduler (no TLB flush between sibling threads) |

---

## 2. Networking

| # | Item | Status | Notes |
|---|------|--------|-------|
| 2.1 | HTTPS POST | ✅ | `tls_post()` + `SYS_HTTPS_POST` (syscall 50) + `post <url> <body>` shell command |
| 2.2 | WebSocket (RFC 6455) | ✅ | `GET /ws` upgrades to WS; SHA-1/base64 handshake; token streaming via ws_send_text |
| 2.3 | IPv6 | ✅ | Dual-stack: `IpAddr` enum (V4/V6); EUI-64 link-local; SLAAC via RA; NDP (NS/NA/RS/RA); ICMPv6 echo; DNS AAAA; TCP over IPv6 with pseudo-header checksum |
| 2.4 | EL0 TCP socket syscalls | ✅ | SYS_TCP_CONNECT/LISTEN/ACCEPT/WRITE/READ/WAIT_READABLE/CLOSE (60-66); `CAP_NET`; `nc`/`listen` shell commands |
| 2.5 | TLS cert verification | ✅ | ASN.1 DER parser + X.509 leaf cert extraction; CN/SAN hostname match (RFC 2818/6125, wildcards); validity window via PL031 RTC; wired into TLS handshake at HT_CERTIFICATE |

---

## 3. AI / Numenor

| # | Item | Status | Notes |
|---|------|--------|-------|
| 3.1 | Streaming inference | ✅ | Per-token IPC (AI_OP_TOKEN); Ash prints live; GUI chat panel shows partial text each frame |
| 3.2 | Conversation history / context window | ✅ | 4 KB rolling history in C++; injected into every call; `reset` clears it |
| 3.3 | Structured output / tool calling | ✅ | `[[TOOL:args]]` syntax; 11 tools (FETCH/POST/DNS/PING/LS/CAT/SAVE/MEM_*/STATS); 3-round feedback loop in Ash |
| 3.4 | Fast model C++ integration | ✅ | `llm_fast_infer`/`llm_fast_infer_streaming` fully implemented in `llm_engine.cpp` using `g_fast_model` (Qwen3-0.6B loaded from `model_fast.gguf`); `is_simple_query` routes ≤100-char non-complex prompts to fast path; build script downloads model if absent |
| 3.5 | Voice / TTS output via HDA | ✅ | `hda::speak(text)` + `SYS_SPEAK(71)`; two-formant vowel synthesis + triangle-wave consonants; `speak <text>` shell command |
| 3.6 | Embedding model hot-reload | ✅ | `llm_get_embedding_dim()` C++ FFI queries model at runtime; `AI_OP_RELOAD_EMB(30)` IPC replies with live dim; EMB_BUF sized to 4096 (max model); semantic store clips to 384 for on-disk stability |

---

## 4. Userspace / Shell (Ash)

| # | Item | Status | Notes |
|---|------|--------|-------|
| 4.1 | HTTPS POST from shell | ✅ | `post <url> <body>` dispatches to `SYS_HTTPS_POST(50)` |
| 4.2 | Shell pipes (`cmd1 \| cmd2`) | ✅ | capture-mode `print()` + `run_pipeline()` + `grep`/`head`/`wc` consumers; multi-stage chains work |
| 4.3 | Background jobs (`cmd &`) | ✅ | trailing `&` launches via `SYS_EXEC`; `JOB_TIDS` table; `jobs` lists, `wait <tid>` blocks; `BgExec` intent |
| 4.4 | Tab completion | ✅ | Prefix match against all 30 built-in commands; single match completes, multiple shows list |
| 4.5 | Arrow-key history (readline) | ✅ | Full readline: up/down history, left/right cursor, Home/End, Ctrl-A/E/K/U, DEL, Tab |
| 4.6 | EL0 raw socket API | ✅ | `nc`/`connect`/`listen` commands wired to `SYS_TCP_CONNECT/LISTEN/ACCEPT/WRITE/READ/WAIT_READABLE/CLOSE(60-66)`; `CAP_NET` gated |
| 4.7 | `env` / environment variables | ✅ | 32-slot store; `export KEY=VALUE`, `unset KEY`, `env`; `$VAR`/`${VAR}` expanded in every command before parsing; pre-seeded USER/HOME/SHELL/OS/ARCH |

---

## 5. Storage / Filesystem

| # | Item | Status | Notes |
|---|------|--------|-------|
| 5.1 | FAT32 subdirectory support | ✅ | `resolve()` walks all path components via `scan_dir(dir_cluster)`; `open()`, `open_write()`, `create()`, `readdir()`, `read_file_alloc()` all use `resolve()` — nested paths work |
| 5.2 | VFS unification layer | ✅ | VFS fd table routes `/proc/…`→ProcFS, `/mem/…`→MemFS, else→FAT32; `cat /proc/meminfo|version|uptime|net/ip|tasks|stats|cpuinfo` |
| 5.3 | Persistent configuration | ✅ | `config.rs` module: key=value store in `/config` on FAT32; `config::init()` at boot; `SYS_CONFIG_GET(74)` / `SYS_CONFIG_SET(75)`; up to 32 entries, values up to 128 chars |
| 5.4 | Atomic file writes | ✅ | `fat32::unlink()` + `fat32::rename(src, dst)`; `SYS_ATOMIC_WRITE(76)` writes to `<name>.t` then renames; `SYS_UNLINK(77)`, `SYS_RENAME(78)`; config flush uses atomic rename |

---

## 6. GUI / Aurora Compositor

| # | Item | Status | Notes |
|---|------|--------|-------|
| 6.1 | True multi-window compositor | ✅ | Pre-clipped blit (no per-pixel bounds checks); dirty-flag early-exit; resize/raise/lower syscalls (67-70); draw_order array fixes z-order-on-click |
| 6.2 | App manifest / launcher | ✅ | Desktop icon click → SYS_OPEN + SYS_READ_FD + SYS_EXEC(elf_buf); folder click → navigate FS panel; ELF magic check; launch toast overlay |
| 6.3 | Vector / TrueType font rendering | ✅ | `draw_char_share_tech_scaled(scale: usize)` + `draw_string_share_tech_scaled()` in ui.rs; integer 1–8× nearest-neighbour scale over antialiased alpha bitmaps; 2× variants already existed |
| 6.4 | AI chat panel token streaming | ✅ | Per-frame `SYS_TRY_RECV` drains tokens into `llm_buffer`; `draw()` renders partial buffer live each frame |
| 6.5 | Drag-and-drop | ✅ | Icon press → drag threshold (>4px) → ghost icon + drop-zone highlight; drop on terminal inserts `exec /name`; drop on AI dispatches `describe /name` |
| 6.6 | Clipboard | ✅ | `ctrl_down` tracking (codes 29/97); Ctrl+C copies `term_buffer` to 256-byte clipboard; Ctrl+V pastes clipboard into active input |

---

## 7. Security

| # | Item | Status | Notes |
|---|------|--------|-------|
| 7.1 | Real entropy source | ✅ | `csprng.rs`: RNDR (ARMv8.5-A) preferred; 1024-sample timer-jitter LFSR fallback; both feed ChaCha20-DRBG |
| 7.2 | TLS certificate verification | ✅ | See 2.5 — hostname + validity verification in `net/x509.rs`; leaf cert only (no CA chain; no ECDSA sig verify) |
| 7.3 | Capability inheritance policy | ✅ | `sys_exec`: `effective_caps = requested & caller_caps` — child cannot exceed parent's cap set; `sys_cap_grant`: grantor can only delegate caps it holds; `sys_wasm_load` attenuated likewise |
| 7.4 | Syscall allow-listing (seccomp-like) | ✅ | `syscall_filter: [u64; 2]` per task (128-bit bitmap); checked in `dispatch()` before cap gating; `SYS_FILTER_SET(72)` / `SYS_FILTER_GET(73)` [CAP_ALL required]; kernel tasks default all-ones |
| 7.5 | ASLR | ✅ | `aslr_slide()` in `elf.rs`: ET_DYN (PIE) binaries randomised across 896 MB, 64 KB-aligned slots (14336 positions); ET_EXEC binaries load at link-time addrs (no relocation); uses ChaCha20-DRBG |

---

## Priority Tiers

### Tier 1 — Implement Next (highest leverage for AI-native vision)

| Ref | Item | Reason |
|-----|------|--------|
| 3.1 | Streaming inference | Transforms the UX; tokens appear live instead of after a long pause |
| 3.2 | Conversation history | Enables multi-turn AI interactions; currently every prompt is cold-start |
| 1.2 | CSPRNG | TLS is untrustworthy without it; blocks all security-sensitive features |
| 2.1 | HTTPS POST | Required to call external AI APIs (Anthropic, OpenAI) from within the OS |
| 2.2 | WebSocket | Ties streaming inference to the HTTP server; enables real-time browser clients |

### Tier 2 — Near-term

| Ref | Item |
|-----|------|
| 1.1 | SMP work stealing |
| 2.4 | EL0 TCP socket syscalls |
| 3.3 | Structured output / tool calling |
| 4.1 | HTTPS POST from shell |
| 5.2 | VFS unification layer |
| 6.1 | Full multi-window compositor |

### Tier 3 — Polish & completeness

| Ref | Item |
|-----|------|
| 1.4 | Signals |
| 1.5 | Pipes |
| 4.2 | Shell pipes (`cmd1 \| cmd2`) | ✅ | capture-mode `print()` + `run_pipeline()` + `grep`/`head`/`wc` consumers; multi-stage chains work |
| 4.3 | Background jobs (`cmd &`) | ✅ | trailing `&` launches via `SYS_EXEC`; `JOB_TIDS` table; `jobs` lists, `wait <tid>` blocks |
| 4.4–4.5 | Tab completion, readline history | ✅ | Done previous session |
| 4.7 | Environment variables |
| 5.1 | FAT32 subdirectories |
| 5.3 | Persistent config |
| 6.2 | App launcher |
| 6.3 | Vector fonts |
| 7.4 | Syscall allow-listing |
| 7.5 | ASLR |

---

## Implementation Order (suggested)

```
1.2  CSPRNG                     ← unblocks trustworthy TLS
2.1  HTTPS POST                 ← unblocks external API calls
3.1  Streaming inference        ← unblocks 2.2 + 6.4
2.2  WebSocket                  ← real-time streaming to browsers
3.2  Conversation history       ← multi-turn AI
3.3  Tool calling               ← AI-driven OS control
1.1  SMP work stealing          ← inference on dedicated core
2.4  EL0 socket syscalls        ← Ash networking power
5.2  VFS layer                  ← unified storage
6.1  Compositor completion      ← production GUI
```

---

*Last updated: 2026-03-18*
