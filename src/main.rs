//! K210 clock-tree readout. Decodes the sysctl PLL registers and clock selectors
//! into real frequencies and prints them, instead of just trusting the boot-ROM
//! defaults. Cross-checks the decoded CPU clock against the independently
//! measured CLINT mtime rate (commit 69cfde6 found mtime = aclk/50 = 7.80 MHz).
//!
//! K210 clock tree (26 MHz crystal on IN0):
//!   PLLn  = IN0 / (clkr+1) * (clkf+1) / (clkod+1)
//!   aclk  = PLL0 / 2^(aclk_divider_sel+1)   (CPU clock; or IN0 if aclk_sel=0)
//!   apbN  = aclk / (apbN_clk_sel+1)

#![no_std]
#![no_main]

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const IN0_HZ: u64 = 26_000_000; // onboard crystal
const MTIME_HZ_MEASURED: u64 = 7_799_258; // commit 69cfde6, mtime = aclk/50

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
/// Print a frequency in Hz as "DDD.DD MHz".
fn put_mhz(hz: u64) {
    put_dec((hz / 1_000_000) as u32);
    putc(b'.');
    let frac = (hz % 1_000_000) / 10_000;
    if frac < 10 {
        putc(b'0');
    }
    put_dec(frac as u32);
    puts(b" MHz");
}
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}

fn pll_freq(r: u8, f: u8, od: u8) -> u64 {
    IN0_HZ * (f as u64 + 1) / ((r as u64 + 1) * (od as u64 + 1))
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(115_200.bps(), &clocks);

    let sc = unsafe { &*pac::SYSCTL::ptr() };

    loop {
        let p0 = sc.pll0.read();
        let p1 = sc.pll1.read();
        let p2 = sc.pll2.read();
        let pll0 = pll_freq(p0.clkr().bits(), p0.clkf().bits(), p0.clkod().bits());
        let pll1 = pll_freq(p1.clkr().bits(), p1.clkf().bits(), p1.clkod().bits());
        let pll2 = pll_freq(p2.clkr().bits(), p2.clkf().bits(), p2.clkod().bits());

        let cs = sc.clk_sel0.read();
        let aclk = if cs.aclk_sel().bit() {
            pll0 / (2u64 << cs.aclk_divider_sel().bits())
        } else {
            IN0_HZ
        };
        let apb0 = aclk / (cs.apb0_clk_sel().bits() as u64 + 1);
        let apb1 = aclk / (cs.apb1_clk_sel().bits() as u64 + 1);
        let apb2 = aclk / (cs.apb2_clk_sel().bits() as u64 + 1);

        puts(b"\r\n-- K210 clock tree (IN0 = 26.00 MHz) --\r\n");
        puts(b"PLL0 = ");
        put_mhz(pll0);
        puts(b"   PLL1 = ");
        put_mhz(pll1);
        puts(b"   PLL2 = ");
        put_mhz(pll2);
        puts(b"\r\nCPU (aclk) = ");
        put_mhz(aclk);
        puts(b"   APB0 = ");
        put_mhz(apb0);
        puts(b"   APB1 = ");
        put_mhz(apb1);
        puts(b"   APB2 = ");
        put_mhz(apb2);
        // Cross-check against the independently measured CLINT mtime (= aclk/50).
        let expect = aclk / 50;
        let diff = if expect > MTIME_HZ_MEASURED {
            expect - MTIME_HZ_MEASURED
        } else {
            MTIME_HZ_MEASURED - expect
        };
        puts(b"\r\naclk/50 = ");
        put_mhz(expect);
        puts(b" vs measured mtime ");
        put_mhz(MTIME_HZ_MEASURED);
        if diff < 50_000 {
            puts(b"  -> MATCH (within 0.05 MHz)\r\n");
        } else {
            puts(b"  -> differ\r\n");
        }

        delay(20_000_000);
    }
}
