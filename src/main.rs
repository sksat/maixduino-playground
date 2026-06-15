//! K210 machine-timer interrupt demo. Everything so far has been polled; this one
//! is interrupt-driven: the CPU sleeps in `wfi` and the CLINT machine-timer ISR
//! does all the work -- re-arming `mtimecmp`, counting ticks, and toggling the
//! onboard LED (IO6). Each serial line is printed on waking from one interrupt,
//! so the host-side timestamps between lines confirm the 2 Hz tick rate.
//!
//! riscv-rt only installs the trap vector (mtvec); we still have to enable the
//! machine-timer interrupt (mie.MTIE) and global interrupts (mstatus.MIE)
//! ourselves, which we do with raw CSR writes to avoid pulling in the riscv crate.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;

// CLINT (hart 0). mtime self-calibrated earlier to ~7.8 MHz (= CPU/50).
const MTIME: *const u64 = 0x0200_BFF8 as *const u64;
const MTIMECMP0: *mut u64 = 0x0200_4000 as *mut u64;
const MTIME_HZ: u64 = 7_799_258;
const INTERVAL: u64 = MTIME_HZ / 2; // 2 Hz ISR -> IO6 toggles -> 1 Hz LED blink

const LED_CH: u32 = 0; // IO6 -> GPIOHS channel 0 (active-low)

static TICKS: AtomicU32 = AtomicU32::new(0);

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
fn put_dec(mut v: u32) {
    if v == 0 {
        putc(b'0');
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = 0;
    while v > 0 {
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        putc(buf[i]);
    }
}

/// Machine-timer interrupt handler (riscv-rt dispatches core interrupt 7 here).
#[export_name = "MachineTimer"]
fn machine_timer() {
    unsafe {
        // Re-arm relative to the previous compare value to avoid drift; this also
        // clears the pending timer interrupt (mtip stays high while mtime>=cmp).
        let next = core::ptr::read_volatile(MTIMECMP0).wrapping_add(INTERVAL);
        core::ptr::write_volatile(MTIMECMP0, next);

        // Toggle IO6 (the LED) straight from the ISR.
        let g = &*pac::GPIOHS::ptr();
        g.output_val
            .modify(|r, w| w.bits(r.bits() ^ (1 << LED_CH)));
    }
    TICKS.fetch_add(1, Ordering::Relaxed);
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _led = fpioa.io6.into_function(fpioa::GPIOHS0);
    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(115_200.bps(), &clocks);

    // GPIOHS channel 0 as output (input_en must be cleared or the pad stays Hi-Z).
    let gpiohs = p.GPIOHS;
    gpiohs
        .output_en
        .modify(|r, w| unsafe { w.bits(r.bits() | (1 << LED_CH)) });
    gpiohs
        .input_en
        .modify(|r, w| unsafe { w.bits(r.bits() & !(1 << LED_CH)) });

    puts(b"\r\n-- K210 machine-timer interrupt (2 Hz, ISR drives IO6) --\r\n");

    unsafe {
        // Arm the first compare, then enable timer + global machine interrupts.
        let now = core::ptr::read_volatile(MTIME);
        core::ptr::write_volatile(MTIMECMP0, now.wrapping_add(INTERVAL));
        core::arch::asm!("csrs mie, {0}", in(reg) 1usize << 7); // MTIE
        core::arch::asm!("csrs mstatus, {0}", in(reg) 1usize << 3); // MIE
    }

    loop {
        let t = TICKS.load(Ordering::Relaxed);
        puts(b"tick=");
        put_dec(t);
        puts(b" (ISR-driven, IO6 toggling)\r\n");
        // Sleep until the next timer interrupt bumps the counter.
        while TICKS.load(Ordering::Relaxed) == t {
            unsafe { core::arch::asm!("wfi") };
        }
    }
}
