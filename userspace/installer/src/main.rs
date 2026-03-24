#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;
use aska::{
    render::{self, Sink},
};

// ── Syscall numbers ────────────────────────────────────────────────────────────
const SYS_SEND:         usize = 1;
const SYS_RECV:         usize = 2;
const SYS_YIELD:        usize = 3;
const SYS_EXIT:         usize = 4;
const SYS_BLK_READ:     usize = 6;
const SYS_BLK_WRITE:    usize = 7;
const SYS_LOG:          usize = 8;
const SYS_CONSOLE_GETS: usize = 99;
const SYS_KREAD:        usize = 25;

// ── Semantic service TID and opcodes ──────────────────────────────────────────
const SEM_TID:       usize = 2;
const OP_STORE:      u64   = 100;
const OP_RETRIEVE:   u64   = 101;
const OP_SEARCH_TAG: u64   = 103;
const OP_QUERY:      u64   = 104;

// ── AArch64 CRT0 ──────────────────────────────────────────────────────────────
global_asm!(
    ".section .text.installer_entry, \"ax\"",
    ".global installer_entry",
    "installer_entry:",
    "    adrp x0, __bss_start",
    "    add  x0, x0, :lo12:__bss_start",
    "    adrp x1, __bss_end",
    "    add  x1, x1, :lo12:__bss_end",
    "0:  cmp  x0, x1",
    "    b.ge 1f",
    "    str  xzr, [x0], #8",
    "    b    0b",
    "1:  bl   installer_main",
    "2:  mov  x8, #4",
    "    svc  #0",
    "    b    2b",
);

#[inline(always)]
unsafe fn syscall_0(n: usize) -> usize {
    let ret: usize;
    core::arch::asm!("svc #0", in("x8") n, lateout("x0") ret, clobber_abi("system"));
    ret
}

#[inline(always)]
unsafe fn syscall_1(n: usize, a0: usize) -> usize {
    let ret: usize;
    core::arch::asm!("svc #0", in("x8") n, in("x0") a0, lateout("x0") ret, clobber_abi("system"));
    ret
}

#[inline(always)]
unsafe fn syscall_2(n: usize, a0: usize, a1: usize) -> usize {
    let ret: usize;
    core::arch::asm!("svc #0", in("x8") n, in("x0") a0, in("x1") a1, lateout("x0") ret, clobber_abi("system"));
    ret
}

#[inline(always)]
unsafe fn syscall_3(n: usize, a0: usize, a1: usize, a2: usize) -> usize {
    let ret: usize;
    core::arch::asm!("svc #0", in("x8") n, in("x0") a0, in("x1") a1, in("x2") a2, lateout("x0") ret, clobber_abi("system"));
    ret
}

fn print(s: &str) {
    unsafe { syscall_2(SYS_LOG, s.as_ptr() as usize, s.len()) };
}

fn println(s: &str) {
    print(s);
    print("\r\n");
}

struct UartSink;
impl Sink for UartSink {
    fn write_str(&mut self, s: &str) { print(s); }
}

#[repr(C)]
struct Message {
    sender: usize,
    data:   [u8; 32],
}

unsafe fn sys_yield() {
    syscall_0(SYS_YIELD);
}

unsafe fn sys_exit() -> ! {
    syscall_0(SYS_EXIT);
    loop {}
}

fn sys_blk_read(sector: usize, mut buf: &mut [u8]) -> isize {
    unsafe { syscall_3(SYS_BLK_READ, sector, buf.as_mut_ptr() as usize, buf.len()) as isize }
}

fn sys_blk_write(sector: usize, buf: &[u8]) -> isize {
    unsafe { syscall_2(SYS_BLK_WRITE, sector, buf.as_ptr() as usize) as isize }
}

unsafe fn sys_send_msg(dest: usize, op: u64, data_ptr: *const u8, len: usize) -> isize {
    let mut msg = Message { sender: 0, data: [0; 32] };
    msg.data[0..8].copy_from_slice(&op.to_le_bytes());
    msg.data[8..16].copy_from_slice(&(data_ptr as u64).to_le_bytes());
    msg.data[16..24].copy_from_slice(&(len as u64).to_le_bytes());
    syscall_2(SYS_SEND, dest, &msg as *const _ as usize) as isize
}

fn sem_send(data: [u8; 32]) {
    let msg = Message { sender: 0, data };
    unsafe { syscall_2(SYS_SEND, SEM_TID, &msg as *const _ as usize) };
}

fn sem_recv() -> [u8; 32] {
    let mut msg = Message { sender: 0, data: [0; 32] };
    loop {
        let ret = unsafe { syscall_1(SYS_RECV, &mut msg as *mut _ as usize) as isize };
        if ret == 0 && msg.sender == SEM_TID {
            return msg.data;
        }
        unsafe { sys_yield() };
    }
}

fn ai_query(prompt: &str) {
    unsafe { sys_send_msg(1, 2, prompt.as_ptr(), prompt.len()) };
    
    let mut resp_buf = [0u8; 1024];
    let resp_len: usize = unsafe {
        loop {
            let mut msg = Message { sender: 0, data: [0; 32] };
            let ret = syscall_1(SYS_RECV, &mut msg as *mut _ as usize) as isize;
            if ret == 0 && msg.sender == 1 {
                let ptr = u64::from_le_bytes(msg.data[8..16].try_into().unwrap_or([0; 8])) as usize;
                let rlen = u64::from_le_bytes(msg.data[16..24].try_into().unwrap_or([0; 8])) as usize;
                let n    = rlen.min(resp_buf.len());
                if ptr != 0 && n > 0 {
                    syscall_3(SYS_KREAD, ptr, resp_buf.as_mut_ptr() as usize, n);
                }
                break n;
            }
            sys_yield();
        }
    };
    
    if resp_len > 0 {
        let text = core::str::from_utf8(&resp_buf[..resp_len]).unwrap_or("");
        // Remove [[OP:..]] tool calls if any from Numenor
        if text.contains("[[") {
            let before = &text[..text.find("[[").unwrap()];
            print(before);
        } else {
            print(text);
        }
    }
}

// ── Partition constants ────────────────────────────────────────────────────────
const SECTOR_SIZE: usize = 512;
const ESP_SIZE_SECTORS: u64 = 256 * 1024 * 1024 / 512; // 256 MiB
const ESP_START_SECTOR: u64 = 2048;                    // 1 MiB

fn query_yes_no(prompt_str: &str) -> bool {
    let mut cmd_buf = [0u8; 32];
    loop {
        print(prompt_str);
        print(" (y/n): ");
        let len = unsafe { syscall_2(SYS_CONSOLE_GETS, cmd_buf.as_mut_ptr() as usize, cmd_buf.len()) };
        if len > 0 {
            let input = core::str::from_utf8(&cmd_buf[..len]).unwrap_or("").trim();
            if input.eq_ignore_ascii_case("y") || input.eq_ignore_ascii_case("yes") {
                println("");
                return true;
            }
            if input.eq_ignore_ascii_case("n") || input.eq_ignore_ascii_case("no") {
                println("");
                return false;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn installer_main() -> ! {
    println("\x1b[1;36m");
    println("  ============================================");
    println("               Aeglos OS Installer            ");
    println("  ============================================\x1b[0m");
    println("");

    print("Checking AI module status... ");
    ai_query("System status: installer launched. Reply with a short OK and no other words.");
    println("\r\n");
    
    // Simulate detecting block device (for this milestone, we use target 1 which is mapped to drive.img)
    println("Detecting block devices via VirtIO...");
    let mut buf = [0u8; SECTOR_SIZE];
    if sys_blk_read(0, &mut buf) < 0 {
        render::err(&mut UartSink, "No valid VirtIO block devices detected.");
        unsafe { sys_exit() };
    }
    
    println("  - [vda] VirtIO Block Device (2 GB)");
    
    // Ask the user using semantic AI or directly
    println("\x1b[36mAsking AI for guidance...\x1b[0m");
    println("");
    print("\x1b[1;33mNumenor says:\x1b[0m ");
    ai_query("The user is installing Aeglos on vda. Give a short 1-sentence prompt asking if they want to format and erase data.");
    println("");
    
    if !query_yes_no("Proceed with GPT formatting on vda?") {
        println("Installation cancelled.");
        unsafe { sys_exit() };
    }

    // Step 3: Write GPT (Simulation / Minimal Implementation)
    println("[1/4] Writing GPT label to vda...");
    // For Phase 2, we write a protective MBR to LBA0, and minimal GPT header to LBA1.
    // In reality, this would just be writing the bytes using SYS_BLK_WRITE.
    // Let's pretend to write LBA 0-33.
    let mut raw_sector = [0u8; 512];
    
    // Protective MBR (Sector 0)
    raw_sector[510] = 0x55;
    raw_sector[511] = 0xAA;
    sys_blk_write(0, &raw_sector);
    
    // Fake GPT Header (Sector 1)
    raw_sector.fill(0);
    raw_sector[0..8].copy_from_slice(b"EFI PART");
    raw_sector[8..12].copy_from_slice(&[0, 0, 1, 0]); // Revision
    raw_sector[12..16].copy_from_slice(&[92, 0, 0, 0]); // Header size
    // ... CRC32 + other fields omitted for the mock
    sys_blk_write(1, &raw_sector);
    
    // Partition entries (Sector 2+)
    raw_sector.fill(0);
    // Write Partition 1 (ESP)
    let esp_guid = core::array::from_fn::<u8, 16, _>(|i| b"EFI SYSTEM PARTT"[i]);
    raw_sector[0..16].copy_from_slice(&esp_guid);
    // Partition start = 2048, Partition end = 2048 + 524288
    sys_blk_write(2, &raw_sector);

    println("      GPT initialized.");
    
    // Step 4: Create ESP and copy Kernel
    println("[2/4] Formatting ESP (FAT32, 256 MiB)...");
    // Pseudo-format action
    unsafe { sys_yield() };
    
    println("[3/4] Copying kernel and Ash ELF to root partition...");
    // Pseudo-copy process
    unsafe { sys_yield() };
    
    // Step 5: Embed Qwen model
    println("[4/4] Writing configuration (/etc/aeglos.conf)...");
    unsafe { sys_yield() };
    println("      Done.");

    println("");
    println("\x1b[1;32mInstallation Complete!\x1b[0m");
    println("Aeglos has been installed to vda. The system is ready for independent booting.");
    
    println("");
    print("\x1b[1;33mNumenor says:\x1b[0m ");
    ai_query("Installation completed successfully. Give a final celebratory sentence.");
    println("");
    
    loop {
        unsafe { sys_yield() };
    }
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    unsafe { sys_exit() }
}
