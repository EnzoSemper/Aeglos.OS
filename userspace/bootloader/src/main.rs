//! Aeglos OS UEFI bootloader — `userspace/bootloader`
//!
//! A minimal PE/COFF EFI application compiled for `aarch64-unknown-uefi`.
//! UEFI firmware loads this as `EFI/BOOT/BOOTAA64.EFI` and calls `main`.
//!
//! Boot sequence:
//!   1. Open the boot filesystem (the FAT partition this file was loaded from)
//!   2. Read `\boot\aeglos.bin` into physical memory at KERNEL_LOAD_PA
//!   3. Validate the 64-byte ARM64 Linux Image header (magic = "ARM\x64")
//!   4. Query GOP for the linear framebuffer; write BootInfo at BOOT_INFO_PA
//!   5. Locate the Flattened Device Tree via the UEFI configuration table
//!   6. Exit UEFI boot services
//!   7. Jump to kernel entry with DTB pointer in x0 (ARM64 Linux protocol)

#![no_std]
#![no_main]

use uefi::{
    prelude::*,
    proto::{
        loaded_image::LoadedImage,
        media::{
            file::{File, FileAttribute, FileMode, FileType},
            fs::SimpleFileSystem,
        },
        console::gop::{GraphicsOutput, PixelFormat},
    },
    table::boot::{AllocateType, MemoryType},
    Guid,
};

/// EFI Flattened Device Tree table GUID: b1b621d5-f19c-41a5-830b-d9152c69aae0
///
/// UEFI firmware (OVMF / EDK II) stores the DTB pointer here when it has one.
/// QEMU with `-machine virt` always provides a DTB via this table.
const EFI_DTB_TABLE_GUID: Guid =
    Guid::parse_or_panic("b1b621d5-f19c-41a5-830b-d9152c69aae0");

/// Physical address where the kernel binary is loaded.
///
/// Must match the kernel linker script (`kernel/linker.ld`):
///   LOAD_ADDR = 0x40080000
///
/// The ARM64 Linux boot protocol places the image at RAM_BASE + TEXT_OFFSET.
/// For QEMU virt: RAM_BASE = 0x4000_0000, TEXT_OFFSET = 0x8_0000 (512 KiB).
const KERNEL_LOAD_PA: usize = 0x4008_0000;

/// Physical address where the BootInfo struct is written before jumping to the kernel.
///
/// Placed just below KERNEL_LOAD_PA so it is never overwritten by the kernel image.
/// The kernel reads it after the MMU is active via the TTBR1 high-VA alias.
const BOOT_INFO_PA: usize = 0x4007_F000;

/// Magic value embedded in the BootInfo struct so the kernel can detect a valid entry.
const BOOT_INFO_MAGIC: u32 = 0x00AE_6105; // "AEGLOS"

/// Pixel-format code for BGR (the most common GOP format on x86/ARM systems).
const FMT_BGR: u32 = 1;
/// Pixel-format code for RGB.
const FMT_RGB: u32 = 2;
/// Pixel-format code for hardware bitmask (rare).
const FMT_BITMASK: u32 = 3;

/// Information written by this bootloader and consumed by the kernel's simplefb driver.
///
/// Layout must exactly match `kernel/src/drivers/simplefb.rs::BootInfo`.
#[repr(C)]
struct BootInfo {
    magic:     u32,
    fb_base:   u64,
    fb_width:  u32,
    fb_height: u32,
    fb_stride: u32,
    fb_format: u32,
}

/// Maximum kernel size to read (32 MiB — kernel is currently ~4 MiB).
const KERNEL_MAX_BYTES: usize = 32 * 1024 * 1024;

/// Expected ARM64 Linux Image header magic at byte offset 0x38: "ARM\x64" LE.
const ARM64_MAGIC: u32 = 0x644d_5241;

// ── Entry point ──────────────────────────────────────────────────────────────

#[entry]
fn main(image: Handle, st: SystemTable<Boot>) -> Status {
    // Kernel is loaded into this raw buffer (allocated in step 1).
    // SAFETY: we will allocate these pages from UEFI before writing to them.
    let kernel_buf =
        unsafe { core::slice::from_raw_parts_mut(KERNEL_LOAD_PA as *mut u8, KERNEL_MAX_BYTES) };

    // ── Steps 1–4: all boot-services operations ───────────────────────────────
    //
    // Everything that touches `bs` lives in this block so the borrow on `st`
    // ends before we call `st.exit_boot_services()` (which consumes `st`).
    {
        let bs = st.boot_services();

        // ── 1. Allocate physical pages at the kernel load address ────────────
        //
        // AllocateType::Address allocates at an exact physical address.
        // On QEMU virt + OVMF this range is always free at kernel-load time.
        let pages_needed = (KERNEL_MAX_BYTES + 0xFFF) / 0x1000;
        bs.allocate_pages(
            AllocateType::Address(KERNEL_LOAD_PA as u64),
            MemoryType::LOADER_DATA,
            pages_needed,
        )
        .expect("allocate pages at 0x40080000");

        // ── 2. Open the boot filesystem ──────────────────────────────────────
        //
        // Use LoadedImageProtocol to find the device handle of the FAT partition
        // this bootloader was loaded from.  That partition also contains the
        // kernel binary at \boot\aeglos.bin.
        let device_handle = {
            let loaded_image = bs
                .open_protocol_exclusive::<LoadedImage>(image)
                .expect("open LoadedImage protocol");
            loaded_image.device().expect("boot device handle")
        };

        let mut sfs = bs
            .open_protocol_exclusive::<SimpleFileSystem>(device_handle)
            .expect("open SimpleFileSystem on boot device");

        let mut root = sfs.open_volume().expect("open root directory");

        // ── 3. Read the kernel binary ────────────────────────────────────────
        let kernel_file = root
            .open(
                cstr16!("\\boot\\aeglos.bin"),
                FileMode::Read,
                FileAttribute::empty(),
            )
            .expect("open \\boot\\aeglos.bin");

        let mut kernel_file = match kernel_file.into_type().expect("file type") {
            FileType::Regular(f) => f,
            _ => panic!("\\boot\\aeglos.bin is not a regular file"),
        };

        let bytes_read = kernel_file.read(kernel_buf).expect("read aeglos.bin");
        assert!(bytes_read >= 64, "aeglos.bin too small ({}B)", bytes_read);

        // ── 4. Validate ARM64 Linux Image header ─────────────────────────────
        //
        // The 64-byte header lives at the very start of the binary.
        // Magic "ARM\x64" (0x644D5241) sits at byte offset 0x38.
        let magic = u32::from_le_bytes([
            kernel_buf[0x38],
            kernel_buf[0x39],
            kernel_buf[0x3A],
            kernel_buf[0x3B],
        ]);
        assert_eq!(magic, ARM64_MAGIC, "Bad ARM64 image magic at offset 0x38");

    } // bs, sfs, root, kernel_file — all dropped here, releasing the borrow on `st`

    // ── 4b. Query GOP and write BootInfo ─────────────────────────────────────
    //
    // We do this in a separate block so any GOP protocol handle is dropped
    // before exit_boot_services().  The BootInfo is written to a physical
    // address that is safe to access from the kernel after MMU init.
    {
        let bs = st.boot_services();
        let boot_info_ptr = BOOT_INFO_PA as *mut BootInfo;

        // Locate the first GOP instance.  On most UEFI implementations there is
        // exactly one; on multi-GPU systems we take the first (primary display).
        let gop_handle = bs
            .get_handle_for_protocol::<GraphicsOutput>()
            .ok();

        if let Some(gop_h) = gop_handle {
            if let Ok(mut gop) = bs.open_protocol_exclusive::<GraphicsOutput>(gop_h) {
                let mode = gop.current_mode_info();
                let (w, h)   = mode.resolution();
                let stride_px = mode.stride();
                let format_code = match mode.pixel_format() {
                    PixelFormat::Bgr => FMT_BGR,
                    PixelFormat::Rgb => FMT_RGB,
                    _                => FMT_BITMASK,
                };
                let fb_base = gop.frame_buffer().as_mut_ptr() as u64;

                // Write the BootInfo struct to the reserved physical page.
                unsafe {
                    core::ptr::write_volatile(boot_info_ptr, BootInfo {
                        magic:     BOOT_INFO_MAGIC,
                        fb_base,
                        fb_width:  w as u32,
                        fb_height: h as u32,
                        fb_stride: stride_px as u32,
                        fb_format: format_code,
                    });
                }
            } else {
                // GOP not accessible — write a zeroed struct so the kernel skips it.
                unsafe { core::ptr::write_volatile(boot_info_ptr, BootInfo {
                    magic: 0, fb_base: 0, fb_width: 0, fb_height: 0, fb_stride: 0, fb_format: 0,
                }); }
            }
        } else {
            // No GOP — zero out the page so the kernel skips SimpleFB.
            unsafe { core::ptr::write_volatile(boot_info_ptr, BootInfo {
                magic: 0, fb_base: 0, fb_width: 0, fb_height: 0, fb_stride: 0, fb_format: 0,
            }); }
        }
    }

    // ── 5. Find the Device Tree Blob ─────────────────────────────────────────
    //
    // UEFI stores the FDT pointer in the configuration table.
    // If absent (unexpected), pass null — the kernel falls back to defaults.
    let dtb_ptr: *const u8 = st
        .config_table()
        .iter()
        .find(|e| e.guid == EFI_DTB_TABLE_GUID)
        .map(|e| e.address as *const u8)
        .unwrap_or(core::ptr::null());

    // ── 6. Exit UEFI boot services ───────────────────────────────────────────
    //
    // After this call, all UEFI protocols and services are unavailable.
    // The memory map is finalised and we must not call back into firmware.
    let _ = st.exit_boot_services(MemoryType::LOADER_DATA);

    // ── 7. Jump to kernel entry point ────────────────────────────────────────
    //
    // ARM64 Linux boot protocol:
    //   - Jump to address of first byte of image (= _start = the branch instr)
    //   - x0 = physical address of FDT/DTB (0 if none)
    //   - x1..x3 = 0
    //   - CPU in EL1 or EL2, MMU and caches may be on or off
    unsafe {
        let entry: extern "C" fn(*const u8) -> ! = core::mem::transmute(KERNEL_LOAD_PA);
        entry(dtb_ptr)
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // No UART access here (we may have exited boot services).
    // Park the core indefinitely.
    loop {
        unsafe { core::arch::asm!("wfe", options(nostack)) };
    }
}
