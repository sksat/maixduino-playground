//! Bare-metal Rust on the Maixduino (Kendryte K210).
//!
//! - **Serial (UARTHS) — verified working** on /dev/ttyUSB0 @115200: prints
//!   "hello" then "on"/"off" each loop. (Keep writes <= the 8-byte TX FIFO;
//!   k210-hal's write_all truncates longer bursts.)
//! - **RGB red LED (K210 IO13) blink — driven, but NOT visually confirmed.**
//!   Per the schematic/datasheet the RGB LED is IO13(R)/IO12(G)/IO14(B), but it
//!   has a 4.7K series resistor (~0.3 mA, extremely dim) and we never got a
//!   confirmed visible blink out of it -- via this code, via the regular GPIO
//!   peripheral, or via MaixPy. We drive it the same way the official Arduino
//!   core does (GPIOHS). See docs/finding-the-led.md for the full saga and the
//!   one objective test still outstanding (a multimeter on IO13).

#![no_std]
#![no_main]

use panic_halt as _;

use embedded_io::Write as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;

/// RGB red LED: K210 IO13, active-low, routed to GPIOHS channel 0.
const LED_CH: usize = 0;

#[riscv_rt::entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _led = fpioa.io13.into_function(fpioa::GPIOHS0); // RGB red = K210 IO13

    let clocks = k210_hal::clock::Clocks::new();
    let (mut tx, _rx) = p.UARTHS.configure(115_200.bps(), &clocks).split();
    tx.write_all(b"hello\r\n").ok();

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
            gpiohs
                .output_val
                .modify(|r, w| unsafe { w.bits(r.bits() & !(1 << LED_CH)) }); // active-low: lit
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

fn delay(count: u32) {
    for _ in 0..count {
        unsafe { core::arch::asm!("nop") };
    }
}
