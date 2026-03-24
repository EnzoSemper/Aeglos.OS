/// System call interface.
///
/// Syscall numbers:
///  1: SEND (target_tid, msg_ptr)
///  2: RECV (msg_ptr) - blocks if empty
///  3: YIELD
///  4: EXIT (code)
///  5: AI_CALL
///  6: BLK_READ
///  7: BLK_WRITE
///  8: LOG
///  9..12: MALLOC / FREE / REALLOC / ALIGNED_ALLOC
/// 13: EXEC (path_ptr, path_len, caps) → tid
/// 14: NET_SEND
/// 15: NET_RECV
/// 16..20: OPEN / READ_FD / WRITE_FD / CLOSE / READDIR
/// 21: WAIT (tid) → exit_code

use crate::process::scheduler;
use crate::ipc::Message;

pub const SYS_SEND: usize = 1;
pub const SYS_RECV: usize = 2;
pub const SYS_YIELD: usize = 3;
pub const SYS_EXIT: usize = 4;
pub const SYS_AI_CALL: usize = 5;
pub const SYS_BLK_READ: usize = 6;
pub const SYS_BLK_WRITE: usize = 7;
pub const SYS_LOG: usize = 8;
pub const SYS_MALLOC: usize = 9;
pub const SYS_FREE: usize = 10;
pub const SYS_REALLOC: usize = 11;
pub const SYS_ALIGNED_ALLOC: usize = 12;
pub const SYS_EXEC: usize = 13;      // (elf_ptr, elf_len, caps) → tid
pub const SYS_NET_SEND: usize = 14;  // (buf_ptr, buf_len, 0) → 0 ok / -1 err
pub const SYS_NET_RECV: usize = 15;  // (buf_ptr, buf_len, 0) → bytes / -1 none
pub const SYS_OPEN: usize = 16;      // (path_ptr, path_len, 0) → fd or -1
pub const SYS_READ_FD: usize = 17;   // (fd, buf_ptr, len) → bytes or -1
pub const SYS_WRITE_FD: usize = 18;  // (fd, buf_ptr, len) → bytes or -1
pub const SYS_CLOSE: usize = 19;     // (fd, 0, 0) → 0
pub const SYS_READDIR: usize = 20;   // (path_ptr, path_len, out_ptr) → count
pub const SYS_WAIT: usize = 21;      // (tid, 0, 0) → exit_code (blocks)
pub const SYS_WASM_LOAD: usize = 22; // (wasm_ptr, wasm_len, caps) → tid or -1
pub const SYS_MMAP:      usize = 23; // (addr_hint, size, prot) → va or -1
pub const SYS_MUNMAP:    usize = 24; // (va, size, 0) → 0 or -1
pub const SYS_KREAD:     usize = 25; // (k_ptr, u_ptr, len) → bytes copied or -1
pub const SYS_FB_INFO:   usize = 26; // (out_w, out_h, out_pitch) -> 0
pub const SYS_FB_MAP:    usize = 27; // () -> va
pub const SYS_FB_FLUSH:  usize = 28; // () -> 0
pub const SYS_INPUT_POLL: usize = 29; // (out_events_ptr, max_events) -> n
pub const SYS_TRY_RECV:  usize = 30; // (msg_ptr) -> 0 ok / 1 empty  (non-blocking, never Blocks task)
pub const SYS_GET_RTC:   usize = 31; // () -> Unix epoch seconds (u32) from PL031 RTC
pub const SYS_GET_STATS: usize = 32; // (out16: *mut u32) -> 0  writes [free_mb, total_mb, cpu_pct, task_cnt]
pub const SYS_GET_IP:    usize = 33; // () -> returns IP as packed u32 in isize
pub const SYS_PING:      usize = 34; // (ip_packed_u32, timeout_ms) -> rtt_ms or -1
pub const SYS_HTTP_GET:   usize = 35; // (url_ptr, url_len, buf_ptr, buf_len) -> bytes or -err
pub const SYS_HTTPS_GET:  usize = 36; // same, TLS
pub const SYS_HTTPS_POST: usize = 50; // (url_ptr, url_len, body_ptr, body_len, buf_ptr, buf_len) -> bytes or -err
pub const SYS_DNS_RESOLVE: usize = 37; // (name_ptr, name_len, out_ip4_ptr) -> 0 or -1
pub const SYS_CREATE:      usize = 38; // (path_ptr, path_len, flags) → fd or -1
pub const SYS_PIPE:         usize = 45; // (out_fds_ptr) → 0 ok / -1; writes [read_fd:u32, write_fd:u32]
pub const SYS_SURF_CREATE:  usize = 40; // (width, height, z_order) → surface_id or -1
pub const SYS_SURF_DESTROY: usize = 41; // (surface_id, 0, 0) → 0
pub const SYS_SURF_MAP:     usize = 42; // (surface_id, 0, 0) → user VA of pixel buffer
pub const SYS_SURF_FLUSH:   usize = 43; // (surface_id, 0, 0) → 0 (composites all surfaces)
pub const SYS_SURF_MOVE:    usize = 44; // (surface_id, x, y) → 0
pub const SYS_SET_PRIORITY: usize = 50; // (tid, priority 1-8) → 0  [CAP_AI]
pub const SYS_WASM_EXEC:   usize = 51; // (path_ptr, path_len) → exit_code or -err
pub const SYS_CAP_GRANT:   usize = 52; // (tid, cap_bits) → 0 ok / -1 not found  [CAP_ALL]
pub const SYS_CAP_REVOKE:  usize = 53; // (tid, cap_bits) → 0 ok / -1 not found  [CAP_ALL]
pub const SYS_CAP_QUERY:   usize = 54; // (tid) → caps bitmask as usize, or -1
pub const SYS_LIST_TASKS:  usize = 55; // (out_ptr, max_entries) → count; writes TaskRecord × n [CAP_LOG]

// TCP socket syscalls — EL0 raw TCP access [CAP_NET]
pub const SYS_TCP_CONNECT:       usize = 60; // (ip_u32, port, timeout_ms) → conn_id ≥0 or -1
pub const SYS_TCP_LISTEN:        usize = 61; // (port, 0, 0) → listener_id ≥0 or -1
pub const SYS_TCP_ACCEPT:        usize = 62; // (listener_id, timeout_ms, 0) → conn_id ≥0 or -1
pub const SYS_TCP_WRITE:         usize = 63; // (conn_id, buf_ptr, len) → bytes written or -1
pub const SYS_TCP_READ:          usize = 64; // (conn_id, buf_ptr, len) → bytes read or -1
pub const SYS_TCP_WAIT_READABLE: usize = 65; // (conn_id, timeout_ms, 0) → 0 ok / -1 timeout/closed
pub const SYS_TCP_CLOSE:         usize = 66; // (conn_id, 0, 0) → 0

// Compositor control syscalls — Phase 6.1
pub const SYS_SURF_RESIZE: usize = 67; // (surface_id, new_w, new_h) → 0 ok / -1
pub const SYS_SURF_RAISE:  usize = 68; // (surface_id, 0, 0) → 0
pub const SYS_SURF_LOWER:  usize = 69; // (surface_id, 0, 0) → 0
pub const SYS_SURF_DIRTY:  usize = 70; // (surface_id, 0, 0) → 0  (mark dirty)
pub const SYS_SPEAK:       usize = 71; // (text_ptr, text_len, 0) → 0 ok / -1 (TTS via HDA)
pub const SYS_FILTER_SET:  usize = 72; // (tid, lo64, hi64) → 0 ok / -1; sets filter[0]=lo, filter[1]=hi  [CAP_ALL]
pub const SYS_FILTER_GET:  usize = 73; // (tid, out_ptr) → 0 ok / -1; writes [lo:u64, hi:u64] to out_ptr
pub const SYS_CONFIG_GET:   usize = 74; // (key_ptr, key_len, out_ptr, out_len) → bytes or -1
pub const SYS_CONFIG_SET:   usize = 75; // (key_ptr, key_len, val_ptr, val_len) → 0 or -1  [CAP_BLK]
pub const SYS_ATOMIC_WRITE: usize = 76; // (path_ptr, path_len, data_ptr, data_len) → 0 ok / -1  [CAP_BLK]
pub const SYS_UNLINK:       usize = 77; // (path_ptr, path_len, 0) → 0 ok / -1  [CAP_BLK]
pub const SYS_RENAME:       usize = 78; // (src_ptr, src_len, dst_ptr, dst_len) → 0 ok / -1  [CAP_BLK]

// fork/clone (1.6): user-initiated task creation
// SYS_CLONE: spawn a new EL0 task sharing the current process's page table.
// arg0 = entry_va  (EL0 function pointer — the new thread's entry point)
// arg1 = stack_top (top of the new thread's pre-allocated user stack)
// arg2 = caps      (capability mask for child; attenuated to parent's caps)
// Returns new TID ≥1 on success, -1 on failure.
pub const SYS_CLONE: usize = 83;

// Signal syscalls (1.4)
pub const SYS_SIGACTION: usize = 79; // (handler_va, mask_u32, 0) → 0 ok  — set EL0 signal trampoline
pub const SYS_KILL:      usize = 80; // (tid, sig_num) → 0 ok / -1 not found
pub const SYS_SIGRETURN: usize = 81; // (0, 0, 0) → (no return) — called by handler trampoline to resume
pub const SYS_SIGMASK:   usize = 82; // (mask_u32, 0, 0) → old_mask — set signal mask

// Signal numbers
pub const SIGTERM:  u32 = 0;
pub const SIGKILL:  u32 = 1;
pub const SIGUSR1:  u32 = 2;
pub const SIGUSR2:  u32 = 3;
pub const SIGCHLD:  u32 = 4;
pub const SIGALRM:  u32 = 5;

// TID for the Numenor service (Assuming it's always spawned as TID 1)
const NUMENOR_TID: usize = 1;

// ─── Capability bitmask constants ────────────────────────────────────────────
/// Capability to send IPC messages (SYS_SEND).
pub const CAP_SEND:  u64 = 1 << 0;
/// Capability to receive IPC messages (SYS_RECV).
pub const CAP_RECV:  u64 = 1 << 1;
/// Capability to access the block device (SYS_BLK_READ / SYS_BLK_WRITE).
pub const CAP_BLK:   u64 = 1 << 2;
/// Capability to invoke AI/Numenor via SYS_AI_CALL.
pub const CAP_AI:    u64 = 1 << 3;
/// Capability to write to the console (SYS_LOG, console_gets).
pub const CAP_LOG:   u64 = 1 << 4;
/// Capability to allocate kernel heap memory (SYS_MALLOC / SYS_FREE / etc.).
pub const CAP_MEM:   u64 = 1 << 5;
/// Capability to open raw TCP sockets (SYS_TCP_CONNECT / LISTEN / ACCEPT / etc.).
pub const CAP_NET:   u64 = 1 << 6;
/// All capabilities — granted to kernel tasks.
pub const CAP_ALL:   u64 = !0u64;

/// Convenience: standard set for trusted user tasks (shell, services).
/// No block device access; heap and log allowed.
pub const CAP_USER_DEFAULT: u64 = CAP_SEND | CAP_RECV | CAP_LOG | CAP_MEM | CAP_AI | CAP_NET;
// ─────────────────────────────────────────────────────────────────────────────

/// Dispatch a system call.
/// Called from the exception handler.
/// Returns the value to be placed in x0.
pub fn dispatch(syscall_id: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> isize {
    // Per-task syscall allowlist check (7.4): reject blocked syscall IDs
    // before capability gating.  Kernel tasks always have [!0, !0] → pass.
    if syscall_id < 128 {
        let filter = scheduler::current_syscall_filter();
        let word = syscall_id / 64;
        let bit  = syscall_id % 64;
        if filter[word] & (1u64 << bit) == 0 {
            return -1; // ENOSYS — syscall not in this task's allowlist
        }
    }

    // Fetch caller's capability bitmask once.
    let caps = scheduler::current_caps();

    // Helper: return -13 (EPERM) if the caller lacks a required capability.
    macro_rules! require {
        ($cap:expr) => {
            if caps & $cap == 0 {
                return -13; // EPERM
            }
        };
    }

    match syscall_id {
        SYS_SEND  => { require!(CAP_SEND);  sys_send(arg0, arg1)        }
        SYS_RECV  => { require!(CAP_RECV);  sys_recv(arg0)               }
        SYS_YIELD => sys_yield(),
        SYS_EXIT  => sys_exit(arg0 as i32),
        SYS_WAIT  => sys_wait(arg0),
        SYS_AI_CALL       => { require!(CAP_AI);  sys_ai_call(arg0, arg1, arg2)  }
        SYS_BLK_READ      => { require!(CAP_BLK); sys_blk_read(arg0, arg1, arg2) }
        SYS_BLK_WRITE     => { require!(CAP_BLK); sys_blk_write(arg0, arg1)      }
        SYS_LOG           => { require!(CAP_LOG); sys_log(arg0, arg1)            }
        SYS_MALLOC        => { require!(CAP_MEM); sys_malloc(arg0)               }
        SYS_FREE          => { require!(CAP_MEM); sys_free(arg0)                 }
        SYS_REALLOC       => { require!(CAP_MEM); sys_realloc(arg0, arg1)        }
        SYS_ALIGNED_ALLOC => { require!(CAP_MEM); sys_aligned_alloc(arg0, arg1)  }
        SYS_EXEC     => { require!(CAP_MEM); sys_exec(arg0, arg1, arg2 as u64) }
        SYS_NET_SEND => sys_net_send(arg0, arg1),
        SYS_NET_RECV => sys_net_recv(arg0, arg1),
        SYS_OPEN     => crate::fs::sys_open(arg0 as *const u8, arg1),
        SYS_READ_FD  => crate::fs::sys_read_fd(arg0, arg1 as *mut u8, arg2),
        SYS_WRITE_FD => crate::fs::sys_write_fd(arg0, arg1 as *const u8, arg2),
        SYS_CLOSE    => crate::fs::sys_close(arg0),
        SYS_READDIR  => crate::fs::sys_readdir(
            arg0 as *const u8, arg1,
            arg2 as *mut crate::fs::DirEntry, 64,
        ),
        SYS_WASM_LOAD => { require!(CAP_MEM); sys_wasm_load(arg0, arg1, arg2 as u64) }
        SYS_MMAP      => { require!(CAP_MEM); sys_mmap(arg0, arg1, arg2)             }
        SYS_MUNMAP    => { require!(CAP_MEM); sys_munmap(arg0, arg1)                 }
        SYS_KREAD     => sys_kread(arg0, arg1, arg2),
        SYS_FB_INFO   => sys_fb_info(arg0, arg1, arg2),
        SYS_FB_MAP    => sys_fb_map(),
        SYS_FB_FLUSH  => sys_fb_flush(),
        SYS_INPUT_POLL=> sys_input_poll(arg0, arg1),
        SYS_TRY_RECV  => { require!(CAP_RECV); sys_try_recv(arg0) }
        SYS_GET_RTC   => sys_get_rtc(),
        SYS_GET_STATS => sys_get_stats(arg0),
        SYS_GET_IP      => sys_get_ip(),
        SYS_PING        => sys_ping(arg0, arg1),
        SYS_HTTP_GET    => sys_http_get(arg0, arg1, arg2, arg3),
        SYS_HTTPS_GET   => sys_https_get(arg0, arg1, arg2, arg3),
        SYS_HTTPS_POST  => sys_https_post(arg0, arg1, arg2, arg3, arg4, arg5),
        SYS_DNS_RESOLVE => sys_dns_resolve(arg0, arg1, arg2),
        SYS_CREATE      => crate::fs::sys_create(arg0 as *const u8, arg1),
        SYS_PIPE        => crate::fs::sys_pipe(arg0),
        SYS_SURF_CREATE  => sys_surf_create(arg0 as u32, arg1 as u32, arg2 as u8),
        SYS_SURF_DESTROY => { crate::drivers::compositor::destroy(arg0); 0 }
        SYS_SURF_MAP     => sys_surf_map(arg0),
        SYS_SURF_FLUSH   => { crate::drivers::compositor::composite(); 0 }
        SYS_SURF_MOVE    => { crate::drivers::compositor::set_pos(arg0, arg1 as i32, arg2 as i32); 0 }
        SYS_SET_PRIORITY => { require!(CAP_AI); sys_set_priority(arg0, arg1) }
        SYS_WASM_EXEC    => { require!(CAP_MEM);  sys_wasm_exec(arg0, arg1)           }
        SYS_CAP_GRANT    => { require!(CAP_ALL);  sys_cap_grant(arg0, arg1 as u64)   }
        SYS_CAP_REVOKE   => { require!(CAP_ALL);  sys_cap_revoke(arg0, arg1 as u64)  }
        SYS_CAP_QUERY    => sys_cap_query(arg0),
        SYS_LIST_TASKS   => { require!(CAP_LOG);  sys_list_tasks(arg0, arg1)         }
        SYS_TCP_CONNECT       => { require!(CAP_NET); sys_tcp_connect(arg0, arg1, arg2)            }
        SYS_TCP_LISTEN        => { require!(CAP_NET); sys_tcp_listen(arg0)                         }
        SYS_TCP_ACCEPT        => { require!(CAP_NET); sys_tcp_accept(arg0, arg1)                   }
        SYS_TCP_WRITE         => { require!(CAP_NET); sys_tcp_write(arg0, arg1, arg2)              }
        SYS_TCP_READ          => { require!(CAP_NET); sys_tcp_read(arg0, arg1, arg2)               }
        SYS_TCP_WAIT_READABLE => { require!(CAP_NET); sys_tcp_wait_readable(arg0, arg1)            }
        SYS_TCP_CLOSE         => { require!(CAP_NET); sys_tcp_close(arg0); 0                       }
        SYS_SURF_RESIZE => {
            let ok = crate::drivers::compositor::resize(arg0, arg1 as u32, arg2 as u32);
            if ok { 0 } else { -1 }
        }
        SYS_SURF_RAISE  => { crate::drivers::compositor::raise(arg0); 0 }
        SYS_SURF_LOWER  => { crate::drivers::compositor::lower(arg0); 0 }
        SYS_SURF_DIRTY  => { crate::drivers::compositor::mark_dirty(arg0); 0 }
        SYS_SPEAK => {
            if arg0 != 0 && arg1 > 0 {
                let text = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
                if crate::drivers::hda::speak(text) { 0 } else { -1 }
            } else { -1 }
        }
        SYS_FILTER_SET => {
            // Only privileged (CAP_ALL) callers may restrict another task's
            // syscall filter.  This prevents unprivileged tasks from blocking
            // syscalls for other tasks.
            require!(CAP_ALL);
            let tid = arg0;
            let lo  = arg1 as u64;
            let hi  = arg2 as u64;
            if scheduler::set_task_syscall_filter(tid, [lo, hi]) { 0 } else { -1 }
        }
        SYS_FILTER_GET => {
            let tid = arg0;
            if arg1 == 0 { return -1; }
            match scheduler::get_task_syscall_filter(tid) {
                Some(f) => {
                    let out = arg1 as *mut u64;
                    unsafe {
                        *out = f[0];
                        *out.add(1) = f[1];
                    }
                    0
                }
                None => -1,
            }
        }
        SYS_CONFIG_GET => crate::config::sys_config_get(arg0, arg1, arg2, arg3),
        SYS_CONFIG_SET => { require!(CAP_BLK); crate::config::sys_config_set(arg0, arg1, arg2, arg3) }
        SYS_ATOMIC_WRITE => { require!(CAP_BLK); sys_atomic_write(arg0, arg1, arg2, arg3) }
        SYS_UNLINK => { require!(CAP_BLK); sys_unlink(arg0, arg1) }
        SYS_RENAME => { require!(CAP_BLK); sys_rename(arg0, arg1, arg2, arg3) }
        SYS_SIGACTION => sys_sigaction(arg0, arg1 as u32),
        SYS_KILL      => { require!(CAP_SEND); sys_kill(arg0, arg1 as u32) }
        SYS_SIGRETURN => sys_sigreturn(),
        SYS_SIGMASK   => sys_sigmask(arg0 as u32),
        SYS_CLONE     => { require!(CAP_MEM); sys_clone(arg0, arg1, arg2 as u64) }
        99  => { require!(CAP_LOG); sys_console_gets(arg0, arg1) } // SYS_CONSOLE_GETS
        100 => { require!(CAP_LOG); sys_console_getc()           } // SYS_GETC
        _ => -1, // Unknown syscall
    }
}

/// Load an ELF binary from the FAT32 filesystem by path and spawn it as a
/// new EL0 task.
/// arg0 = path_ptr, arg1 = path_len, arg2 = caps  →  new TID or -1
fn sys_exec(path_ptr: usize, path_len: usize, caps: u64) -> isize {
    if path_ptr == 0 || path_len == 0 { return -1; }
    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
    let path = core::str::from_utf8(path_bytes).unwrap_or("");
    if path.is_empty() { return -1; }

    // Capability attenuation (7.3): child may only receive a subset of the
    // caller's own capabilities.  Prevents privilege escalation via spawn.
    let caller_caps = scheduler::current_caps();
    let effective_caps = caps & caller_caps;

    // Read ELF image from FAT32 into a heap buffer.
    let (elf_ptr, elf_len) = crate::fs::fat32::read_file_alloc(path);
    if elf_ptr.is_null() || elf_len == 0 { return -1; }

    let bytes = unsafe { core::slice::from_raw_parts(elf_ptr as *const u8, elf_len) };
    let result = match crate::process::elf::spawn_elf("user", bytes, effective_caps) {
        Ok(tid) => tid as isize,
        Err(_)  => -1,
    };
    // Free the temporary ELF buffer (ELF loader copied segments to their VAs).
    unsafe { crate::memory::heap::c_free(elf_ptr); }
    result
}

/// Load and run a WASM binary from a user-provided buffer.
/// arg0 = wasm_ptr, arg1 = wasm_len, arg2 = caps  →  0 on success, -1 on error.
/// Runs the module synchronously in the caller's task context (no spawning yet).
fn sys_wasm_load(wasm_ptr: usize, wasm_len: usize, caps: u64) -> isize {
    // Capability attenuation: WASM module may only receive caller's caps.
    let _effective_caps = caps & scheduler::current_caps();
    if wasm_ptr == 0 || wasm_len == 0 { return -1; }
    let bytes = unsafe { core::slice::from_raw_parts(wasm_ptr as *const u8, wasm_len) };
    let module = match crate::wasm::load(bytes) {
        Ok(m)  => m,
        Err(_) => return -1,
    };
    // If the module exports "main", call it.
    if let Some(_idx) = module.find_export("main") {
        let _ = module.call_export("main", &[]);
    }
    0
}

// ─── Capability management syscalls ──────────────────────────────────────────

/// Grant additional capabilities to a task.  Bitwise-OR into existing caps.
/// The granting task may only delegate capabilities it currently holds itself.
fn sys_cap_grant(tid: usize, cap_bits: u64) -> isize {
    // Attenuation: grantor can only delegate what it possesses.
    let grantor_caps = scheduler::current_caps();
    let safe_bits = cap_bits & grantor_caps;
    let existing = match scheduler::get_task_caps(tid) {
        Some(c) => c,
        None    => return -1,
    };
    if scheduler::set_task_caps(tid, existing | safe_bits) { 0 } else { -1 }
}

/// Revoke capabilities from a task.  Bitwise-AND with complement of cap_bits.
fn sys_cap_revoke(tid: usize, cap_bits: u64) -> isize {
    let existing = match scheduler::get_task_caps(tid) {
        Some(c) => c,
        None    => return -1,
    };
    if scheduler::set_task_caps(tid, existing & !cap_bits) { 0 } else { -1 }
}

/// Query the capability bitmask of any task by TID.
/// Returns the bitmask as a usize, or -1 if TID is not found.
fn sys_cap_query(tid: usize) -> isize {
    match scheduler::get_task_caps(tid) {
        Some(c) => c as isize,
        None    => -1,
    }
}

/// Write a snapshot of all live tasks as `TaskRecord` structs to user memory.
/// `out_ptr` must point to a buffer with room for at least `max_entries` records
/// of 32 bytes each.  Returns the number of records written.
fn sys_list_tasks(out_ptr: usize, max_entries: usize) -> isize {
    if out_ptr == 0 || max_entries == 0 { return 0; }
    let out = unsafe {
        core::slice::from_raw_parts_mut(
            out_ptr as *mut scheduler::TaskRecord,
            max_entries,
        )
    };
    scheduler::list_tasks(out) as isize
}

/// Load and run a WASM binary from the FAT32 filesystem by path.
/// Calls `_start` (WASI) or `main` if exported; the module-level `start`
/// function (if any) is already invoked by `Interpreter::new`.
/// Returns the WASM exit code (0 = success) or a negative error:
///   -1  invalid path, -2 file not found, -3 validation failed,
///   -4  load/parse failed, -5 trap during execution.
fn sys_wasm_exec(path_ptr: usize, path_len: usize) -> isize {
    if path_ptr == 0 || path_len == 0 { return -1; }
    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
    let path = match core::str::from_utf8(path_bytes) {
        Ok(s) if !s.is_empty() => s,
        _ => return -1,
    };

    // Load WASM bytes from FAT32 into a temporary heap buffer.
    let (ptr, len) = crate::fs::fat32::read_file_alloc(path);
    if ptr.is_null() || len == 0 { return -2; }

    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };

    // Validate structure before handing to the interpreter.
    if crate::wasm::validate(bytes).is_err() {
        unsafe { crate::memory::heap::c_free(ptr); }
        return -3;
    }

    let result = match crate::wasm::load(bytes) {
        Err(_) => -4,
        Ok(module) => {
            // Try WASI _start, then main(argc, argv), then treat as library (no error).
            if module.find_export("_start").is_some() {
                match module.call_export("_start", &[]) {
                    Ok(_)                                            => 0,
                    Err(crate::wasm::WasmError::Trap("proc_exit")) => 0,
                    Err(_)                                           => -5,
                }
            } else if module.find_export("main").is_some() {
                match module.call_export("main", &[0, 0]) {
                    Ok(rets) => rets.first().copied().unwrap_or(0) as isize,
                    Err(crate::wasm::WasmError::Trap("proc_exit")) => 0,
                    Err(_)                                           => -5,
                }
            } else {
                0 // library module — no entry point required
            }
        }
    };

    unsafe { crate::memory::heap::c_free(ptr); }
    result
}

/// Transmit a raw Ethernet frame via VirtIO-net.
/// arg0 = buf_ptr, arg1 = buf_len.  Returns 0 on success, -1 on failure.
fn sys_net_send(buf_ptr: usize, buf_len: usize) -> isize {
    if buf_ptr == 0 || buf_len == 0 { return -1; }
    let pkt = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, buf_len) };
    if unsafe { crate::drivers::virtio_net::transmit(pkt) } { 0 } else { -1 }
}

/// Poll for a received Ethernet frame from VirtIO-net (non-blocking).
/// arg0 = buf_ptr, arg1 = buf_len.  Returns frame length on success, -1 if none.
fn sys_net_recv(buf_ptr: usize, buf_len: usize) -> isize {
    if buf_ptr == 0 || buf_len == 0 { return -1; }
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len) };
    match unsafe { crate::drivers::virtio_net::receive(buf) } {
        Some(n) => n as isize,
        None    => -1,
    }
}

/// Return one raw character from UART, blocking until available. No echo.
/// Used by the Ash readline implementation for full line-editing in userspace.
fn sys_console_getc() -> isize {
    let uart = crate::drivers::uart::Uart::new();
    crate::arch::aarch64::exceptions::enable_irqs();
    loop {
        if let Some(b) = uart.try_getc() {
            return b as isize;
        }
        unsafe { core::arch::asm!("wfi") };
    }
}

fn sys_console_gets(ptr: usize, max_len: usize) -> isize {
    let ptr = ptr as *mut u8;
    if ptr.is_null() || max_len == 0 {
        return -1;
    }
    let uart = crate::drivers::uart::Uart::new();
    let mut count = 0;
    
    // Enable IRQs to allow timer preemption while we poll
    crate::arch::aarch64::exceptions::enable_irqs();

    loop {
        if let Some(b) = uart.try_getc() {
            // Echo back
            uart.putc(b);
            
            // Handle backspace (127 or 8)
            if b == 127 || b == 8 {
                if count > 0 {
                    count -= 1;
                    // Echo erased char: backspace, space, backspace
                    uart.puts("\x08 \x08"); 
                }
                continue;
            }
            
            // Store byte
            if count < max_len {
                unsafe { *ptr.add(count) = b };
                count += 1;
            }
            
            // Enter key (CR or LF)
            if b == b'\r' || b == b'\n' {
                uart.puts("\r\n"); // Echo newline
                break;
            }
        } else {
            // Wait for interrupt (timer or UART if we had it)
            // This yields CPU to other tasks until next tick (10ms)
            unsafe { core::arch::asm!("wfi") };
        }
    }
    
    count as isize
}

fn sys_log(ptr: usize, len: usize) -> isize {
    let ptr = ptr as *const u8;
    if ptr.is_null() || len == 0 {
        return -1;
    }
    let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
    if let Ok(s) = core::str::from_utf8(slice) {
        let uart = crate::drivers::uart::Uart::new();
        uart.puts(s);
    }
    0
}

fn sys_send(target_tid: usize, msg_ptr: usize) -> isize {
    let msg_ptr = msg_ptr as *const Message;
    if msg_ptr.is_null() {
        return -1;
    }
    let mut msg = unsafe { *msg_ptr };

    let sender = scheduler::current_tid();
    let caps   = scheduler::current_caps();

    // IPC target capability enforcement:
    //   TID 1 (Numenor)  — requires CAP_AI
    //   TID 2 (Semantic) — requires CAP_SEND (already checked in dispatch)
    //   TID 0 (idle/kernel) — blocked; no task should message idle
    // Kernel tasks (caps == CAP_ALL) bypass these checks.
    if caps != CAP_ALL {
        let allowed = match target_tid {
            0 => false,                         // Never message idle task
            1 => caps & CAP_AI   != 0,          // Numenor requires CAP_AI
            _ => caps & CAP_SEND != 0,          // All other targets: CAP_SEND sufficient
        };
        if !allowed {
            return -13; // EPERM
        }
    }

    msg.sender = sender; // Kernel enforces sender identity

    let op = u64::from_le_bytes(msg.data[0..8].try_into().unwrap_or([0; 8]));
    let uart = crate::drivers::uart::Uart::new();
    uart.puts("[sys] SEND from ");
    uart.put_dec(sender);
    uart.puts(" to ");
    uart.put_dec(target_tid);
    uart.puts(" op=");
    uart.put_dec(op as usize);
    uart.puts("\r\n");

    match scheduler::send_message(target_tid, msg) {
        Ok(_)  => 0,
        Err(_) => -2,
    }
}

fn sys_recv(msg_ptr: usize) -> isize {
    let msg_ptr = msg_ptr as *mut Message;
    if msg_ptr.is_null() {
        return -1;
    }

    // Checking mailbox
    if let Some(msg) = scheduler::recv_message() {
        unsafe { *msg_ptr = msg; }
        
        let receiver = scheduler::current_tid();
        let op = u64::from_le_bytes(msg.data[0..8].try_into().unwrap_or([0; 8]));
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[sys] RECV by ");
        uart.put_dec(receiver);
        uart.puts(" from ");
        uart.put_dec(msg.sender);
        uart.puts(" op=");
        uart.put_dec(op as usize);
        uart.puts("\r\n");

        return 0; // Success
    } else {
        return 1; 
    }
}

/// Non-blocking receive: pops a message if available, does NOT set task to Blocked.
/// Returns 0 and fills *msg_ptr on success; returns 1 if no message.
fn sys_try_recv(msg_ptr: usize) -> isize {
    let msg_ptr = msg_ptr as *mut Message;
    if msg_ptr.is_null() { return -1; }
    match scheduler::try_recv_message() {
        Some(msg) => { unsafe { *msg_ptr = msg; } 0 }
        None      => 1,
    }
}

fn sys_yield() -> isize {
    scheduler::yield_cpu();
    0
}

fn sys_exit(code: i32) -> isize {
    scheduler::exit_task(code);
    0 // Return value is irrelevant — schedule_next will switch us away
}

/// Block until `target_tid` exits; returns its exit code.
/// If the task is already dead the call returns immediately.
fn sys_wait(target_tid: usize) -> isize {
    match scheduler::wait_for_tid(target_tid) {
        Some(code) => code as isize,
        // None ⟹ current task is now Blocked; schedule_next (called by
        // sync_handler after dispatch) will switch us out.  When exit_task
        // wakes us, it patches x0 in our trap frame with the real exit code.
        None => 0,
    }
}

/// AI Syscall: Simple wrapper around IPC to Numenor.
/// user sends: op, arg1, arg2
/// We construct an AiMessage and send it to NUMENOR_TID.
/// Then we block waiting for reply?
///
/// Actually, to keep it simple and synchronous-looking to the user:
/// 1. Construct IPC message with the args.
/// 2. Send to Numenor (this might block if mailbox full, or fail).
/// 3. Recv from Numenor (blocking).
///
/// NOTE: This implementation is "in-kernel" IPC. The user task is the one calling this.
/// So we are running in the context of the user task.
///
/// However, `sys_recv` expects a `Message` struct pointer.
/// And `sys_send` expects a `Message` struct.
///
/// Numenor expects `AiMessage` encoded in the 32-byte payload.
fn sys_ai_call(op: usize, arg1: usize, arg2: usize) -> isize {
    // 1. Construct request
    let ai_msg = numenor::ipc::AiMessage {
        op: op as u64,
        arg1: arg1 as u64,
        arg2: arg2 as u64,
    };
    let data = ai_msg.to_bytes();
    
    // We need to know who we are to receive reply? 
    // The IPC system fills in `sender` automatically in `send_message`.
    // Numenor sees our TID and replies to it.
    
    let req = Message {
        sender: scheduler::current_tid(),
        data,
    };
    
    // 2. Send to Numenor
    if let Err(_) = scheduler::send_message(NUMENOR_TID, req) {
        return -1; // Failed to send (full or Numenor dead)
    }
    
    // 3. Receive reply
    // We want to block until we get a reply FROM Numenor.
    // The generic `sys_recv` pops ANY message.
    // If we have other messages, better implementation would filter.
    // For now, accept next message.
    
    // We can't easily reuse `sys_recv` here because `sys_recv` returns to userspace for blocking.
    // If we want `sys_ai_call` to BLOCK inside the syscall, we need to:
    // a) Set task state Blocked.
    // b) Call schedule?
    // But we are in the syscall handler.
    //
    // If we return, we return to userspace.
    // So `sys_ai_call` CANNOT block in the kernel logic easily without a "wait_for" mechanism in scheduler
    // that doesn't return to userspace.
    //
    // ALTERNATIVE: `sys_ai_call` sends the request and returns "0" (Async) or "Blocked" (Sync)?
    // Or we stick to the plan: Userspace wrapper does Send + Recv manually?
    //
    // Let's make `sys_ai_call` just SEND for Phase 3 (Async request).
    // An actual synchronous call would require userspace cooperation or more complex kernel scheduling.
    //
    // WAIT. If `sys_ai_call` returns, the user task continues.
    // If we want it to wait for answer, the USER CODE must call `recv`.
    //
    // So `sys_ai_call` is just a helper to format and send the message to Numenor?
    // Yes.
    
    0 // Success (Request sent)
}

fn sys_blk_read(sector: usize, buf_ptr: usize, len: usize) -> isize {
    let buf_ptr = buf_ptr as *mut u8;
    if buf_ptr.is_null() || len == 0 {
        return -1;
    }

    // IRQs are already masked by the SVC exception entry — no explicit
    // disable/enable needed here.  Re-enabling inside the exception handler
    // allows nested timer IRQs to fire mid-schedule_next, corrupting
    // scheduler state.  The eret from sync_handler naturally restores
    // the pre-SVC PSTATE (I=0) via the saved SPSR.
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, len) };
    unsafe { crate::drivers::virtio::read_sectors(sector as u64, buf); }

    len as isize
}


fn sys_blk_write(sector: usize, buf_ptr: usize) -> isize {
    let buf_ptr = buf_ptr as *const u8;
    if buf_ptr.is_null() {
        return -1;
    }

    let buf = unsafe { core::slice::from_raw_parts(buf_ptr, 512) };
    unsafe { crate::drivers::virtio::write_block(sector as u64, buf); }

    0
}


fn sys_malloc(size: usize) -> isize {
    // let uart = crate::drivers::uart::Uart::new();
    // uart.puts("[sys] malloc ");
    // uart.put_dec(size);
    // uart.puts("\r\n");
    unsafe { crate::memory::heap::c_malloc(size) as isize }
}

fn sys_free(ptr: usize) -> isize {
    unsafe { crate::memory::heap::c_free(ptr as *mut u8) };
    0
}

fn sys_realloc(ptr: usize, size: usize) -> isize {
    unsafe { crate::memory::heap::c_realloc(ptr as *mut u8, size) as isize }
}

fn sys_aligned_alloc(align: usize, size: usize) -> isize {
    unsafe { crate::memory::heap::c_aligned_alloc(size, align) as isize }
}

/// Copy bytes from a kernel VA (TTBR1) to a user VA (TTBR0).
///
/// After the TTBR1 split, EL0 code cannot access kernel high-VA addresses
/// (0xFFFF_0000_...). This syscall lets the kernel do the copy on behalf of
/// the EL0 caller: we run at EL1 and can reach TTBR1 directly.
///
/// k_ptr — source, must be a kernel high-VA (>= KERNEL_VA_OFFSET)
/// u_ptr — destination, a user-space VA accessible via the current TTBR0
/// len   — bytes to copy (capped at 4096)
///
/// Returns bytes copied, or -1 on invalid arguments.
fn sys_kread(k_ptr: usize, u_ptr: usize, len: usize) -> isize {
    use crate::memory::vmm::KERNEL_VA_OFFSET;
    if k_ptr < KERNEL_VA_OFFSET || u_ptr == 0 || len == 0 { return -1; }
    let n = len.min(4096);
    unsafe { core::ptr::copy_nonoverlapping(k_ptr as *const u8, u_ptr as *mut u8, n); }
    n as isize
}

/// Allocate anonymous memory and map it into the calling task's page table.
///
/// `_addr_hint` — ignored (identity-mapped: VA == PA, allocated address used).
/// `size`       — bytes to allocate; rounded up to the next 4 KiB multiple.
/// `_prot`      — ignored; always maps as EL0 read/write, no-execute.
///
/// Returns the virtual address of the new mapping on success, -12 (ENOMEM)
/// or -1 on failure.
fn sys_mmap(_addr_hint: usize, size: usize, _prot: usize) -> isize {
    if size == 0 { return -1; }
    let pages = (size + 0xFFF) / 4096;
    let pa = match crate::memory::alloc_pages(pages) {
        Some(a) => a,
        None    => return -12, // ENOMEM
    };
    // Zero the newly allocated pages (anonymous mmap semantics).
    unsafe { core::ptr::write_bytes(pa as *mut u8, 0, pages * 4096); }

    // Map pages in the current task's per-process table.
    let ttbr0 = scheduler::current_task_ttbr0();
    if ttbr0 != 0 {
        crate::memory::vmm::map_user_rw(ttbr0, pa, size);
    }
    pa as isize
}

/// Unmap a range of anonymous memory from the calling task's page table.
///
/// Walks the per-process L3 page table, clears each valid 4 KiB entry,
/// issues a per-VA `tlbi vaae1` flush, and returns the physical pages to
/// the page allocator.  Returns 0 on success, -1 on bad arguments.
fn sys_munmap(va: usize, size: usize) -> isize {
    if va == 0 || size == 0 { return -1; }
    let ttbr0 = scheduler::current_task_ttbr0();
    if ttbr0 == 0 { return -1; }
    crate::memory::vmm::unmap_pages(ttbr0, va, size);
    0
}

fn sys_fb_info(w_ptr: usize, h_ptr: usize, p_ptr: usize) -> isize {
    let (_, w, h, p) = unsafe { crate::drivers::virtio_gpu::get_framebuffer() };
    if w_ptr != 0 { unsafe { *(w_ptr as *mut u32) = w; } }
    if h_ptr != 0 { unsafe { *(h_ptr as *mut u32) = h; } }
    if p_ptr != 0 { unsafe { *(p_ptr as *mut u32) = p; } }
    0
}

fn sys_fb_map() -> isize {
    let (va, w, h, p) = unsafe { crate::drivers::virtio_gpu::get_framebuffer() };
    if va.is_null() || w == 0 { return -1; }

    let fb_pa = va as usize - crate::memory::vmm::KERNEL_VA_OFFSET;
    let size = (p * h) as usize;
    let ttbr0 = crate::process::scheduler::current_task_ttbr0();
    if ttbr0 != 0 {
        crate::memory::vmm::map_user_rw(ttbr0, fb_pa, size);
    }
    fb_pa as isize
}

fn sys_fb_flush() -> isize {
    unsafe { crate::drivers::virtio_gpu::flush(); }
    0
}

/// Write system stats into a 16-byte user buffer: [free_mb, total_mb, cpu_pct, task_count] (u32 each).
fn sys_get_stats(out_ptr: usize) -> isize {
    if out_ptr == 0 { return -1; }
    let free_p  = crate::memory::page::free_pages();
    let total_p = crate::memory::page::total_pages();
    let free_mb  = (free_p  * 4096 / (1024 * 1024)) as u32;
    let total_mb = (total_p * 4096 / (1024 * 1024)) as u32;
    let idle  = scheduler::idle_ticks() as u64;
    let total = scheduler::total_ticks() as u64;
    let cpu_pct = if total > 0 { ((total - idle) * 100 / total) as u32 } else { 0 };
    let task_cnt = scheduler::task_count() as u32;
    let buf = [free_mb, total_mb, cpu_pct, task_cnt];
    unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr() as *const u8, out_ptr as *mut u8, 16); }
    0
}

fn sys_get_ip() -> isize {
    let ip = crate::net::get_ip();
    ((ip[0] as isize) << 24) | ((ip[1] as isize) << 16) | ((ip[2] as isize) << 8) | (ip[3] as isize)
}

fn sys_ping(ip_packed: usize, timeout_ms: usize) -> isize {
    let ip = [
        (ip_packed >> 24) as u8,
        (ip_packed >> 16) as u8,
        (ip_packed >> 8)  as u8,
        ip_packed as u8,
    ];
    crate::net::send_ping(ip, timeout_ms as u32)
}

/// Read the PL031 RTC data register and return the Unix epoch as isize.
/// The RTC is at PA 0x09010000; after the TTBR1 split its kernel VA = PA + KERNEL_VA_OFFSET.
fn sys_get_rtc() -> isize {
    use crate::memory::vmm::KERNEL_VA_OFFSET;
    let va = 0x0901_0000usize + KERNEL_VA_OFFSET;
    unsafe { (va as *const u32).read_volatile() as isize }
}

fn sys_input_poll(out_ptr: usize, max_events: usize) -> isize {
    if out_ptr == 0 || max_events == 0 { return 0; }
    unsafe {
        let ptr = out_ptr as *mut crate::drivers::virtio_input::VirtIOInputEvent;
        crate::drivers::virtio_input::poll_sys(ptr, max_events) as isize
    }
}

// ── Network high-level syscalls ───────────────────────────────────────────────

/// Resolve a hostname to an IPv4 address.
/// arg0 = name_ptr, arg1 = name_len, arg2 = out_ip4_ptr (4 bytes, user VA)
/// Returns 0 on success, -1 on failure.
fn sys_dns_resolve(name_ptr: usize, name_len: usize, out_ptr: usize) -> isize {
    if name_ptr == 0 || name_len == 0 { return -1; }
    let bytes = unsafe { core::slice::from_raw_parts(name_ptr as *const u8, name_len) };
    let name  = core::str::from_utf8(bytes).unwrap_or("");
    match crate::net::dns::dns_resolve(name) {
        Some(crate::net::IpAddr::V4(ip)) => {
            if out_ptr != 0 {
                unsafe { core::ptr::copy_nonoverlapping(ip.as_ptr(), out_ptr as *mut u8, 4); }
            }
            0
        }
        Some(crate::net::IpAddr::V6(ip6)) => {
            // Return first 4 bytes of IPv6 if caller only wants IPv4
            if out_ptr != 0 {
                unsafe { core::ptr::copy_nonoverlapping(ip6[12..].as_ptr(), out_ptr as *mut u8, 4); }
            }
            0
        }
        None => -1,
    }
}

/// Perform an HTTP GET request.
/// arg0 = url_ptr, arg1 = url_len, arg2 = buf_ptr, arg3 = buf_len
/// Returns bytes written on success; negative error codes:
///   -1 = DNS error, -2 = TCP error, -3 = HTTP non-2xx, -4 = timeout,
///   -5 = buffer too small, -6 = TLS not implemented
fn sys_http_get(url_ptr: usize, url_len: usize, buf_ptr: usize, buf_len: usize) -> isize {
    if url_ptr == 0 || url_len == 0 || buf_ptr == 0 || buf_len == 0 { return -1; }
    let url_bytes = unsafe { core::slice::from_raw_parts(url_ptr as *const u8, url_len) };
    let url = core::str::from_utf8(url_bytes).unwrap_or("");
    let (https, host, port, path) = match crate::net::http::parse_url(url) {
        Some(t) => t,
        None    => return -1,
    };
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len) };
    http_result_to_isize(if https {
        crate::net::tls::tls_get(host, path, buf)
    } else {
        crate::net::http::http_get(host, path, port, buf)
    })
}

/// Perform an HTTPS GET request via TLS 1.3 (AES-128-GCM-SHA256 + x25519).
fn sys_https_get(url_ptr: usize, url_len: usize, buf_ptr: usize, buf_len: usize) -> isize {
    if url_ptr == 0 || url_len == 0 || buf_ptr == 0 || buf_len == 0 { return -1; }
    let url_bytes = unsafe { core::slice::from_raw_parts(url_ptr as *const u8, url_len) };
    let url = core::str::from_utf8(url_bytes).unwrap_or("");
    let (_, host, _, path) = match crate::net::http::parse_url(url) {
        Some(t) => t,
        None    => return -1,
    };
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len) };
    http_result_to_isize(crate::net::tls::tls_get(host, path, buf))
}

/// HTTPS POST — (url_ptr, url_len, body_ptr, body_len, buf_ptr, buf_len) → bytes or -err
/// Content-Type is auto-detected: JSON if body starts with `{`, else `application/octet-stream`.
fn sys_https_post(url_ptr: usize, url_len: usize, body_ptr: usize, body_len: usize,
                  buf_ptr: usize, buf_len: usize) -> isize {
    if url_ptr == 0 || url_len == 0 || buf_ptr == 0 || buf_len == 0 { return -1; }
    let url_bytes = unsafe { core::slice::from_raw_parts(url_ptr as *const u8, url_len) };
    let url = core::str::from_utf8(url_bytes).unwrap_or("");
    let (_, host, _, path) = match crate::net::http::parse_url(url) {
        Some(t) => t,
        None    => return -1,
    };
    let body = if body_ptr != 0 && body_len > 0 {
        unsafe { core::slice::from_raw_parts(body_ptr as *const u8, body_len) }
    } else {
        &[]
    };
    let ct = if body.first() == Some(&b'{') { "application/json" } else { "application/octet-stream" };
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len) };
    http_result_to_isize(crate::net::tls::tls_post(host, path, ct, body, buf))
}

#[inline]
fn http_result_to_isize(r: crate::net::http::HttpResult) -> isize {
    match r {
        crate::net::http::HttpResult::Ok(n)         => n as isize,
        crate::net::http::HttpResult::DnsError       => -1,
        crate::net::http::HttpResult::TcpError       => -2,
        crate::net::http::HttpResult::HttpError(_)   => -3,
        crate::net::http::HttpResult::Timeout        => -4,
        crate::net::http::HttpResult::BufferTooSmall => -5,
    }
}

/// Set a task's scheduling priority (1–8 ticks per round-robin pass).
/// Only callable by tasks with CAP_AI (Numenor).  Returns 0 always.
fn sys_set_priority(tid: usize, priority: usize) -> isize {
    scheduler::set_task_priority(tid, priority.min(8) as u8);
    0
}

// ── Compositor syscalls ───────────────────────────────────────────────────────

/// Create a compositor surface owned by the calling task.
/// arg0=width, arg1=height, arg2=z_order  →  surface_id (>=0) or -1 on error.
fn sys_surf_create(w: u32, h: u32, z: u8) -> isize {
    let tid = scheduler::current_tid();
    let id  = crate::drivers::compositor::create(tid, w, h, z);
    if id == usize::MAX { -1 } else { id as isize }
}

/// Map a surface's pixel buffer into the calling task's address space.
/// Returns the physical address (which sys_mmap / identity mapping makes
/// accessible to the user process); returns -1 if the surface id is invalid.
///
/// The caller should use SYS_MMAP to obtain a writable user-VA for the
/// returned physical address, or access it directly when running as a kernel
/// task (where PA == VA - KERNEL_VA_OFFSET).
fn sys_surf_map(id: usize) -> isize {
    let pa = crate::drivers::compositor::buf_pa(id);
    if pa == 0 { return -1; }

    // Map the surface buffer pages into the calling task's TTBR0 so the
    // EL0 process can write pixels without a per-pixel syscall.
    let surf_size = {
        // Derive byte size from buf_pa by looking up the surface record.
        // We can't access SURFACES directly here, so use buf_pa != 0 as a
        // proxy and let the mmap path use alloc_pages granularity.
        // Re-derive size: read width/height from compositor state.
        // Simple approach: map up to 1280*720*4 = 3.7 MB (rounded to 4 MB).
        // The actual allocation was done with exactly the right page count;
        // we over-map slightly for simplicity — unused tail pages are zeroed.
        (1280 * 720 * 4 + 4095) & !4095  // 3,686,400 rounded up to page boundary
    };

    let ttbr0 = scheduler::current_task_ttbr0();
    if ttbr0 != 0 {
        crate::memory::vmm::map_user_rw(ttbr0, pa, surf_size);
    }
    pa as isize
}

// ── TCP socket syscalls ───────────────────────────────────────────────────────
//
// These give EL0 user tasks direct access to the kernel TCP stack.
// IP addresses are passed as a packed big-endian u32
// (e.g., 10.0.2.15 = 0x0A00_020F).

/// SYS_TCP_CONNECT — initiate and complete a TCP connection.
/// Returns conn_id ≥ 0 on success, -1 on failure/timeout.
fn sys_tcp_connect(ip_u32: usize, port: usize, timeout_ms: usize) -> isize {
    let ip = [
        ((ip_u32 >> 24) & 0xFF) as u8,
        ((ip_u32 >> 16) & 0xFF) as u8,
        ((ip_u32 >> 8)  & 0xFF) as u8,
        ( ip_u32        & 0xFF) as u8,
    ];
    let id = match crate::net::tcp::tcp_connect(crate::net::IpAddr::V4(ip), port as u16) {
        Some(id) => id,
        None     => return -1,
    };
    let ok = crate::net::tcp::tcp_wait_established(id, timeout_ms as u32);
    if ok { id as isize } else { crate::net::tcp::tcp_close(id); -1 }
}

/// SYS_TCP_LISTEN — allocate a listener slot on a local port.
/// Returns listener_id ≥ 0 on success, -1 if no slot available.
fn sys_tcp_listen(port: usize) -> isize {
    match crate::net::tcp::tcp_listen(port as u16) {
        Some(id) => id as isize,
        None     => -1,
    }
}

/// SYS_TCP_ACCEPT — block until a new connection arrives on the listener.
/// Returns conn_id ≥ 0 on success, -1 on timeout.
fn sys_tcp_accept(listener_id: usize, timeout_ms: usize) -> isize {
    match crate::net::tcp::tcp_accept(listener_id, timeout_ms as u32) {
        Some(id) => id as isize,
        None     => -1,
    }
}

/// SYS_TCP_WRITE — write bytes to an established connection.
/// Returns bytes queued, -1 on bad conn_id / wrong state.
fn sys_tcp_write(conn_id: usize, buf_ptr: usize, len: usize) -> isize {
    if buf_ptr == 0 || len == 0 { return -1; }
    let data = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, len) };
    let n = crate::net::tcp::tcp_write(conn_id, data);
    if n == 0 { -1 } else { n as isize }
}

/// SYS_TCP_READ — read bytes from an established connection (non-blocking).
/// Returns bytes read (0 if none available), -1 on bad conn_id.
fn sys_tcp_read(conn_id: usize, buf_ptr: usize, len: usize) -> isize {
    if buf_ptr == 0 || len == 0 { return -1; }
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len) };
    crate::net::tcp::tcp_read(conn_id, buf) as isize
}

/// SYS_TCP_WAIT_READABLE — block until data is available or connection closes.
/// Returns 0 on data available, -1 on timeout / closed.
fn sys_tcp_wait_readable(conn_id: usize, timeout_ms: usize) -> isize {
    if crate::net::tcp::tcp_wait_readable(conn_id, timeout_ms as u32) { 0 } else { -1 }
}

/// SYS_TCP_CLOSE — initiate close and free the conn slot.
fn sys_tcp_close(conn_id: usize) {
    crate::net::tcp::tcp_close(conn_id);
}

// ── 1.4 Signals ───────────────────────────────────────────────────────────────

/// SYS_SIGACTION — register an EL0 signal trampoline.
/// `handler_va` = EL0 function pointer (called with x0 = signum).
/// `mask`       = signals blocked during handler execution.
fn sys_sigaction(handler_va: usize, mask: u32) -> isize {
    scheduler::set_signal_handler(handler_va, mask);
    0
}

/// SYS_KILL — send signal `sig` to task `tid`.
fn sys_kill(tid: usize, sig: u32) -> isize {
    if sig >= 32 { return -1; }
    // SIGKILL (1) is unconditional and kills the target
    if sig == SIGKILL {
        // Mark target as Dead
        scheduler::force_exit(tid, -1);
        return 0;
    }
    if scheduler::send_signal(tid, sig) { 0 } else { -1 }
}

/// SYS_SIGRETURN — called by the EL0 signal trampoline after the handler
/// returns.  Restores the interrupted context from the kernel's saved frame.
/// This is a no-op here because the trampoline calls `ret` which returns to
/// the ELR that was saved before we redirected it to the handler.
/// The actual resume happens in `maybe_deliver_signal()`.
fn sys_sigreturn() -> isize {
    0
}

/// SYS_SIGMASK — set the current task's signal mask.  Returns old mask.
fn sys_sigmask(new_mask: u32) -> isize {
    scheduler::swap_signal_mask(new_mask) as isize
}

// ── 1.6 fork/clone — user-initiated task creation ────────────────────────────

/// SYS_CLONE — spawn a new EL0 thread sharing the calling process's address
/// space (TTBR0).
///
/// * arg0 = `entry_va`  — EL0 entry point for the new thread
/// * arg1 = `stack_top` — top of the new thread's pre-allocated user stack
/// * arg2 = `caps`      — requested capability mask (attenuated to parent's)
///
/// Returns the new TID (≥1) on success, or -1 on failure.
///
/// The thread shares the parent's page table so they see the same virtual
/// address space.  Each thread has its own kernel stack, trap frame, mailbox,
/// and scheduler slot.  The user must allocate a separate user stack and pass
/// its top address as `stack_top` — threads sharing a stack will corrupt each
/// other.
fn sys_clone(entry_va: usize, stack_top: usize, caps: u64) -> isize {
    if entry_va == 0 { return -1; }

    // Attenuate requested caps to the caller's own cap set (7.3 inheritance).
    let caller_caps = scheduler::current_caps();
    let effective_caps = caps & caller_caps;

    // Share the calling task's TTBR0 (address space).
    let parent_ttbr0 = scheduler::current_task_ttbr0();
    if parent_ttbr0 == 0 { return -1; } // Kernel task — no user address space

    match scheduler::spawn_user_thread("thread", entry_va, stack_top, effective_caps, parent_ttbr0) {
        Ok(tid) => tid as isize,
        Err(_)  => -1,
    }
}

// ── 5.4 Atomic file writes ────────────────────────────────────────────────────

/// SYS_ATOMIC_WRITE — write `data` to `path` atomically using a temp file.
///
/// Algorithm:
///   1. Write data to `<path>.t` (a temp file in the same directory).
///   2. Call `fat32::rename("<path>.t", path)` to atomically replace.
///
/// Power-loss during step 1 leaves the original file intact.
/// Power-loss during step 2 may leave either the old or new file.
fn sys_atomic_write(path_ptr: usize, path_len: usize, data_ptr: usize, data_len: usize) -> isize {
    if path_ptr == 0 || path_len == 0 || data_ptr == 0 { return -1; }
    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
    let path = match core::str::from_utf8(path_bytes) {
        Ok(s) => s, Err(_) => return -1,
    };

    // Build temp path: append ".t" (keep under 8-char FAT32 limit by truncating base)
    let mut tmp = [0u8; 16];
    let base = path.trim_start_matches('/');
    let dot  = base.rfind('.').unwrap_or(base.len());
    let stem = &base[..dot.min(7)]; // at most 7 chars so ".t" fits
    let tlen = stem.len();
    tmp[0] = b'/';
    tmp[1..1+tlen].copy_from_slice(stem.as_bytes());
    tmp[1+tlen..3+tlen].copy_from_slice(b".t");
    let tmp_path = core::str::from_utf8(&tmp[..3+tlen]).unwrap_or("/tmp.t");

    // Write data to temp file
    let fd = match crate::fs::fat32::open_write(tmp_path) {
        Some(f) => f, None => return -1,
    };
    let data = unsafe { core::slice::from_raw_parts(data_ptr as *const u8, data_len) };
    crate::fs::fat32::write(fd, data);
    crate::fs::fat32::close(fd);

    // Rename temp → destination
    if crate::fs::fat32::rename(tmp_path, path) { 0 } else { -1 }
}

/// SYS_UNLINK — delete a file.
fn sys_unlink(path_ptr: usize, path_len: usize) -> isize {
    if path_ptr == 0 || path_len == 0 { return -1; }
    let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
    let path  = match core::str::from_utf8(bytes) { Ok(s) => s, Err(_) => return -1 };
    if crate::fs::fat32::unlink(path) { 0 } else { -1 }
}

/// SYS_RENAME — rename/move a file (same directory only).
fn sys_rename(src_ptr: usize, src_len: usize, dst_ptr: usize, dst_len: usize) -> isize {
    if src_ptr == 0 || src_len == 0 || dst_ptr == 0 || dst_len == 0 { return -1; }
    let src_b = unsafe { core::slice::from_raw_parts(src_ptr as *const u8, src_len) };
    let dst_b = unsafe { core::slice::from_raw_parts(dst_ptr as *const u8, dst_len) };
    let src = match core::str::from_utf8(src_b) { Ok(s) => s, Err(_) => return -1 };
    let dst = match core::str::from_utf8(dst_b) { Ok(s) => s, Err(_) => return -1 };
    if crate::fs::fat32::rename(src, dst) { 0 } else { -1 }
}
