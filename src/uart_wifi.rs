//! K210 <-> onboard ESP32 "WiFi modem" link over UART1.
//!
//! The ESP32 runs the firmware in `esp32-modem/` (UART command protocol) instead of
//! nina-fw(SPI). This path uses IO6/IO7 (ESP32 U0TXD/U0RXD), which are independent of
//! the camera's DVP/SPI0 pads -- so a camera capture no longer wedges WiFi.
//!
//! Wire framing both ways: `<tag:1><len:2 LE><payload>`.
//!   K210->ESP32: P ping, C connect(ssid\0pass), L listen(port[2]), A accept,
//!                R recv, S send(bytes), X close
//!   ESP32->K210: O ok, E err, I ip[4], A connected(1), R bytes, S sent[2]
//!
//! UART1 is a Synopsys DW_apb_uart (16550-style); k210-hal's APB-uart Tx/Rx are buggy
//! (TX waits on inverted THRE, RX reads len bytes after one ready-check), so this is
//! PAC-direct. apb0 = 195 MHz.

#![allow(dead_code)]

use k210_hal::pac;

const APB0: u32 = 195_000_000;
const CLINT_MTIME: *const u64 = 0x0200_BFF8 as *const u64;
const MTIME_HZ: u64 = 7_800_000;

// EN (ESP32 chip-enable) = IO8 = GPIOHS channel 0.
const EN_CH: u32 = 0;

pub const CMD_PING: u8 = b'P';
pub const CMD_CONNECT: u8 = b'C';
pub const CMD_LISTEN: u8 = b'L';
pub const CMD_ACCEPT: u8 = b'A';
pub const CMD_RECV: u8 = b'R';
pub const CMD_SEND: u8 = b'S';
pub const CMD_SEND565: u8 = b'B'; // send RGB565 bytes; ESP32 expands to BGR24 (33% less UART)
pub const CMD_CLOSE: u8 = b'X';

fn uart1() -> &'static pac::uart1::RegisterBlock {
    unsafe { &*pac::UART1::ptr() }
}
fn gpiohs() -> &'static pac::gpiohs::RegisterBlock {
    unsafe { &*pac::GPIOHS::ptr() }
}
fn mtime() -> u64 {
    unsafe { core::ptr::read_volatile(CLINT_MTIME) }
}
pub fn sleep_ms(ms: u64) {
    let end = mtime() + ms * (MTIME_HZ / 1000);
    while mtime() < end {}
}

fn set_en(high: bool) {
    let g = gpiohs();
    unsafe {
        g.output_en.modify(|r, w| w.bits(r.bits() | (1 << EN_CH)));
        if high {
            g.output_val.modify(|r, w| w.bits(r.bits() | (1 << EN_CH)));
        } else {
            g.output_val.modify(|r, w| w.bits(r.bits() & !(1 << EN_CH)));
        }
    }
}

/// Power-cycle the ESP32 via EN so it (re)boots the modem firmware. GPIO0 (boot
/// strap) is on the board's CH552 and floats high when no USB host drives the ESP32
/// port, so this is a normal (flash) boot.
pub fn reset_esp() {
    set_en(false);
    sleep_ms(40);
    set_en(true);
    sleep_ms(50); // just let EN settle; the caller scans for the ready marker
}

/// Configure UART1 (caller has muxed IO6->UART1_RX, IO7->UART1_TX).
pub fn init(baud: u32) {
    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc).clk_en_peri.modify(|_, w| w.uart1_clk_en().set_bit());
        (*sc).peri_reset.modify(|_, w| w.uart1_reset().set_bit());
        for _ in 0..2000 {
            core::arch::asm!("nop")
        }
        (*sc).peri_reset.modify(|_, w| w.uart1_reset().clear_bit());
        // Enable the IO6 (UART1_RX) input buffer + Schmitt trigger. into_function
        // doesn't always turn the pad input on (we hit this with GPIOHS READY too).
        (*pac::FPIOA::ptr()).io[6].modify(|_, w| w.ie_en().set_bit().st().set_bit());
    }
    let u = uart1();
    let divisor = APB0 / baud;
    let dlh = ((divisor >> 12) & 0xff) as u8;
    let dll = ((divisor >> 4) & 0xff) as u8;
    let dlf = (divisor & 0xf) as u8;
    unsafe {
        u.dlh_ier.write(|w| w.bits(0)); // IER = 0: polling, no interrupts
        u.lcr.write(|w| w.bits(1 << 7)); // DLAB=1 to access divisor latches
        u.dlh_ier.write(|w| w.bits(dlh as u32)); // DLH
        u.rbr_dll_thr.write(|w| w.bits(dll as u32)); // DLL
        u.dlf.write(|w| w.bits(dlf as u32)); // fractional divisor
        u.lcr.write(|w| w.bits(0x03)); // 8 data bits, 1 stop, no parity, DLAB=0
        u.fcr_iir.write(|w| w.bits(0x07)); // FIFO enable + reset RX FIFO + reset TX FIFO
        u.fcr_iir.write(|w| w.bits(0x01)); // FIFO enable
        let _ = u.lsr.read().bits(); // clear sticky error state
    }
    while (u.lsr.read().bits() & 1) != 0 {
        let _ = u.rbr_dll_thr.read().bits(); // drain stale RX
    }
}

/// Scan the RX stream for the modem's ready marker (`AA 55 'M' 'D' 'M' '1'`, emitted
/// once at boot). This is the only trustworthy "modem is running the app" signal.
pub fn wait_marker(timeout_ms: u64) -> bool {
    const PAT: [u8; 6] = [0xAA, 0x55, b'M', b'D', b'M', b'1'];
    let u = uart1();
    let mut m = 0usize;
    let end = mtime() + timeout_ms * (MTIME_HZ / 1000);
    while mtime() < end {
        if (u.lsr.read().bits() & 1) != 0 {
            let b = (u.rbr_dll_thr.read().bits() & 0xff) as u8;
            if b == PAT[m] {
                m += 1;
                if m == PAT.len() {
                    return true;
                }
            } else {
                m = if b == PAT[0] { 1 } else { 0 };
            }
        }
    }
    false
}

/// Power-cycle the ESP32 and wait for the ready marker, retrying the EN pulse. If the
/// marker never appears the ESP32 came up in download mode (GPIO0 low) instead of the
/// app -- another EN pulse usually fixes it.
pub fn bringup() -> bool {
    for _ in 0..6 {
        reset_esp();
        if wait_marker(2500) {
            drain(20); // consume the marker's trailing '\n' so the first cmd is aligned
            return true;
        }
    }
    false
}

fn tx_byte(b: u8) {
    let u = uart1();
    while (u.lsr.read().bits() & (1 << 5)) == 0 {} // wait THRE (room in TX)
    unsafe { u.rbr_dll_thr.write(|w| w.bits(b as u32)) };
}

pub fn write(data: &[u8]) {
    for &b in data {
        tx_byte(b);
    }
}

fn rx_byte(timeout_ms: u64) -> Option<u8> {
    let u = uart1();
    let end = mtime() + timeout_ms * (MTIME_HZ / 1000);
    while (u.lsr.read().bits() & 1) == 0 {
        if mtime() > end {
            return None;
        }
    }
    Some((u.rbr_dll_thr.read().bits() & 0xff) as u8)
}

pub fn read_exact(buf: &mut [u8], timeout_ms: u64) -> bool {
    for slot in buf.iter_mut() {
        match rx_byte(timeout_ms) {
            Some(b) => *slot = b,
            None => return false,
        }
    }
    true
}

/// Drop any pending RX bytes (e.g. ROM boot noise) within `ms`.
pub fn drain(ms: u64) {
    let end = mtime() + ms * (MTIME_HZ / 1000);
    let u = uart1();
    while mtime() < end {
        if (u.lsr.read().bits() & 1) != 0 {
            let _ = u.rbr_dll_thr.read().bits();
        }
    }
}

/// Send a command frame and read one reply frame. Returns (reply_tag, payload_len)
/// with the payload copied into `reply` (truncated to its capacity; extra drained).
pub fn cmd(
    tag: u8,
    payload: &[u8],
    reply: &mut [u8],
    timeout_ms: u64,
) -> Option<(u8, usize)> {
    let hdr = [tag, (payload.len() & 0xff) as u8, ((payload.len() >> 8) & 0xff) as u8];
    write(&hdr);
    if !payload.is_empty() {
        write(payload);
    }
    // Resync to the reply's AA 55 prefix, skipping any line noise (e.g. from the
    // ESP32 restarting its UART around the WiFi connect).
    let mut b = [0u8; 1];
    let mut state = 0u8;
    loop {
        if !read_exact(&mut b, timeout_ms) {
            return None;
        }
        if state == 0 {
            if b[0] == 0xAA {
                state = 1;
            }
        } else if b[0] == 0x55 {
            break;
        } else {
            state = if b[0] == 0xAA { 1 } else { 0 };
        }
    }
    let mut rh = [0u8; 3];
    if !read_exact(&mut rh, timeout_ms) {
        return None;
    }
    let rlen = (rh[1] as usize) | ((rh[2] as usize) << 8);
    let n = rlen.min(reply.len());
    if n > 0 && !read_exact(&mut reply[..n], timeout_ms) {
        return None;
    }
    let mut extra = rlen - n;
    let mut junk = [0u8; 1];
    while extra > 0 {
        if !read_exact(&mut junk, timeout_ms) {
            return None;
        }
        extra -= 1;
    }
    Some((rh[0], n))
}
