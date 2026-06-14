//! Bare-metal Rust on the Maixduino (Kendryte K210): blink the onboard LED and
//! print over serial.
//!
//! The onboard user LED is on **K210 IO6** (active-low). That was found the hard
//! way -- by sweeping every IO -- because the "IO13/12/14" you'll find online are
//! *Arduino* pin numbers for a different board layout, not the Maixduino's K210
//! IOs (see docs/finding-the-led.md for the whole saga). IO6's FPIOA default is
//! even RESV0 (an unassigned, free pin), which fits a user LED.
//!
//! - LED: IO6 -> GPIOHS channel 0, driven at the PAC register level (k210-hal's
//!   GPIOHS output is unfinished; GPIOHS is AHB-clocked so it needs no clock
//!   setup -- unlike the regular GPIO, which is why GPIOHS is the easy path).
//! - Serial: UARTHS on IO5 -> the onboard USB-UART, 115200. Prints "on"/"off"
//!   each toggle. (Keep writes <= the 8-byte TX FIFO; write_all truncates more.)

#![no_std]
#![no_main]

use panic_halt as _;

use embedded_io::Write as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;

/// Onboard LED: K210 IO6, active-low, routed to GPIOHS channel 0.
const LED_CH: usize = 0;

#[riscv_rt::entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    // Pin mux: IO5 -> UARTHS TX (onboard USB serial), IO6 -> GPIOHS0 (the LED).
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _led = fpioa.io6.into_function(fpioa::GPIOHS0);

    let clocks = k210_hal::clock::Clocks::new();
    let (mut tx, _rx) = p.UARTHS.configure(115_200.bps(), &clocks).split();
    tx.write_all(b"hello\r\n").ok();

    // GPIOHS channel 0 as output: output_en set, input_en cleared (= direction).
    // Clearing input_en matters -- with it left on, the pad stays a high-Z input
    // and never drives.
    let gpiohs = p.GPIOHS;
    gpiohs
        .output_en
        .modify(|r, w| unsafe { w.bits(r.bits() | (1 << LED_CH)) });
    gpiohs
        .input_en
        .modify(|r, w| unsafe { w.bits(r.bits() & !(1 << LED_CH)) });

    let mut on = true;
    loop {
        if on {
            // active-low: drive low to light the LED.
            gpiohs
                .output_val
                .modify(|r, w| unsafe { w.bits(r.bits() & !(1 << LED_CH)) });
            tx.write_all(b"on\r\n").ok();
        } else {
            gpiohs
                .output_val
                .modify(|r, w| unsafe { w.bits(r.bits() | (1 << LED_CH)) });
            tx.write_all(b"off\r\n").ok();
        }
        on = !on;
        delay(2_000_000);
    }
}

/// Crude busy-wait; good enough to make the blink visible.
fn delay(count: u32) {
    for _ in 0..count {
        unsafe { core::arch::asm!("nop") };
    }
}
