/// Platform / CPU detection for runtime hardware selection.
///
/// Reads MIDR_EL1 to distinguish Apple Silicon from generic AArch64 (QEMU).
///
/// MIDR_EL1 layout:
///   bits[31:24] = Implementer  (0x41 = ARM, 0x61 = Apple)
///   bits[23:20] = Variant
///   bits[19:16] = Architecture (0xF = ID registers)
///   bits[15:4]  = PartNum
///   bits[3:0]   = Revision

/// CPU implementer codes.
pub const IMPL_ARM:   u32 = 0x41;
pub const IMPL_APPLE: u32 = 0x61;

/// Apple Silicon PartNums (bits[15:4] of MIDR_EL1).
/// M1:  0x022 (Icestorm/Firestorm)
/// M2:  0x025 (Blizzard/Avalanche)
/// M3:  0x035
/// M4:  0x045 (unconfirmed — may be 0x046 or nearby)
pub const PART_APPLE_M1: u32 = 0x022;
pub const PART_APPLE_M2: u32 = 0x025;
pub const PART_APPLE_M3: u32 = 0x035;
pub const PART_APPLE_M4: u32 = 0x045; // best-known at time of writing

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Platform {
    /// Generic AArch64 — QEMU virt, Graviton, RPi, etc.
    Generic,
    /// Apple Silicon M1 / M1 Pro / M1 Max / M1 Ultra
    AppleM1,
    /// Apple Silicon M2 family
    AppleM2,
    /// Apple Silicon M3 family
    AppleM3,
    /// Apple Silicon M4 family
    AppleM4,
    /// Apple Silicon — generation unrecognised but confirmed Apple implementer
    AppleUnknown,
}

/// Read MIDR_EL1 and classify the current platform.
pub fn detect() -> Platform {
    let midr: u64;
    unsafe {
        core::arch::asm!("mrs {}, midr_el1", out(reg) midr);
    }
    let implementer = ((midr >> 24) & 0xFF) as u32;
    let part        = ((midr >> 4)  & 0xFFF) as u32;

    if implementer != IMPL_APPLE {
        return Platform::Generic;
    }

    // Apple CPU — identify generation by part number.
    // Each generation has Efficiency and Performance cores with adjacent part#s.
    match part {
        p if p & 0xFF0 == PART_APPLE_M1 & 0xFF0 => Platform::AppleM1,
        p if p & 0xFF0 == PART_APPLE_M2 & 0xFF0 => Platform::AppleM2,
        p if p & 0xFF0 == PART_APPLE_M3 & 0xFF0 => Platform::AppleM3,
        p if p & 0xFF0 == PART_APPLE_M4 & 0xFF0 => Platform::AppleM4,
        _ => Platform::AppleUnknown,
    }
}

impl Platform {
    /// True for any Apple Silicon variant.
    pub fn is_apple(self) -> bool {
        !matches!(self, Platform::Generic)
    }

    /// Human-readable name for boot log.
    pub fn name(self) -> &'static str {
        match self {
            Platform::Generic      => "Generic AArch64 (QEMU/Graviton)",
            Platform::AppleM1      => "Apple M1",
            Platform::AppleM2      => "Apple M2",
            Platform::AppleM3      => "Apple M3",
            Platform::AppleM4      => "Apple M4",
            Platform::AppleUnknown => "Apple Silicon (unknown gen)",
        }
    }
}
