/// PL011 UART driver for QEMU virt machine.
///
/// The QEMU virt machine maps a PL011 UART at 0x0900_0000.
/// This is a minimal polled-mode driver — no interrupts yet.

const UART_BASE: usize = 0x0900_0000 + crate::memory::vmm::KERNEL_VA_OFFSET;

/// UART Data Register offset
const UARTDR: usize = 0x00;
/// UART Flag Register offset
const UARTFR: usize = 0x18;
/// Transmit FIFO full flag
const UARTFR_TXFF: u32 = 1 << 5;

pub struct Uart {
    base: usize,
}

impl Uart {
    /// Create a new UART instance at the QEMU virt PL011 address.
    pub const fn new() -> Self {
        Self { base: UART_BASE }
    }

    /// Write a single byte, waiting if the transmit FIFO is full.
    pub fn putc(&self, byte: u8) {
        unsafe {
            let fr = self.base + UARTFR;
            // Spin while TX FIFO is full
            while (core::ptr::read_volatile(fr as *const u32) & UARTFR_TXFF) != 0 {
                core::hint::spin_loop();
            }
            core::ptr::write_volatile((self.base + UARTDR) as *mut u32, byte as u32);
        }
    }

    /// Write a string to UART.
    pub fn puts(&self, s: &str) {
        for byte in s.bytes() {
            self.putc(byte);
        }
    }

    /// Print a usize as decimal.
    pub fn put_dec(&self, mut val: usize) {
        if val == 0 {
            self.putc(b'0');
            return;
        }
        let mut buf = [0u8; 20];
        let mut i = 0;
        while val > 0 {
            buf[i] = b'0' + (val % 10) as u8;
            val /= 10;
            i += 1;
        }
        while i > 0 {
            i -= 1;
            self.putc(buf[i]);
        }
    }

    /// Print a usize as hexadecimal with 0x prefix.
    pub fn put_hex(&self, val: usize) {
        self.puts("0x");
        if val == 0 {
            self.putc(b'0');
            return;
        }
        let mut started = false;
        for shift in (0..16).rev() {
            let nibble = ((val >> (shift * 4)) & 0xF) as u8;
            if nibble != 0 || started {
                started = true;
                if nibble < 10 {
                    self.putc(b'0' + nibble);
                } else {
                    self.putc(b'a' + nibble - 10);
                }
            }
        }
    }

    /// Read a single byte (blocking). Returns the byte read.
    pub fn getc(&self) -> u8 {
        loop {
            if let Some(c) = self.try_getc() {
                return c;
            }
            core::hint::spin_loop();
        }
    }

    /// Try to read a single byte (non-blocking).
    pub fn try_getc(&self) -> Option<u8> {
        unsafe {
             let fr = self.base + UARTFR;
             if (core::ptr::read_volatile(fr as *const u32) & (1 << 4)) == 0 {
                 Some(core::ptr::read_volatile((self.base + UARTDR) as *const u32) as u8)
             } else {
                 None
             }
        }
    }
}
