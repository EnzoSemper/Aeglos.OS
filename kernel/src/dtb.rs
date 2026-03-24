/// Flattened Device Tree (FDT/DTB) parser — full multi-node parsing.
///
/// QEMU passes the DTB address in x0 at kernel entry (AArch64 Linux boot
/// protocol).  We walk the structure block and extract all hardware information
/// needed to run on real AArch64 boards, not just hard-coded QEMU constants.
///
/// FDT wire format (all multi-byte values are big-endian):
///   Header @ 0:
///     [0..4]   magic      = 0xD00D_FEED
///     [4..8]   totalsize
///     [8..12]  off_dt_struct   (byte offset to structure block)
///     [12..16] off_dt_strings  (byte offset to strings block)
///     [16..20] off_dt_rsvmap   (byte offset to memory reserve map)
///   Structure block: stream of 4-byte tokens + data
///     FDT_BEGIN_NODE (1): null-terminated name, padded to 4 bytes
///     FDT_END_NODE   (2)
///     FDT_PROP       (3): u32 len, u32 nameoff, data[len] padded to 4 bytes
///     FDT_NOP        (4)
///     FDT_END        (9)

const FDT_MAGIC: u32      = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32   = 2;
const FDT_PROP: u32       = 3;
const FDT_NOP: u32        = 4;
const FDT_END: u32        = 9;

// ── Defaults matching QEMU virt + -m 4096M ────────────────────────────────────
const DEFAULT_RAM_BASE:      usize = 0x4000_0000;
const DEFAULT_RAM_SIZE:      usize = 0x1_0000_0000; // 4 GiB
const DEFAULT_GIC_DIST:      usize = 0x0800_0000;
const DEFAULT_GIC_CPU:       usize = 0x0801_0000;
const DEFAULT_UART_BASE:     usize = 0x0900_0000;

// ── Public output types ───────────────────────────────────────────────────────

/// Information extracted from the DTB on all supported nodes.
#[derive(Copy, Clone)]
pub struct DtbInfo {
    /// RAM base address and size from the /memory node.
    pub ram_base: usize,
    pub ram_size: usize,

    /// Number of CPU cores from /cpus.
    pub cpu_count: usize,

    /// Whether CPUs use PSCI or spin-table for secondary core bring-up.
    pub psci: bool,

    /// GIC distributor base and CPU interface base addresses.
    pub gic_dist:  usize,
    pub gic_cpu:   usize,

    /// Primary serial/UART base address (PL011 or 16550).
    pub uart_base: usize,

    /// Timer clock frequency in Hz (0 = read CNTFRQ_EL0 at runtime).
    pub timer_freq: u32,

    /// PCIe ECAM base address (0 = not found in DTB, use probed defaults).
    pub pcie_ecam: usize,

    /// Apple Interrupt Controller (AIC) physical base address.
    /// Present only on Apple Silicon boards; 0 on QEMU/GIC platforms.
    pub aic_base: usize,

    /// SimpleFB framebuffer from DTB /framebuffer or /simple-framebuffer node.
    pub fb_base:   usize,
    pub fb_width:  u32,
    pub fb_height: u32,
    pub fb_stride: u32,  // bytes per row

    /// Firmware-reserved regions (base, size) to exclude from page allocator.
    /// Up to 8 regions.
    pub reserved:       [(usize, usize); 8],
    pub reserved_count: usize,
}

impl DtbInfo {
    fn defaults() -> Self {
        DtbInfo {
            ram_base:       DEFAULT_RAM_BASE,
            ram_size:       DEFAULT_RAM_SIZE,
            cpu_count:      1,
            psci:           false,
            gic_dist:       DEFAULT_GIC_DIST,
            gic_cpu:        DEFAULT_GIC_CPU,
            uart_base:      DEFAULT_UART_BASE,
            timer_freq:     0,
            pcie_ecam:      0,
            aic_base:       0,
            fb_base:        0,
            fb_width:       0,
            fb_height:      0,
            fb_stride:      0,
            reserved:       [(0, 0); 8],
            reserved_count: 0,
        }
    }
}

// ── Legacy entry point (kept for callers that only need RAM) ──────────────────

/// Parse the DTB and return `(ram_base, ram_size)`.
///
/// Falls back to QEMU virt defaults on any failure.
pub fn parse_memory(dtb_ptr: *const u8) -> (usize, usize) {
    let info = parse(dtb_ptr);
    (info.ram_base, info.ram_size)
}

/// Parse the DTB and return a `DtbInfo` with all discovered hardware parameters.
///
/// Falls back to QEMU virt defaults for any node that is absent or malformed.
pub fn parse(dtb_ptr: *const u8) -> DtbInfo {
    if dtb_ptr.is_null() {
        return DtbInfo::defaults();
    }
    unsafe { walk(dtb_ptr) }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Read a big-endian u32 from an unaligned byte pointer.
#[inline(always)]
unsafe fn rbe32(p: *const u8) -> u32 {
    ((*p as u32) << 24)
        | ((*p.add(1) as u32) << 16)
        | ((*p.add(2) as u32) << 8)
        | (*p.add(3) as u32)
}

/// Read a big-endian u64 from two consecutive big-endian u32s.
#[inline(always)]
unsafe fn rbe64(p: *const u8) -> u64 {
    ((rbe32(p) as u64) << 32) | (rbe32(p.add(4)) as u64)
}

/// Compare a NUL-terminated string in the strings block to `want`.
#[inline(always)]
unsafe fn streq(strs: *const u8, nameoff: usize, want: &[u8]) -> bool {
    let p = strs.add(nameoff);
    for (i, &b) in want.iter().enumerate() {
        if *p.add(i) != b { return false; }
    }
    *p.add(want.len()) == 0
}

/// True if `slice` equals `prefix` or starts with `prefix` followed by `@`.
#[inline(always)]
fn name_matches(slice: &[u8], prefix: &[u8]) -> bool {
    slice.len() >= prefix.len()
        && &slice[..prefix.len()] == prefix
        && (slice.len() == prefix.len() || slice[prefix.len()] == b'@')
}

// ── Node context stack ────────────────────────────────────────────────────────

/// Which kind of DTB node is currently open at a given depth.
#[derive(Copy, Clone, PartialEq)]
enum Ctx {
    Other,
    Root,
    Memory,
    Cpus,
    Cpu,
    Intc,       // interrupt-controller (GIC)
    Uart,       // pl011 / 16550 serial
    ResvMem,    // /reserved-memory container
    ResvRegion, // individual reserved-memory child
    Psci,       // /psci node
    Chosen,
    Pcie,       // /pcie or /pci node (ECAM base)
    SimpleFb,   // /framebuffer or /simple-framebuffer node
    Aic,        // Apple Interrupt Controller node
}

const MAX_DEPTH: usize = 12;

// ── Main FDT walker ───────────────────────────────────────────────────────────

unsafe fn walk(blob: *const u8) -> DtbInfo {
    if rbe32(blob) != FDT_MAGIC {
        return DtbInfo::defaults();
    }

    let total_size  = rbe32(blob.add(4))  as usize;
    let off_struct  = rbe32(blob.add(8))  as usize;
    let off_strings = rbe32(blob.add(12)) as usize;
    let off_rsvmap  = rbe32(blob.add(16)) as usize;

    let sblk = blob.add(off_struct);
    let strs  = blob.add(off_strings);

    let mut info = DtbInfo::defaults();

    // ── Memory reserve map (pairs of u64: address, size; terminated by 0,0)
    {
        let mut rp = blob.add(off_rsvmap);
        loop {
            let addr = rbe64(rp);
            let size = rbe64(rp.add(8));
            rp = rp.add(16);
            if addr == 0 && size == 0 { break; }
            if info.reserved_count < 8 {
                info.reserved[info.reserved_count] = (addr as usize, size as usize);
                info.reserved_count += 1;
            }
        }
    }

    // ── Structure block walker ────────────────────────────────────────────────
    let mut off:  usize = 0;
    let mut depth: usize = 0;
    let mut ctx = [Ctx::Other; MAX_DEPTH];

    // Tracked per-level cell counts (needed to parse `reg` correctly).
    // Index = depth of the node that *declares* the cells value.
    let mut addr_cells = [2u32; MAX_DEPTH]; // root default: 2
    let mut size_cells = [2u32; MAX_DEPTH];

    loop {
        if off_struct + off + 4 > total_size { break; }

        let token = rbe32(sblk.add(off));
        off += 4;

        match token {
            FDT_BEGIN_NODE => {
                // Read NUL-terminated node name.
                let name_ptr = sblk.add(off);
                let mut nlen = 0usize;
                while nlen < 128 && *name_ptr.add(nlen) != 0 { nlen += 1; }
                let name = core::slice::from_raw_parts(name_ptr, nlen);
                off += (nlen + 1 + 3) & !3;

                let parent_ctx = if depth > 0 { ctx[depth - 1] } else { Ctx::Other };
                let new_ctx;

                if depth == 0 {
                    new_ctx = Ctx::Root;
                } else if name_matches(name, b"memory") {
                    new_ctx = Ctx::Memory;
                } else if name == b"cpus" {
                    new_ctx = Ctx::Cpus;
                } else if parent_ctx == Ctx::Cpus && name_matches(name, b"cpu") {
                    new_ctx = Ctx::Cpu;
                    info.cpu_count += 1; // count will be decremented once at end since we start at 1
                } else if name_matches(name, b"intc")
                    || name_matches(name, b"gic")
                    || name_matches(name, b"interrupt-controller")
                {
                    new_ctx = Ctx::Intc;
                } else if name_matches(name, b"pl011")
                    || name_matches(name, b"uart")
                    || name_matches(name, b"serial")
                {
                    new_ctx = Ctx::Uart;
                } else if name == b"reserved-memory" {
                    new_ctx = Ctx::ResvMem;
                } else if parent_ctx == Ctx::ResvMem {
                    new_ctx = Ctx::ResvRegion;
                } else if name == b"psci" {
                    new_ctx = Ctx::Psci;
                } else if name == b"chosen" {
                    new_ctx = Ctx::Chosen;
                } else if name_matches(name, b"pcie")
                    || name_matches(name, b"pci")
                {
                    new_ctx = Ctx::Pcie;
                } else if name == b"aic" {
                    // Apple Interrupt Controller — `aic` is the canonical node name
                    // in the Device Tree passed by m1n1/U-Boot on Apple Silicon.
                    new_ctx = Ctx::Aic;
                } else if name_matches(name, b"framebuffer")
                    || name_matches(name, b"simple-framebuffer")
                    || name_matches(name, b"display")
                {
                    new_ctx = Ctx::SimpleFb;
                } else {
                    new_ctx = Ctx::Other;
                }

                if depth < MAX_DEPTH {
                    ctx[depth] = new_ctx;
                    // Inherit cell counts from parent (will be overwritten by props)
                    if depth > 0 {
                        addr_cells[depth] = addr_cells[depth - 1];
                        size_cells[depth] = size_cells[depth - 1];
                    }
                    depth += 1;
                }
            }

            FDT_END_NODE => {
                if depth > 0 { depth -= 1; }
            }

            FDT_PROP => {
                let prop_len  = rbe32(sblk.add(off))     as usize;
                let name_off  = rbe32(sblk.add(off + 4)) as usize;
                let data      = sblk.add(off + 8);
                off += 8 + ((prop_len + 3) & !3);

                let cur = if depth > 0 { ctx[depth - 1] } else { Ctx::Other };
                let d   = if depth > 0 { depth - 1 } else { 0 };

                // Helper: extract u64 address from a reg field using the
                // *parent* node's cell counts (d is the current node's depth).
                let parent_ac = if d > 0 { addr_cells[d - 1] } else { 2u32 };
                let parent_sc = if d > 0 { size_cells[d - 1] } else { 2u32 };

                // ── #address-cells / #size-cells ─────────────────────────────
                if streq(strs, name_off, b"#address-cells") && prop_len == 4 {
                    addr_cells[d] = rbe32(data);
                    continue;
                }
                if streq(strs, name_off, b"#size-cells") && prop_len == 4 {
                    size_cells[d] = rbe32(data);
                    continue;
                }

                match cur {
                    // ── /memory ──────────────────────────────────────────────
                    Ctx::Memory => {
                        if streq(strs, name_off, b"reg") && prop_len >= 16 {
                            // Assume 64-bit (2+2) reg entries from root.
                            let base = rbe64(data)       as usize;
                            let size = rbe64(data.add(8)) as usize;
                            if size > 0 {
                                info.ram_base = base;
                                info.ram_size = size;
                            }
                        }
                    }

                    // ── /cpus — container ─────────────────────────────────────
                    Ctx::Cpus => {
                        // Nothing needed from the cpus container node itself.
                    }

                    // ── /cpus/cpu@N — individual core ─────────────────────────
                    Ctx::Cpu => {
                        if streq(strs, name_off, b"clock-frequency") && prop_len == 4 {
                            // Some boards put timer freq here; prefer the dedicated timer prop.
                            if info.timer_freq == 0 {
                                info.timer_freq = rbe32(data);
                            }
                        }
                        if streq(strs, name_off, b"enable-method") {
                            // Value is a NUL-terminated string like "psci" or "spin-table"
                            if prop_len >= 4 {
                                let s = core::slice::from_raw_parts(data, prop_len.min(16));
                                if &s[..4.min(s.len())] == b"psci" {
                                    info.psci = true;
                                }
                            }
                        }
                    }

                    // ── /intc — GIC ────────────────────────────────────────────
                    Ctx::Intc => {
                        if streq(strs, name_off, b"reg") {
                            // reg layout depends on parent's address/size cells.
                            // Most boards: 2+2 (64-bit): [dist_addr, dist_size, cpu_addr, cpu_size]
                            // Each addr or size is parent_ac * 4 bytes; each pair is (parent_ac + parent_sc) * 4 bytes.
                            let entry_bytes = ((parent_ac + parent_sc) * 4) as usize;
                            if prop_len >= entry_bytes * 2 {
                                // First entry: distributor
                                let dist = if parent_ac == 2 {
                                    rbe64(data) as usize
                                } else {
                                    rbe32(data) as usize
                                };
                                // Second entry: CPU interface (skip first entry)
                                let cpu_off = entry_bytes;
                                let cpu = if parent_ac == 2 {
                                    rbe64(data.add(cpu_off)) as usize
                                } else {
                                    rbe32(data.add(cpu_off)) as usize
                                };
                                if dist > 0 { info.gic_dist = dist; }
                                if cpu  > 0 { info.gic_cpu  = cpu;  }
                            }
                        }
                    }

                    // ── pl011 / serial ─────────────────────────────────────────
                    Ctx::Uart => {
                        if streq(strs, name_off, b"reg") && prop_len >= 8 {
                            let base = if parent_ac == 2 && prop_len >= 16 {
                                rbe64(data) as usize
                            } else {
                                rbe32(data) as usize
                            };
                            if base > 0 && info.uart_base == DEFAULT_UART_BASE {
                                // Only update if we haven't found one yet.
                                info.uart_base = base;
                            }
                        }
                    }

                    // ── /reserved-memory child region ──────────────────────────
                    Ctx::ResvRegion => {
                        if streq(strs, name_off, b"reg") {
                            let entry_bytes = ((parent_ac + parent_sc) * 4) as usize;
                            if prop_len >= entry_bytes && info.reserved_count < 8 {
                                let base = if parent_ac == 2 {
                                    rbe64(data) as usize
                                } else {
                                    rbe32(data) as usize
                                };
                                let size_off = (parent_ac * 4) as usize;
                                let size = if parent_sc == 2 {
                                    rbe64(data.add(size_off)) as usize
                                } else {
                                    rbe32(data.add(size_off)) as usize
                                };
                                if size > 0 {
                                    info.reserved[info.reserved_count] = (base, size);
                                    info.reserved_count += 1;
                                }
                            }
                        }
                    }

                    // ── /pcie — PCIe ECAM ──────────────────────────────────────
                    Ctx::Pcie => {
                        if streq(strs, name_off, b"reg") && prop_len >= 8 {
                            // PCIe reg is typically child-addr-cells(3)+parent-addr-cells(2)
                            // The ECAM base is in the first reg entry as a 64-bit value.
                            // Common layout: [phys_hi, phys_mid, phys_lo, parent_hi, parent_lo, size_hi, size_lo]
                            // Simplification: try reading as u64 at offset 0 or 4 depending on cell count.
                            let ecam = if prop_len >= 16 {
                                rbe64(data) as usize
                            } else {
                                rbe32(data) as usize
                            };
                            if ecam > 0 && info.pcie_ecam == 0 {
                                info.pcie_ecam = ecam;
                            }
                        }
                    }

                    // ── Apple AIC ──────────────────────────────────────────────
                    Ctx::Aic => {
                        if streq(strs, name_off, b"reg") && prop_len >= 4 {
                            // AIC reg is a single (base, size) pair.
                            // The Apple DTB passed by m1n1/U-Boot uses 64-bit cells.
                            let base = if prop_len >= 16 {
                                rbe64(data) as usize  // 64-bit base address
                            } else {
                                rbe32(data) as usize  // 32-bit fallback
                            };
                            if base > 0 && info.aic_base == 0 {
                                info.aic_base = base;
                            }
                        }
                    }

                    // ── /simple-framebuffer ────────────────────────────────────
                    Ctx::SimpleFb => {
                        if streq(strs, name_off, b"reg") && prop_len >= 8 {
                            let base = if parent_ac == 2 && prop_len >= 16 {
                                rbe64(data) as usize
                            } else {
                                rbe32(data) as usize
                            };
                            if base > 0 { info.fb_base = base; }
                        }
                        if streq(strs, name_off, b"width") && prop_len == 4 {
                            info.fb_width = rbe32(data);
                        }
                        if streq(strs, name_off, b"height") && prop_len == 4 {
                            info.fb_height = rbe32(data);
                        }
                        if streq(strs, name_off, b"stride") && prop_len == 4 {
                            info.fb_stride = rbe32(data);
                        }
                    }

                    _ => {}
                }
            }

            FDT_NOP => {}
            FDT_END => break,
            _       => break, // corrupt token
        }
    }

    // cpu_count starts at 1 (default) and we incremented for each Cpu BEGIN.
    // Reset to actual count: if we saw any cpu@ nodes use that, else keep 1.
    // The walk above increments once per Cpu node. The default of 1 was the
    // pre-walk default, so we need to correct: if cpu nodes were found the
    // counter above over-counts by 1 (due to the default).  We track it
    // separately here.
    // Actually: info.cpu_count starts at 1 (default). Each Cpu BEGIN_NODE
    // increments it → after walk, info.cpu_count = 1 + actual_count.
    // Subtract the initial 1 if we found at least one Cpu node:
    if info.cpu_count > 1 {
        info.cpu_count -= 1;
    }

    info
}
