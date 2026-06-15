//! K210 RTC demo: set the wall clock, watch it tick, and cross-check its 1 Hz
//! against the CLINT machine timer (`mtime`).
//!
//! The RTC and `mtime` are independent clocks. `mtime` was calibrated in the
//! clock demo to ~7.80 MHz (= CPU/50). If the RTC really advances at 1 Hz, then
//! exactly one `mtime`-second (~7.80M ticks) should elapse between consecutive
//! RTC second changes — which is what this prints and checks.

#![no_std]
#![no_main]

mod rtc;

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;
use rtc::Rtc;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const CLINT_MTIME: *const u64 = 0x0200_BFF8 as *const u64;
const BAUD: u32 = 115_200;

/// Expected mtime ticks per real second (from the clock demo: CPU ~390 MHz / 50).
const MTIME_HZ: u32 = 7_799_258;

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
/// Two-digit zero-padded (for hh:mm:ss).
fn put_2(v: u8) {
    putc(b'0' + (v / 10) % 10);
    putc(b'0' + v % 10);
}
fn put_time(h: u8, m: u8, s: u8) {
    put_2(h);
    putc(b':');
    put_2(m);
    putc(b':');
    put_2(s);
}
fn mtime() -> u64 {
    unsafe { core::ptr::read_volatile(CLINT_MTIME) }
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(BAUD.bps(), &clocks);

    let rtc = Rtc::new(p.RTC);
    rtc.init();
    rtc.set_datetime(2026, 6, 16, 2, 12, 0, 0); // 2026-06-16 (Tue) 12:00:00

    puts(b"RTC demo: set 2026-06-16 12:00:00\n");
    let (y, mo, d) = rtc.date();
    puts(b"date readback: ");
    put_dec(y as u32);
    putc(b'-');
    put_2(mo);
    putc(b'-');
    put_2(d);
    putc(b'\n');
    puts(b"checking 1 Hz against mtime (expect ~7.80M ticks/RTC-second):\n");

    // Align to a second boundary, then measure several full RTC seconds.
    let mut prev_s = rtc.time().2;
    loop {
        let s = rtc.time().2;
        if s != prev_s {
            prev_s = s;
            break;
        }
    }
    let mut prev_mt = mtime();

    let mut all_ok = true;
    let mut n = 0;
    while n < 6 {
        let (h, m, s) = rtc.time();
        if s != prev_s {
            let now = mtime();
            let dmt = (now - prev_mt) as u32;
            put_time(h, m, s);
            puts(b"  d-mtime=");
            put_dec(dmt);
            // within 2% of one mtime-second?
            let diff = if dmt > MTIME_HZ { dmt - MTIME_HZ } else { MTIME_HZ - dmt };
            let ok = diff < MTIME_HZ / 50;
            if ok {
                puts(b"  ok\n");
            } else {
                puts(b"  OFF\n");
                all_ok = false;
            }
            prev_s = s;
            prev_mt = now;
            n += 1;
        }
    }

    if all_ok {
        puts(b"PASS: RTC ticks at 1 Hz (validated by the independent mtime clock)\n");
    } else {
        puts(b"FAIL: RTC second != one mtime-second\n");
    }

    // Keep showing the live clock so the stream is observable at any time.
    puts(b"live clock:\n");
    let mut prev = prev_s;
    loop {
        let (h, m, s) = rtc.time();
        if s != prev {
            puts(b"  ");
            put_time(h, m, s);
            putc(b'\n');
            prev = s;
        }
    }
}
