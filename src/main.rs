//! Bare-metal Rust on the Kendryte K210 (Maixduino).
//!
//! Two things at once, to exercise the toolchain end to end:
//!   1. "Hello" over UARTHS -> the onboard USB-UART (IO5 = TX), 115200 baud.
//!   2. Blink the onboard red LED, wired to K210 IO13 (active-low).
//!
//! Pin muxing and the UART go through `k210-hal`. The LED is driven at the
//! GPIOHS *register* level via the PAC, because the HAL's GPIOHS support is only
//! half-built (it has an input constructor and a literal `// todo: all modes`,
//! but no output pin) -- a nice illustration of dropping below the HAL when its
//! coverage runs out.

#![no_std]
#![no_main]

use panic_halt as _;

// Brings `write_all` (embedded_io::Write) into scope; Tx<UARTHS> implements it.
use embedded_io::Write as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;

/// Maixduino red LED: K210 IO13, active-low. We route it to GPIOHS channel 0.
const LED_GPIOHS_CH: usize = 0;

#[riscv_rt::entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    // Pin mux: IO5 -> UARTHS TX (wired to the onboard USB serial),
    //          IO13 -> GPIOHS0 (the red LED).
    let _tx_pin = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _led_pin = fpioa.io13.into_function(fpioa::GPIOHS0);

    let clocks = k210_hal::clock::Clocks::new();
    let (mut tx, _rx) = p.UARTHS.configure(115_200.bps(), &clocks).split();

    tx.write_all(b"Hello from Rust on K210 (Maixduino)!\r\n").ok();

    // GPIOHS channel 0 as output, driven directly through the PAC registers.
    let gpiohs = p.GPIOHS;
    gpiohs
        .output_en
        .modify(|r, w| unsafe { w.bits(r.bits() | (1 << LED_GPIOHS_CH)) });

    let mut on = true;
    loop {
        if on {
            // active-low: clear the bit to sink current and light the LED.
            gpiohs
                .output_val
                .modify(|r, w| unsafe { w.bits(r.bits() & !(1 << LED_GPIOHS_CH)) });
            tx.write_all(b"LED on\r\n").ok();
        } else {
            gpiohs
                .output_val
                .modify(|r, w| unsafe { w.bits(r.bits() | (1 << LED_GPIOHS_CH)) });
            tx.write_all(b"LED off\r\n").ok();
        }
        on = !on;
        delay(7_000_000);
    }
}

/// Crude busy-wait. Good enough to make the blink visible; swap for a real timer
/// once timing matters.
fn delay(count: u32) {
    for _ in 0..count {
        unsafe { core::arch::asm!("nop") };
    }
}
