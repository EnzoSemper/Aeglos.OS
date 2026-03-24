#![no_std]
#![no_main]

mod boot;
mod dtb;
mod arch;
mod drivers;
mod memory;
mod process;
mod ipc;
mod syscall;
mod wasm;
mod fs;
mod net;
mod boot_logo;
mod platform;
mod smp;
mod csprng;
pub mod config;

use core::panic::PanicInfo;
use drivers::uart::Uart;
use drivers::framebuffer;

/// C++ DSO handle — required so that llama.cpp global constructors can find
/// __dso_handle via ADRP within ±4 GiB.  LLD's internal synthesized copy
/// lands at address 0 (pre-first-section), which after the TTBR1 VMA split
/// is 0xFFFF_0000_0000_0000 away from the high-VA .text section — out of
/// ADRP range.  Defining it here places it in .rodata at high VMA alongside
/// the code, keeping the PC-relative offset within a few hundred KiB.
#[unsafe(export_name = "__dso_handle")]
#[used]
static DSO_HANDLE: u8 = 0;

/// Boot guard — detects re-entry to kernel_main.
/// Placed outside BSS (in .data) so _start's BSS zeroing doesn't reset it.
#[used]
#[link_section = ".data"]
static mut BOOT_GUARD: u32 = 0xDEAD_BEEF;

/// Kernel entry point — called from boot.rs after initial setup.
///
/// `dtb_ptr` is the physical address of the Flattened Device Tree (FDT/DTB)
/// blob, passed in x0 by QEMU / GRUB / UEFI per the ARM64 Linux boot protocol.
/// It may be 0 (null) if no bootloader provides one.
#[no_mangle]
pub extern "C" fn kernel_main(dtb_ptr: *const u8) -> ! {
    let uart = Uart::new();

    // Detect re-entry. If BOOT_GUARD was set to our sentinel on a previous
    // boot, something has re-entered kernel_main without going through _start
    // (which zeros BSS but NOT .data).
    unsafe {
        if BOOT_GUARD == 0x12345678 {
            uart.puts("\r\n!!! KERNEL RE-ENTRY DETECTED !!!\r\n");
            let lr: u64;
            let sp_val: u64;
            core::arch::asm!("mov {}, x30", out(reg) lr);
            core::arch::asm!("mov {}, sp", out(reg) sp_val);
            uart.puts("LR: ");
            uart.put_hex(lr as usize);
            uart.puts("  SP: ");
            uart.put_hex(sp_val as usize);
            uart.puts("\r\n");
            // Print scheduler state
            uart.puts("Scheduler CURRENT slot: ");
            uart.put_dec(crate::process::scheduler::current_slot());
            uart.puts("  TID: ");
            uart.put_dec(crate::process::current_tid());
            uart.puts("\r\n");
            loop { core::hint::spin_loop(); }
        }
        BOOT_GUARD = 0x12345678;
    }

    uart.puts("\r\n");
    uart.puts("============================================\r\n");
    uart.puts("  Aeglos OS v0.1.0 - DEBUG BUILD\r\n");
    uart.puts("  AI-Native Operating System\r\n");
    uart.puts("  Aeglos Systems LLC\r\n");
    uart.puts("============================================\r\n");
    uart.puts("\r\n");


    // --- Exceptions (install early so we see faults) ---
    arch::aarch64::exceptions::init();
    arch::aarch64::exceptions::enable_fp();
    uart.puts("[exc]  Exception vectors installed\r\n");

    // --- DTB: discover hardware from bootloader ---
    let dtb_info = dtb::parse(dtb_ptr);
    let ram_end  = dtb_info.ram_base + dtb_info.ram_size;

    uart.puts("[dtb]  RAM:  base=");
    uart.put_hex(dtb_info.ram_base);
    uart.puts(" size=");
    uart.put_hex(dtb_info.ram_size);
    uart.puts("\r\n");
    uart.puts("[dtb]  CPUs: ");
    uart.put_dec(dtb_info.cpu_count);
    uart.puts(if dtb_info.psci { "  (PSCI)\r\n" } else { "  (spin-table)\r\n" });
    uart.puts("[dtb]  GIC:  dist=");
    uart.put_hex(dtb_info.gic_dist);
    uart.puts(" cpu=");
    uart.put_hex(dtb_info.gic_cpu);
    uart.puts("\r\n");
    uart.puts("[dtb]  UART: base=");
    uart.put_hex(dtb_info.uart_base);
    uart.puts("\r\n");
    if dtb_info.reserved_count > 0 {
        uart.puts("[dtb]  Reserved regions: ");
        uart.put_dec(dtb_info.reserved_count);
        uart.puts("\r\n");
    }

    // --- Page allocator ---
    memory::init(ram_end);
    uart.puts("[mem]  Page allocator initialized\r\n");
    uart.puts("[mem] Debug: total_pages call...\r\n");
    uart.puts("[mem]  Total pages: ");
    uart.put_dec(memory::total_pages());
    uart.puts(" (");
    uart.put_dec(memory::total_pages() * memory::PAGE_SIZE / 1024 / 1024);
    uart.puts(" MiB)\r\n");
    uart.puts("[mem]  Free pages:  ");
    uart.put_dec(memory::free_pages());
    uart.puts("\r\n");

    // --- Virtual memory ---
    let (ttbr0, ttbr1) = memory::vmm::init();
    arch::aarch64::mmu::init(ttbr0, ttbr1);
    uart.puts("[mmu]  Virtual memory enabled (TTBR1 kernel/user split)\r\n");

    // --- Heap ---
    // Heap init uses atomics (ALLOCATOR.lock()). Atomics fault if MMU is OFF on real hardware.
    // Must be done AFTER mmu::init.
    memory::heap::init();
    uart.puts("[mem]  Kernel Heap initialized\r\n");

    // Framebuffer init moved to after VirtIO init

    uart.puts("[mem]  Free pages:  ");
    uart.put_dec(memory::free_pages());
    uart.puts("\r\n");

    // --- Platform detection ---
    let plat = platform::detect();
    uart.puts("[plat] CPU: ");
    uart.puts(plat.name());
    uart.puts("\r\n");

    // --- Interrupt controller ---
    // Apple Silicon uses AIC (Apple Interrupt Controller) instead of GICv2.
    // The AIC base comes from the Device Tree `aic` node; if not found in the
    // DTB, fall back to GIC (QEMU or generic AArch64 board).
    if plat.is_apple() && dtb_info.aic_base != 0 {
        arch::aarch64::aic::set_base(dtb_info.aic_base);
        arch::aarch64::aic::init();
        uart.puts("[aic]  Apple Interrupt Controller initialized\r\n");
        // On Apple Silicon the virtual timer fires as FIQ.
        // The FIQ vector is already wired to fiq_trampoline → fiq_handler_inner.
        // Enable FIQ delivery by clearing the F bit in DAIF.
        unsafe {
            core::arch::asm!("msr daifclr, #1"); // clear F bit → FIQs unmasked
        }
        uart.puts("[aic]  FIQ unmasked (timer via FIQ)\r\n");
    } else {
        arch::aarch64::gic::init();
        uart.puts("[gic]  GICv2 initialized\r\n");
    }

    // --- SMP: wake secondary cores (after interrupt controller, before timer) ---
    smp::init(dtb_info.cpu_count, dtb_info.psci);

    // Initialize VirtIO Block Device
    unsafe { crate::drivers::virtio::init(); }
    unsafe { drivers::virtio_gpu::init(); }

    // Initialize VirtIO Network Device (optional — not present in basic QEMU run)
    unsafe { drivers::virtio_net::init(); }

    // Initialize VirtIO Input Device (keyboard/mouse)
    unsafe { drivers::virtio_input::init(); }

    // Pass DTB-discovered PCIe ECAM hint to the PCIe driver so real hardware
    // gets the correct config space base without hardcoded QEMU constants.
    if dtb_info.pcie_ecam != 0 {
        crate::drivers::pcie::set_dtb_ecam_hint(dtb_info.pcie_ecam);
    }

    // If the DTB describes a SimpleFB (e.g. from U-Boot splash), initialise it
    // so framebuffer::init() can pick it up even before VirtIO GPU probing.
    if dtb_info.fb_base != 0 && dtb_info.fb_width != 0 {
        let stride_px = if dtb_info.fb_stride > 0 {
            dtb_info.fb_stride / 4 // bytes → pixels (assume 4 bpp)
        } else {
            dtb_info.fb_width
        };
        unsafe {
            crate::drivers::simplefb::init(
                dtb_info.fb_base as u64,
                dtb_info.fb_width,
                dtb_info.fb_height,
                stride_px,
                crate::drivers::simplefb::FMT_BGR,
            );
        }
        uart.puts("[fb]   DTB SimpleFB hint applied\r\n");
    }

    // PCIe enumeration — discover real hardware NICs (e.g. Intel e1000 on QEMU with -device e1000)
    {
        let pcie_devs = crate::drivers::pcie::enumerate();
        for dev in &pcie_devs {
            // Intel e1000 family: 82540EM (0x100E), e1000e (0x10D3), 82545EM (0x100F)
            if dev.vendor == 0x8086
                && (dev.device == 0x100e || dev.device == 0x10d3 || dev.device == 0x100f)
            {
                if crate::drivers::e1000::init(dev.bar0) {
                    uart.puts("[e1000] Intel NIC initialized via PCIe\r\n");
                }
            }
            // Intel HDA audio: 0x8086:0x2668 (QEMU intel-hda)
            if dev.vendor == 0x8086 && dev.device == 0x2668 {
                crate::drivers::hda::init(dev.bar0);
            }
            // NVMe storage (class=0x01, subclass=0x08 = NVMe): QEMU nvme + generic
            if dev.class == 0x01 && dev.subclass == 0x08 {
                if crate::drivers::nvme::init(dev.bar0) {
                    uart.puts("[nvme] NVMe block device ready\r\n");
                }
            } else if crate::drivers::nvme::is_apple_nvme(dev.vendor, dev.device) {
                if crate::drivers::nvme::init(dev.bar0) {
                    uart.puts("[nvme] Apple ANS2 NVMe ready\r\n");
                }
            }
            // Apple ATCP USB-PD controller
            if crate::drivers::usb_pd::is_atcp_device(dev.vendor, dev.device) {
                crate::drivers::usb_pd::init(dev.bar0);
            }
        }
    }

    // --- Framebuffer ---
    if framebuffer::init() {
        uart.puts("[fb]   Framebuffer initialized\r\n");

        let white = framebuffer::rgb(255, 255, 255);
        let accent = framebuffer::rgb(100, 160, 255);
        let dim = framebuffer::rgb(120, 120, 140);

        let (mut fb_w, mut fb_h) = framebuffer::dimensions();
        if fb_w == 0 { fb_w = 640; } // Fallback
        if fb_h == 0 { fb_h = 360; }

        // Draw Aeglos Boot Logo (Generated)
        let logo_x = (fb_w.saturating_sub(boot_logo::LOGO_W)) / 2;
        let logo_y = (fb_h.saturating_sub(boot_logo::LOGO_H + 40)) / 2;
        
        let mut idx = 0;
        for y in 0..boot_logo::LOGO_H {
            for x in 0..boot_logo::LOGO_W {
                let px = boot_logo::LOGO_PIXELS[idx];
                if px != 0 { // simple alpha test
                    framebuffer::put_pixel(logo_x + x, logo_y + y, px);
                }
                idx += 1;
            }
        }
        
        let line_y = logo_y + boot_logo::LOGO_H + 20;

        // Subtitle
        let subtitle = "AI-Native Operating System";
        let sub_w = subtitle.len() * 8;
        let sub_x = (fb_w - sub_w) / 2;
        let sub_y = line_y;
        framebuffer::draw_string(sub_x, sub_y, subtitle, dim);

        // Version
        let ver = "v0.1.0 - Phase 3: Pixel-to-Metal";
        let ver_w = ver.len() * 8;
        let ver_x = (fb_w - ver_w) / 2;
        let ver_y = sub_y + 16;
        framebuffer::draw_string(ver_x, ver_y, ver, dim);

        framebuffer::flush();
        uart.puts("[fb]   Boot screen rendered. Delaying 10s...\r\n");

        // Simple loop based on physical timer to simulate sleep before loading OS
        let freq: u64;
        unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq); }
        let start: u64;
        unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) start); }
        let end = start + freq * 10;
        
        loop {
            let now: u64;
            unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) now); }
            if now >= end { break; }
        }

        // Stop the boot chiptune exactly as the logo screen ends.
        crate::drivers::hda::stop();
    } else {
        uart.puts("[fb]   Framebuffer not available\r\n");
    }

    // Initialize network stack (ARP/IPv4/ICMP/UDP)
    net::init();

    // Establish network connectivity at boot via DNS query to QEMU Virtual DNS (10.0.2.3:53)
    let dns_payload = [
        0x12, 0x34, // Transaction ID
        0x01, 0x00, // Flags: Standard query
        0x00, 0x01, // Questions: 1
        0x00, 0x00, // Answer RRs: 0
        0x00, 0x00, // Authority RRs: 0
        0x00, 0x00, // Additional RRs: 0
        0x06, b'g', b'o', b'o', b'g', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, // Q: google.com
        0x00, 0x01, // Type: A
        0x00, 0x01  // Class: IN
    ];
    if net::udp_send([10, 0, 2, 3], 53, 12345, &dns_payload) {
        uart.puts("[net]  DNS TX (google.com) sent to 10.0.2.3. Network stack active.\r\n");
    } else {
        uart.puts("[net]  DNS TX failed: Network offline.\r\n");
    }

    // Initialize FAT32 filesystem (sector 0 of block device; no-op if not formatted)
    fs::fat32::init(0);

    // Load persistent configuration from /config on FAT32
    config::init();
    uart.puts("[cfg]  Persistent config loaded\r\n");

    // VirtIO write + read + verify test
    {
        let buf_page = memory::alloc_page().expect("failed to alloc buf");
        // buf_page is a PA; CPU must access it via the high-VA alias.
        let buf = unsafe { core::slice::from_raw_parts_mut(
            memory::vmm::phys_to_virt(buf_page) as *mut u8, 512,
        ) };

        // Write a known pattern to a high sector (avoid semantic data region)
        for i in 0..512 {
            buf[i] = (i & 0xFF) as u8;
        }
        uart.puts("[virtio] Writing sector 19999...\r\n");
        unsafe { drivers::virtio::write_block(19999, buf); }

        // Clear buffer, then read back
        for b in buf.iter_mut() { *b = 0; }
        uart.puts("[virtio] Reading sector 19999...\r\n");
        unsafe { drivers::virtio::read_block(19999, buf); }

        // Verify
        let mut ok = true;
        for i in 0..512 {
            if buf[i] != (i & 0xFF) as u8 {
                uart.puts("[virtio] MISMATCH at offset ");
                uart.put_dec(i);
                uart.puts(": expected ");
                uart.put_hex((i & 0xFF) as usize);
                uart.puts(" got ");
                uart.put_hex(buf[i] as usize);
                uart.puts("\r\n");
                ok = false;
                break;
            }
        }
        if ok {
            uart.puts("[virtio] TEST PASSED: write/read/verify OK\r\n");
        }
    }

    // --- Remoboth: Task management ---
    process::init();
    uart.puts("[task] Remoboth initialized\r\n");



    // -------------------------------------------------------------
    // TEMPORARILY DISABLED for fast booting and UI testing:
    // -------------------------------------------------------------
    // Spawn Numenor (TID 1) — pinned to CPU 1 for dedicated inference.
    if true {
        match process::spawn("numenor", numenor::main) {
            Ok(tid) => {
                uart.puts("[task] Spawned Numenor (TID ");
                uart.put_dec(tid);
                uart.puts(")\r\n");
                // Pin Numenor to CPU 1 so LLM inference runs on a dedicated
                // core and does not compete with interactive tasks on CPU 0.
                process::set_affinity(tid, 1);
            }
            Err(e) => {
                uart.puts("[task] Failed to spawn numenor: ");
                uart.puts(e);
                uart.puts("\r\n");
            }
        }
    }

    // Spawn Semantic Service (TID 2)
    match process::spawn("semantic", semantic::main) {
        Ok(tid) => {
            uart.puts("[task] Spawned Semantic (TID ");
            uart.put_dec(tid);
            uart.puts(")\r\n");
        }
        Err(e) => {
            uart.puts("[task] Failed to spawn semantic: ");
            uart.puts(e);
            uart.puts("\r\n");
        }
    }

    // Spawn AI/Semantic Test Task (TID 3)
    match process::spawn("sys_test", sys_test_task) { 
        Ok(tid) => {
            uart.puts("[task] Spawned sys_test (TID ");
            uart.put_dec(tid);
            uart.puts(")\r\n");
        }
        Err(e) => {
            uart.puts("[task] Failed to spawn sys_test: ");
            uart.puts(e);
            uart.puts("\r\n");
        }
    }
    // Spawn HTTP server (REST API for AI inference + system status on port 80)
    match process::spawn("httpd", net::httpd::httpd_task) {
        Ok(tid) => {
            uart.puts("[task] Spawned httpd (TID ");
            uart.put_dec(tid);
            uart.puts(") → http://");
            let ip = net::get_ip();
            for (i, &b) in ip.iter().enumerate() {
                uart.put_dec(b as usize);
                if i < 3 { uart.puts("."); }
            }
            uart.puts(":80/\r\n");
        }
        Err(e) => {
            uart.puts("[task] Failed to spawn httpd: ");
            uart.puts(e);
            uart.puts("\r\n");
        }
    }

    // Spawn Ash (TID X) — EL0 userspace shell binary embedded at build time.
    {
        static ASH_ELF: &[u8] = include_bytes!(
            "../../target/aarch64-unknown-none/release/ash"
        );
        match process::spawn_elf("ash", ASH_ELF, crate::syscall::CAP_USER_DEFAULT) {
            Ok(tid) => {
                uart.puts("[task] Spawned Ash EL0 (TID ");
                uart.put_dec(tid);
                uart.puts(")\r\n");
            }
            Err(e) => {
                uart.puts("[task] Failed to spawn ash: ");
                uart.puts(e);
                uart.puts("\r\n");
            }
        }
    }

    // --- WASM smoke test: smallest valid module (add function) ---
    {
        // Hand-crafted minimal WASM: (func (param i32 i32) (result i32) local.get 0 local.get 1 i32.add)
        // exported as "add"
        #[rustfmt::skip]
        static WASM_ADD: &[u8] = &[
            0x00, 0x61, 0x73, 0x6D, // magic \0asm
            0x01, 0x00, 0x00, 0x00, // version 1
            // Type section: (i32,i32)->i32
            0x01, 0x07, 0x01, 0x60, 0x02, 0x7F, 0x7F, 0x01, 0x7F,
            // Function section: func 0 uses type 0
            0x03, 0x02, 0x01, 0x00,
            // Export section: "add" = func 0
            0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00,
            // Code section: body = local.get 0, local.get 1, i32.add, end
            0x0A, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6A, 0x0B,
        ];
        match wasm::load(WASM_ADD) {
            Ok(module) => {
                match module.call_export("add", &[7, 35]) {
                    Ok(results) => {
                        let sum = results.first().copied().unwrap_or(0);
                        uart.puts("[wasm] smoke test: 7+35=");
                        uart.put_dec(sum as usize);
                        uart.puts(if sum == 42 { " OK\r\n" } else { " FAIL\r\n" });
                    }
                    Err(_e) => {
                        uart.puts("[wasm] call failed\r\n");
                    }
                }
            }
            Err(_e) => {
                uart.puts("[wasm] load failed\r\n");
            }
        }
    }

    // --- Timer (100 Hz for preemptive scheduling) ---
    arch::aarch64::timer::init();
    uart.puts("[tmr]  Timer started (100 Hz)\r\n");

    // --- CSPRNG: seed ChaCha20-DRBG from hardware entropy ---
    // Must be called after timer init (jitter accumulator needs a running counter).
    csprng::init();
    uart.puts("[rng]  CSPRNG initialized");
    uart.puts(if crate::csprng::has_rndr_available() {
        " (RNDR hardware)\r\n"
    } else {
        " (timer-jitter)\r\n"
    });

    // Enable IRQs — timer interrupts drive the scheduler
    arch::aarch64::exceptions::enable_irqs();
    uart.puts("[cpu]  IRQs enabled\r\n");

    // --- Input / UI Testing ---
    uart.puts("[input] Ready for input events...\r\n");
    let mut mouse_x = 640_usize;
    let mut mouse_y = 360_usize;
    let mut mouse_down = false;

    // Idle loop — task 0. Scheduler preempts us to run other tasks.
    loop {
        unsafe { core::arch::asm!("wfi", options(nostack)); }
        
        // Yield to other tasks
        call_sys_yield();
    }
}

fn sys_test_task() -> ! {
    let uart = Uart::new();
    uart.puts("[test] === Semantic Memory Test Suite ===\r\n");

    // Helper: send to semantic service (TID 2) and wait for reply
    let sem_tid: usize = 2;

    // ── Test 1: STORE (plain, no metadata) ──
    let test_data = "Hello, Semantic World! This is a test.";
    let mut hash_buf = [0u8; 32];

    uart.puts("[test] 1. STORE (plain)...\r\n");
    {
        let mut req = [0u8; 32];
        req[0..8].copy_from_slice(&100u64.to_le_bytes());
        req[8..16].copy_from_slice(&(test_data.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(test_data.len() as u64).to_le_bytes());
        req[24..32].copy_from_slice(&(hash_buf.as_mut_ptr() as u64).to_le_bytes());
        sem_send_wait(&uart, sem_tid, req, |reply| {
            let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
            if status == 0 {
                uart.puts("   OK. Hash: ");
                for b in &hash_buf[0..4] { uart.put_hex(*b as usize); }
                uart.puts("...\r\n");
            } else {
                uart.puts("   FAIL: status ");
                uart.put_dec(status as usize);
                uart.puts("\r\n");
            }
        });
    }

    // ── Test 2: RETRIEVE by hash ──
    uart.puts("[test] 2. RETRIEVE by hash...\r\n");
    {
        let mut out_buf = [0u8; 128];
        let mut req = [0u8; 32];
        req[0..8].copy_from_slice(&101u64.to_le_bytes());
        req[8..16].copy_from_slice(&(hash_buf.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(out_buf.as_mut_ptr() as u64).to_le_bytes());
        req[24..32].copy_from_slice(&(out_buf.len() as u64).to_le_bytes());
        sem_send_wait(&uart, sem_tid, req, |reply| {
            let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
            let len = u64::from_le_bytes(reply[8..16].try_into().unwrap());
            if status == 0 {
                uart.puts("   OK. Content: ");
                if let Ok(s) = core::str::from_utf8(&out_buf[..len as usize]) {
                    uart.puts(s);
                }
                uart.puts("\r\n");
            } else {
                uart.puts("   FAIL\r\n");
            }
        });
    }

    // ── Test 3: STORE_META (with metadata + tags) ──
    let tagged_data = "Aeglos kernel boot log: all systems nominal.";
    let mut meta = semantic::metadata::Metadata::new(1); // content_type=1 (Text)
    // Set tags
    let tag_str = b"boot,kernel,log";
    meta.tags[..tag_str.len()].copy_from_slice(tag_str);

    uart.puts("[test] 3. STORE_META (tagged)...\r\n");
    {
        let mut req = [0u8; 32];
        req[0..8].copy_from_slice(&102u64.to_le_bytes());
        req[8..16].copy_from_slice(&(tagged_data.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(tagged_data.len() as u64).to_le_bytes());
        req[24..32].copy_from_slice(&(&meta as *const _ as u64).to_le_bytes());
        sem_send_wait(&uart, sem_tid, req, |reply| {
            let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
            if status == 0 {
                uart.puts("   OK. Tags: \"boot,kernel,log\"\r\n");
            } else {
                uart.puts("   FAIL: status ");
                uart.put_dec(status as usize);
                uart.puts("\r\n");
            }
        });
    }

    // ── Test 4: SEARCH by tag ──
    uart.puts("[test] 4. SEARCH by tag \"kernel\"...\r\n");
    {
        let tag = "kernel";
        let mut result_hashes = [0u8; 256]; // up to 8 hashes
        let mut req = [0u8; 32];
        req[0..8].copy_from_slice(&103u64.to_le_bytes());
        req[8..16].copy_from_slice(&(tag.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(tag.len() as u64).to_le_bytes());
        req[24..32].copy_from_slice(&(result_hashes.as_mut_ptr() as u64).to_le_bytes());
        sem_send_wait(&uart, sem_tid, req, |reply| {
            let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
            let count = u64::from_le_bytes(reply[8..16].try_into().unwrap());
            if status == 0 {
                uart.puts("   OK. Found ");
                uart.put_dec(count as usize);
                uart.puts(" result(s)\r\n");
            } else {
                uart.puts("   FAIL\r\n");
            }
        });
    }

    // ── Test 5: QUERY by keyword ──
    uart.puts("[test] 5. QUERY keyword \"Semantic\"...\r\n");
    {
        let keyword = "Semantic";
        let mut result_hashes = [0u8; 256];
        let mut req = [0u8; 32];
        req[0..8].copy_from_slice(&104u64.to_le_bytes());
        req[8..16].copy_from_slice(&(keyword.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(keyword.len() as u64).to_le_bytes());
        req[24..32].copy_from_slice(&(result_hashes.as_mut_ptr() as u64).to_le_bytes());
        sem_send_wait(&uart, sem_tid, req, |reply| {
            let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
            let count = u64::from_le_bytes(reply[8..16].try_into().unwrap());
            if status == 0 {
                uart.puts("   OK. Found ");
                uart.put_dec(count as usize);
                uart.puts(" result(s)\r\n");
            } else {
                uart.puts("   FAIL\r\n");
            }
        });
    }

    // ── Test 6: PRELOAD RAG Data ──
    uart.puts("[test] 6. PRELOAD RAG Data...\r\n");
    {
        // 1. "plan"
        let secret_data = "The secret plan is: 1. Build OS. 2. Integrate AI. 3. ??? 4. Profit.";
        let mut req = [0u8; 32];
        let mut hash_buf = [0u8; 32];
        req[0..8].copy_from_slice(&100u64.to_le_bytes()); // OP_STORE
        req[8..16].copy_from_slice(&(secret_data.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(secret_data.len() as u64).to_le_bytes());
        req[24..32].copy_from_slice(&(hash_buf.as_mut_ptr() as u64).to_le_bytes());
        sem_send_wait(&uart, sem_tid, req, |reply| {
            let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
            if status == 0 {
                 uart.puts("   OK. Stored 'plan'.\r\n");
            } else {
                 uart.puts("   FAIL.\r\n");
            }
        });
        
        // 2. "notes"
        let notes_data = "My notes: RAG integration is working if you see this.";
        req[8..16].copy_from_slice(&(notes_data.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(notes_data.len() as u64).to_le_bytes());
        sem_send_wait(&uart, sem_tid, req, |reply| {
             uart.puts("   OK. Stored 'notes'.\r\n");
        });
    }

    // ── Test 7: AI INFER (Numenor) ──
    uart.puts("[test] 7. AI INFER (Numenor)...\r\n");
    {
        let prompt = "Who are you?";
        let mut req = [0u8; 32];
        req[0..8].copy_from_slice(&2u64.to_le_bytes()); // AI_OP_INFER = 2
        req[8..16].copy_from_slice(&(prompt.as_ptr() as u64).to_le_bytes());
        req[16..24].copy_from_slice(&(prompt.len() as u64).to_le_bytes());
        
        // We'll treat the reply data[8..16] as ptr and data[16..24] as len
        sem_send_wait(&uart, 1, req, |reply| { // TID 1 is Numenor
            let ptr = u64::from_le_bytes(reply[8..16].try_into().unwrap()) as *const u8;
            let len = u64::from_le_bytes(reply[16..24].try_into().unwrap()) as usize;
            
            uart.puts("   Response ptr: ");
            uart.put_hex(ptr as usize);
            uart.puts(" len: ");
            uart.put_dec(len);
            uart.puts("\r\n");

            if len > 0 && len < 1024 {
                let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
                if let Ok(s) = core::str::from_utf8(slice) {
                    uart.puts("   Result: \"");
                    uart.puts(s);
                    uart.puts("\"\r\n");
                } else {
                    uart.puts("   Result: <invalid utf8>\r\n");
                }
            }
        });
    }

    // ── Test 8: STORE_VEC + VECTOR_SEARCH round-trip ──
    uart.puts("[test] 8. VECTOR_SEARCH round-trip...\r\n");
    {
        // Unit embedding: dim[0] = 1.0, rest = 0.0 → cosine sim with itself = 1.0
        let mut emb_buf = [0u8; 1536];
        emb_buf[0..4].copy_from_slice(&1.0f32.to_le_bytes());

        // Result buffer: 40 bytes per slot × 8 slots max
        let mut result_buf = [0u8; 320];

        // STORE_VEC (106)
        let vec_text = "Vector search test: AI-indexed document.";
        {
            let mut req = [0u8; 32];
            req[0..8].copy_from_slice(&106u64.to_le_bytes());
            req[8..16].copy_from_slice(&(vec_text.as_ptr() as u64).to_le_bytes());
            req[16..24].copy_from_slice(&(vec_text.len() as u64).to_le_bytes());
            req[24..32].copy_from_slice(&(emb_buf.as_ptr() as u64).to_le_bytes());
            sem_send_wait(&uart, sem_tid, req, |reply| {
                let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
                if status == 0 {
                    uart.puts("   STORE_VEC OK.\r\n");
                } else {
                    uart.puts("   STORE_VEC FAIL: status ");
                    uart.put_dec(status as usize);
                    uart.puts("\r\n");
                }
            });
        }

        // VECTOR_SEARCH (105) with same embedding, threshold 0.9
        {
            let thr_bits = 0.9f32.to_bits();
            let mut req = [0u8; 32];
            req[0..8].copy_from_slice(&105u64.to_le_bytes());
            req[8..16].copy_from_slice(&(emb_buf.as_ptr() as u64).to_le_bytes());
            req[16..20].copy_from_slice(&thr_bits.to_le_bytes()); // f32 bits
            // req[20..24] = 0 (pad)
            req[24..32].copy_from_slice(&(result_buf.as_mut_ptr() as u64).to_le_bytes());
            sem_send_wait(&uart, sem_tid, req, |reply| {
                let status = u64::from_le_bytes(reply[0..8].try_into().unwrap());
                let count  = u64::from_le_bytes(reply[8..16].try_into().unwrap());
                if status == 0 && count > 0 {
                    // Similarity is bytes 32-35 of the first 40-byte result slot
                    let sim_bits = u32::from_le_bytes([
                        result_buf[32], result_buf[33], result_buf[34], result_buf[35],
                    ]);
                    let sim = f32::from_bits(sim_bits);
                    uart.puts("   VECTOR_SEARCH OK. count=");
                    uart.put_dec(count as usize);
                    uart.puts(" sim=");
                    uart.put_dec((sim * 100.0) as usize);
                    uart.puts("%\r\n");
                } else {
                    uart.puts("   VECTOR_SEARCH FAIL: status=");
                    uart.put_dec(status as usize);
                    uart.puts(" count=");
                    uart.put_dec(count as usize);
                    uart.puts("\r\n");
                }
            });
        }
    }

    // ── Test 9: HTTP GET (end-to-end TCP + DNS + HTTP) ──
    uart.puts("[test] 9. HTTP GET (end-to-end)...\r\n");
    {
        // Wait for DHCP to complete before making network requests.
        // DHCP should finish within ~2s; yield 300 ticks (3s) to be safe.
        for _ in 0..300 { call_sys_yield(); }

        uart.puts("   Net IP: ");
        { let ip = crate::net::get_ip(); uart.put_dec(ip[0] as usize); uart.puts("."); uart.put_dec(ip[1] as usize); uart.puts("."); uart.put_dec(ip[2] as usize); uart.puts("."); uart.put_dec(ip[3] as usize); uart.puts("\r\n"); }

        // ── Step A: Local TCP sanity check ────────────────────────────────────
        // Connect to 10.0.2.2:9999 (SLIRP gateway, closed port).
        // SLIRP maps 10.0.2.2→localhost; port 9999 is closed → host OS sends RST.
        // RST arriving means VirtIO TX+RX and TCP state machine all work.
        uart.puts("   [A] Local TCP test -> 10.0.2.2:9999...\r\n");
        let local_gw = crate::net::IpAddr::V4([10, 0, 2, 2]);
        match crate::net::tcp::tcp_connect(local_gw, 9999) {
            None => uart.puts("   [A] tcp_connect FAIL (ARP?)\r\n"),
            Some(id) => {
                if !crate::net::tcp::tcp_wait_established(id, 3000) {
                    let st = crate::net::tcp::tcp_state(id);
                    if st == crate::net::tcp::TcpState::Closed {
                        uart.puts("   [A] PASS — got RST (VirtIO RX works)\r\n");
                    } else {
                        uart.puts("   [A] FAIL — timeout (VirtIO RX broken?)\r\n");
                    }
                } else {
                    uart.puts("   [A] Unexpected ESTABLISHED on closed port\r\n");
                }
                crate::net::tcp::tcp_close(id);
            }
        }

        // ── Step B: HTTP GET via SLIRP host-forward ──────────────────────────
        // build.sh starts a Python HTTP server on localhost:19999.
        // SLIRP maps guest→10.0.2.2:19999 to host→127.0.0.1:19999.
        // This test is internet-independent: it verifies the full HTTP GET path.
        uart.puts("   [B] HTTP GET http://10.0.2.2:19999/ (local server)...\r\n");
        {
            let local_http = crate::net::IpAddr::V4([10, 0, 2, 2]);
            match crate::net::tcp::tcp_connect(local_http, 19999) {
                None => uart.puts("   [B] tcp_connect FAIL\r\n"),
                Some(id) => {
                    if !crate::net::tcp::tcp_wait_established(id, 5000) {
                        let st = crate::net::tcp::tcp_state(id);
                        if st == crate::net::tcp::TcpState::Closed {
                            uart.puts("   [B] RST — server not running (start python3 -m http.server 19999)\r\n");
                        } else {
                            uart.puts("   [B] TIMEOUT — host-forward not working?\r\n");
                        }
                        crate::net::tcp::tcp_close(id);
                    } else {
                        uart.puts("   [B] ESTABLISHED. Sending HTTP GET...\r\n");
                        let req = b"GET / HTTP/1.1\r\nHost: 10.0.2.2:19999\r\nConnection: close\r\nUser-Agent: Aeglos/1.0\r\n\r\n";
                        let sent = crate::net::tcp::tcp_write(id, req);
                        uart.puts("   [B] Sent "); uart.put_dec(sent); uart.puts(" bytes\r\n");
                        let mut buf = [0u8; 2048];
                        if crate::net::tcp::tcp_wait_readable(id, 8000) {
                            let n = crate::net::tcp::tcp_read(id, &mut buf);
                            uart.puts("   [B] HTTP response: ");
                            uart.put_dec(n); uart.puts(" bytes\r\n");
                            uart.puts("   [B] Status line: \"");
                            let preview_len = n.min(16);
                            if let Ok(s) = core::str::from_utf8(&buf[..preview_len]) {
                                uart.puts(s);
                            }
                            uart.puts("\"\r\n");
                            uart.puts("   [B] PASS — HTTP GET complete\r\n");
                        } else {
                            uart.puts("   [B] HTTP read timeout\r\n");
                        }
                        crate::net::tcp::tcp_close(id);
                    }
                }
            }
        }
    }

    // ── Step C: External TCP (neverssl.com:80) — proves SLIRP routes externally ─
    uart.puts("   [C] External TCP -> neverssl.com:80...\r\n");
    {
        let neverssl_ip = crate::net::dns::dns_resolve("neverssl.com");
        match neverssl_ip {
            None => uart.puts("   [C] DNS FAIL — neverssl.com did not resolve\r\n"),
            Some(ip) => {
                uart.puts("   [C] DNS OK, IP=");
                crate::net::put_ipaddr(ip);
                uart.puts("\r\n");
                match crate::net::tcp::tcp_connect(ip, 80) {
                    None => uart.puts("   [C] tcp_connect FAIL (ARP?)\r\n"),
                    Some(id) => {
                        if crate::net::tcp::tcp_wait_established(id, 8000) {
                            uart.puts("   [C] PASS — external TCP established\r\n");
                        } else {
                            let st = crate::net::tcp::tcp_state(id);
                            if st == crate::net::tcp::TcpState::Closed {
                                uart.puts("   [C] FAIL — RST (SLIRP rejected external TCP)\r\n");
                            } else {
                                uart.puts("   [C] FAIL — timeout (no SYN-ACK from internet)\r\n");
                            }
                        }
                        crate::net::tcp::tcp_close(id);
                    }
                }
            }
        }
    }

    // ── Step D: Full HTTP GET to neverssl.com ─────────────────────────────────
    uart.puts("   [D] HTTP GET http://neverssl.com/...\r\n");
    {
        let mut buf = [0u8; 2048];
        match crate::net::http::http_get("neverssl.com", "/", 80, &mut buf) {
            crate::net::http::HttpResult::Ok(n) => {
                uart.puts("   [D] PASS — ");
                uart.put_dec(n);
                uart.puts(" bytes, status line: \"");
                let preview = n.min(16);
                if let Ok(s) = core::str::from_utf8(&buf[..preview]) { uart.puts(s); }
                uart.puts("\"\r\n");
            }
            crate::net::http::HttpResult::DnsError  => uart.puts("   [D] FAIL — DNS error\r\n"),
            crate::net::http::HttpResult::TcpError  => uart.puts("   [D] FAIL — TCP error\r\n"),
            crate::net::http::HttpResult::Timeout   => uart.puts("   [D] FAIL — timeout\r\n"),
            crate::net::http::HttpResult::HttpError(c) => {
                uart.puts("   [D] FAIL — HTTP "); uart.put_dec(c as usize); uart.puts("\r\n");
            }
            crate::net::http::HttpResult::BufferTooSmall => uart.puts("   [D] PASS (buf small)\r\n"),
        }
    }

    // ── Step E: ICMP ping 10.0.2.2 ────────────────────────────────────────────
    uart.puts("   [E] ICMP ping 10.0.2.2...\r\n");
    {
        let rtt = crate::net::send_ping([10, 0, 2, 2], 2000);
        if rtt >= 0 {
            uart.puts("   [E] PASS — RTT="); uart.put_dec(rtt as usize); uart.puts(" ms\r\n");
        } else {
            uart.puts("   [E] FAIL — no ICMP reply (SLIRP may not support ICMP on macOS)\r\n");
        }
    }

    uart.puts("[test] === All tests complete ===\r\n");
    uart.puts("[test] Done. Idling...\r\n");
    loop {
        call_sys_yield();
    }
}

/// Send a message to the semantic service and wait for the reply.
fn sem_send_wait(
    _uart: &Uart,
    tid: usize,
    req_data: [u8; 32],
    on_reply: impl FnOnce(&[u8; 32]),
) {
    let msg = crate::ipc::Message { sender: 0, data: req_data };
    if call_sys_send(tid, &msg) == 0 {
        let mut reply = crate::ipc::Message { sender: 0, data: [0; 32] };
        loop {
            let ret = call_sys_recv(&mut reply);
            if ret == 0 && reply.sender == tid {
                on_reply(&reply.data);
                return;
            }
        }
    }
}

// --- Syscall Wrappers (Inline Assembly) ---

#[inline(never)]
fn call_sys_send(target_tid: usize, msg: &crate::ipc::Message) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "mov x8, #1", // SYS_SEND
            "svc #0",
            in("x0") target_tid,
            in("x1") msg as *const _ as usize,
            lateout("x0") ret,
            out("x8") _,
            clobber_abi("system"),
        );
    }
    ret
}

#[inline(never)]
fn call_sys_recv(msg: &mut crate::ipc::Message) -> isize {
    let mut ret: isize;
    loop {
        unsafe {
            core::arch::asm!(
                "mov x8, #2", // SYS_RECV
                "svc #0",
                in("x0") msg as *mut _ as usize,
                lateout("x0") ret,
                out("x8") _,
                clobber_abi("system"),
            );
        }
        if ret != 1 { // 1 = BLOCKED/RETRY
            return ret;
        }
    }
}

#[inline(never)]
#[no_mangle]
pub extern "C" fn sys_console_puts(s: *const u8) {
    use core::ffi::CStr;
    // Cast to *const c_char (which might be u8 or i8 depending on target)
    let s = unsafe { CStr::from_ptr(s.cast()) };
    if let Ok(msg) = s.to_str() {
        let uart = Uart::new();
        uart.puts(msg);
    }
}

fn call_sys_yield() {
    unsafe {
        core::arch::asm!(
            "mov x8, #3", // SYS_YIELD
            "svc #0",
            out("x0") _,
            out("x8") _,
            clobber_abi("system"),
        );
    }
}

#[inline(never)]
fn call_sys_exit() -> ! {
    loop {
        loop {
            unsafe {
                core::arch::asm!(
                    "mov x8, #4", // SYS_EXIT
                    "svc #0",
                    options(noreturn),
                );
            }
        }
    }
}

#[inline(never)]
unsafe fn call_sys_ai(op: usize, arg1: usize, arg2: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "mov x8, #5", // SYS_AI_CALL
        "svc #0",
        in("x0") op,
        in("x1") arg1,
        in("x2") arg2,
        lateout("x0") ret,
        out("x8") _,
        clobber_abi("system"),
    );
    ret
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let uart = Uart::new();
    uart.puts("\r\n!!! KERNEL PANIC !!!\r\n");
    if let Some(location) = info.location() {
        uart.puts("  at ");
        uart.puts(location.file());
        uart.puts(":");
        uart.put_dec(location.line() as usize);
        uart.puts("\r\n");
    }
    loop {
        core::hint::spin_loop();
    }
}


