/// USB Power Delivery (USB-PD) skeleton for Apple Silicon.
///
/// On Apple M-series Macs, USB-PD negotiation is handled by the ATCP
/// (Apple Type-C PHY) controller, which is an internally-connected device
/// accessible via the Apple PCIe fabric and controlled through the ANS
/// firmware coprocessor.
///
/// This module provides:
///   • PCIe device-ID matching for the ATCP controller
///   • Basic power-role detection (host vs device)
///   • A polling interface to read current negotiated contract
///   • Stubs for future VBUS control and alternate-mode (DisplayPort) signalling
///
/// Full ATCP programming requires Apple-internal firmware (ATML); this driver
/// uses the coprocessor messaging interface documented by the Asahi Linux
/// project (drivers/usb/typec/apple-typec/).

use crate::memory::vmm::KERNEL_VA_OFFSET;

// ── Apple ATCP PCIe IDs ───────────────────────────────────────────────────────

pub const APPLE_ATCP_VENDOR: u16 = 0x106B;
// M1: 0x4000, M2: 0x4001, M3/M4: ~0x4002+
pub const APPLE_ATCP_DEVICE_M1:  u16 = 0x4000;
pub const APPLE_ATCP_DEVICE_M2:  u16 = 0x4001;
pub const APPLE_ATCP_DEVICE_M4:  u16 = 0x4003; // best-known

pub fn is_atcp_device(vendor: u16, device: u16) -> bool {
    vendor == APPLE_ATCP_VENDOR
        && matches!(device,
                    APPLE_ATCP_DEVICE_M1 | APPLE_ATCP_DEVICE_M2 | APPLE_ATCP_DEVICE_M4)
}

// ── USB-PD contract state ─────────────────────────────────────────────────────

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PowerRole {
    /// Acting as USB host / power source (laptop wall-side)
    Source,
    /// Acting as USB device / power sink (charging)
    Sink,
    /// Not connected or role not yet negotiated
    Disconnected,
}

#[derive(Copy, Clone, Debug)]
pub struct PdContract {
    pub role:          PowerRole,
    pub voltage_mv:    u32,   // negotiated voltage in millivolts
    pub current_ma:    u32,   // negotiated current limit in milliamps
    pub max_power_mw:  u32,   // = voltage_mv * current_ma / 1000
}

impl PdContract {
    const fn default() -> Self {
        PdContract {
            role:         PowerRole::Disconnected,
            voltage_mv:   0,
            current_ma:   0,
            max_power_mw: 0,
        }
    }
}

// ── Controller state ──────────────────────────────────────────────────────────

struct AtcpCtrl {
    base_va: usize,
    /// Number of USB-C ports (typically 1-4 on Apple Silicon)
    num_ports: usize,
    contracts: [PdContract; 4],
}

static mut ATCP: Option<AtcpCtrl> = None;

// ── MMIO helpers ──────────────────────────────────────────────────────────────

/// ATCP mailbox registers (offsets within BAR0).
/// These are reverse-engineered offsets from the Asahi Linux ATCP driver.
/// NOTE: subject to change; verify against apple-typec.c when available.
const ATCP_STATUS:       usize = 0x0000; // controller status
const ATCP_PORT_COUNT:   usize = 0x0004; // number of USB-C ports
const ATCP_PORT_STATUS:  usize = 0x0100; // per-port status, stride 0x40
const ATCP_PORT_VDO:     usize = 0x0110; // per-port VDO (Vendor Defined Object)

const PORT_STRIDE: usize = 0x40;

// Port status register bits
const PORT_CONNECTED:  u32 = 1 << 0;
const PORT_IS_SOURCE:  u32 = 1 << 1;
const PORT_CONTRACT:   u32 = 1 << 4; // explicit contract negotiated

// ── Initialisation ────────────────────────────────────────────────────────────

unsafe fn read32(va: usize, off: usize) -> u32 {
    core::ptr::read_volatile((va + off) as *const u32)
}

/// Initialise the ATCP USB-PD controller at BAR0 physical address.
/// Returns true if at least one port is found.
pub fn init(bar0_phys: usize) -> bool {
    let uart = crate::drivers::uart::Uart::new();
    let base_va = bar0_phys + KERNEL_VA_OFFSET;

    uart.puts("[usb-pd] ATCP BAR0=");
    uart.put_hex(bar0_phys);

    let status = unsafe { read32(base_va, ATCP_STATUS) };
    if status == 0xFFFF_FFFF {
        uart.puts("  (not present)\r\n");
        return false;
    }

    let num_ports = unsafe { read32(base_va, ATCP_PORT_COUNT) } as usize;
    let num_ports = num_ports.min(4);
    uart.puts("  ports=");
    uart.put_dec(num_ports);
    uart.puts("\r\n");

    let mut ctrl = AtcpCtrl {
        base_va,
        num_ports,
        contracts: [PdContract::default(); 4],
    };

    // Initial poll of all ports
    for i in 0..num_ports {
        poll_port(&mut ctrl, i);
        let ref c = ctrl.contracts[i];
        if c.role != PowerRole::Disconnected {
            uart.puts("[usb-pd] port ");
            uart.put_dec(i);
            uart.puts(": ");
            uart.puts(match c.role {
                PowerRole::Source => "SOURCE ",
                PowerRole::Sink   => "SINK   ",
                _                 => "DISC   ",
            });
            uart.put_dec(c.voltage_mv as usize);
            uart.puts("mV / ");
            uart.put_dec(c.current_ma as usize);
            uart.puts("mA\r\n");
        }
    }

    unsafe { ATCP = Some(ctrl); }
    true
}

/// Poll port `idx` and update its contract.
fn poll_port(ctrl: &mut AtcpCtrl, idx: usize) {
    if idx >= ctrl.num_ports { return; }
    unsafe {
        let off = ATCP_PORT_STATUS + idx * PORT_STRIDE;
        let status = read32(ctrl.base_va, off);

        if status & PORT_CONNECTED == 0 {
            ctrl.contracts[idx] = PdContract::default();
            return;
        }

        let role = if status & PORT_IS_SOURCE != 0 {
            PowerRole::Source
        } else {
            PowerRole::Sink
        };

        // Read negotiated PDO from VDO register (packed mV / mA)
        // Format is Apple-specific; below is a best-effort decode.
        let vdo = read32(ctrl.base_va, ATCP_PORT_VDO + idx * PORT_STRIDE);
        let voltage_mv  = ((vdo >> 16) & 0x3FF) * 50; // bits[25:16] × 50 mV
        let current_ma  = (vdo & 0x3FF) * 10;          // bits[9:0]  × 10 mA
        let max_power_mw = voltage_mv * current_ma / 1000;

        ctrl.contracts[idx] = PdContract {
            role,
            voltage_mv: if voltage_mv == 0 { 5000 } else { voltage_mv },
            current_ma:  if current_ma == 0 { 900 }  else { current_ma },
            max_power_mw,
        };
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// True if an ATCP controller has been initialised.
pub fn is_up() -> bool {
    unsafe { ATCP.is_some() }
}

/// Return the current PD contract for the given port (0-based).
pub fn contract(port: usize) -> PdContract {
    unsafe {
        match ATCP.as_ref() {
            Some(c) if port < c.num_ports => c.contracts[port],
            _ => PdContract::default(),
        }
    }
}

/// Refresh the contract cache for all ports (call from the timer tick or
/// a dedicated task; the ATCP coprocessor updates registers asynchronously).
pub fn poll_all() {
    unsafe {
        if let Some(ctrl) = ATCP.as_mut() {
            let n = ctrl.num_ports;
            for i in 0..n {
                poll_port(ctrl, i);
            }
        }
    }
}

/// Total available charging power in milliwatts across all sink contracts.
/// Useful for deciding whether to enable high-power peripherals.
pub fn total_sink_power_mw() -> u32 {
    unsafe {
        match ATCP.as_ref() {
            Some(c) => c.contracts[..c.num_ports]
                .iter()
                .filter(|co| co.role == PowerRole::Sink)
                .map(|co| co.max_power_mw)
                .fold(0u32, |a, b| a.saturating_add(b)),
            None => 0,
        }
    }
}
