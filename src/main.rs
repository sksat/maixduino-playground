//! K210 <-> onboard ESP32 link bring-up (WiFi step 1).
//!
//! The Maixduino's ESP32-WROOM-32 is wired to the K210 over both UART and SPI:
//!   IO6 = ESP32_U0TX  -> K210 RX   (UART1_RX)
//!   IO7 = ESP32_U0RX  <- K210 TX   (UART1_TX)
//!   IO8 = ESP32_EN     (active-high chip enable / reset)
//!   IO9/IO27/IO28/...  = the nina-fw SPI interface (not used yet)
//!
//! This step just proves the ESP32 is alive: pulse EN to reset it, capture its
//! UART boot banner, forward it to the host (UARTHS on IO5), then send "AT\r\n"
//! to see whether it speaks the esp-at command set. The banner tells us which
//! firmware is flashed, which decides the WiFi path (UART AT vs SPI nina-fw).

#![no_std]
#![no_main]

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const CLINT_MTIME: *const u64 = 0x0200_BFF8 as *const u64;
const MTIME_HZ: u64 = 7_800_000; // ~CPU/50, from the clock demo
const BAUD: u32 = 115_200;

fn mtime() -> u64 {
    unsafe { core::ptr::read_volatile(CLINT_MTIME) }
}

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
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX); // host
    let _e_rx = fpioa.io6.into_function(fpioa::UART1_RX); // <- ESP32 U0TX
    let _e_tx = fpioa.io7.into_function(fpioa::UART1_TX); // -> ESP32 U0RX
    let _en = fpioa.io8.into_function(fpioa::GPIOHS0); // ESP32_EN

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(BAUD.bps(), &clocks);

    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc).clk_en_peri.modify(|_, w| w.uart1_clk_en().set_bit());
    }
    let _u1 = p.UART1.configure(BAUD.bps(), &clocks);
    let u1 = unsafe { &*pac::UART1::ptr() };

    puts(b"\nESP32 probe: UART1 IO6(rx)/IO7(tx) @115200, EN=IO8\n");

    // Pulse ESP32_EN: low (reset) -> high (run). GPIOHS channel 0 = IO8.
    let gh = unsafe { &*pac::GPIOHS::ptr() };
    unsafe {
        gh.output_en.modify(|r, w| w.bits(r.bits() | 1));
        gh.output_val.modify(|r, w| w.bits(r.bits() & !1)); // EN low = reset
    }
    delay(20_000_000);
    unsafe {
        gh.output_val.modify(|r, w| w.bits(r.bits() | 1)); // EN high = run
    }
    puts(b"ESP32 enabled; forwarding its UART output:\n----\n");

    // Forward any pending ESP32 byte to the host for `secs` real seconds (timed by
    // mtime, so it doesn't depend on the debug build's loop speed).
    let forward_secs = |secs: u64| {
        let end = mtime() + secs * MTIME_HZ;
        while mtime() < end {
            if u1.lsr.read().bits() & 1 != 0 {
                let b = (u1.rbr_dll_thr.read().bits() & 0xff) as u8;
                putc(b);
            }
        }
    };

    // Phase 1: capture the boot banner (~2 s).
    forward_secs(2);

    // Phase 2: probe esp-at. If the firmware is esp-at, "AT\r\n" -> "OK"; if it's
    // nina-fw (SPI WiFi), the UART stays silent and we'll drive it over SPI later.
    let mut k = 0;
    while k < 3 {
        puts(b"\n[host->esp32] AT\r\n");
        for &b in b"AT\r\n" {
            u1.rbr_dll_thr.write(|w| unsafe { w.bits(b as u32) });
        }
        forward_secs(1);
        k += 1;
    }
    puts(b"\n----\n[done] if nothing came back, the ESP32 likely runs nina-fw (SPI)\n");

    loop {
        forward_secs(2);
    }
}
