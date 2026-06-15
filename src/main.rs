//! Hardware timer demo on the K210: use the RISC-V CLINT machine timer `mtime`
//! for real timing instead of a nop loop.
//!
//! Twist: we don't know mtime's tick rate up front (it depends on the boot
//! clock), so we *self-calibrate* it against the UART: sending N bytes at a
//! known 115200 baud takes a known wall-clock time, so the mtime delta over that
//! send gives mtime_hz. Then we blink IO6 at a precise 1 Hz off mtime and print
//! the elapsed seconds -- all verifiable against the host clock on the serial.
//!
//! (IO6's red LED is the one that actually lights on this board; the documented
//! RGB LED on IO13 stays dark -- see docs/finding-the-led.md.)

#![no_std]
#![no_main]

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32; // bit31 = FIFO full
const MTIME_LO: *const u32 = 0x0200_BFF8 as *const u32; // CLINT mtime (64-bit)
const MTIME_HI: *const u32 = 0x0200_BFFC as *const u32;

fn putc(c: u8) {
    unsafe {
        while core::ptr::read_volatile(UARTHS_TXDATA) & 0x8000_0000 != 0 {}
        core::ptr::write_volatile(UARTHS_TXDATA, c as u32);
    }
}
fn puts(s: &[u8]) {
    for &c in s {
        putc(c);
    }
}
fn put_u64(mut n: u64) {
    if n == 0 {
        putc(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    puts(&buf[i..]);
}
fn mtime() -> u64 {
    unsafe {
        loop {
            let hi = core::ptr::read_volatile(MTIME_HI);
            let lo = core::ptr::read_volatile(MTIME_LO);
            if core::ptr::read_volatile(MTIME_HI) == hi {
                return ((hi as u64) << 32) | lo as u64;
            }
        }
    }
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _led = fpioa.io6.into_function(fpioa::GPIOHS0);

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(115_200.bps(), &clocks); // sets the baud divisor

    let gpiohs = p.GPIOHS;
    gpiohs.output_en.modify(|r, w| unsafe { w.bits(r.bits() | 1) });
    gpiohs.input_en.modify(|r, w| unsafe { w.bits(r.bits() & !1) });

    puts(b"\r\n-- K210 CLINT mtime timer demo --\r\n");

    // Calibrate mtime_hz: sending N bytes at 115200 (10 bits/byte) takes
    // N/11520 seconds; mtime_hz = delta_ticks * 11520 / N.
    let n: u64 = 2000;
    let t0 = mtime();
    for _ in 0..n {
        putc(b'.');
    }
    let dt = mtime() - t0;
    let mtime_hz = dt * 11520 / n;
    puts(b"\r\nmtime_hz=");
    put_u64(mtime_hz);
    puts(b"\r\n");

    // Precise 1 Hz blink off mtime; print elapsed whole seconds.
    let half = mtime_hz / 2;
    let start = mtime();
    let mut next = start + half;
    let mut on = true;
    let mut tick: u64 = 0;
    loop {
        while mtime() < next {}
        next += half;
        if on {
            gpiohs.output_val.modify(|r, w| unsafe { w.bits(r.bits() & !1) });
        } else {
            gpiohs.output_val.modify(|r, w| unsafe { w.bits(r.bits() | 1) });
        }
        on = !on;
        tick += 1;
        if tick % 2 == 0 {
            puts(b"t=");
            put_u64((mtime() - start) / mtime_hz);
            puts(b"s hz=");
            put_u64(mtime_hz);
            puts(b"\r\n");
        }
    }
}
