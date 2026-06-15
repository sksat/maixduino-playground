//! K210 <-> onboard ESP32 over the nina-fw SPI protocol (WiFi step 2).
//!
//! The Maixduino's ESP32 runs nina-fw (WiFiNINA), driven over SPI with a
//! READY/CS handshake. k210-hal's SPI is incomplete and the handshake pins are
//! GPIO anyway, so we bit-bang SPI on GPIOHS. This step does one transaction,
//! GET_FW_VERSION (cmd 0x37), and prints the version string -- proving the SPI
//! link + nina-fw end to end.
//!
//! Pins (K210 IO -> ESP32, per the working MaixPy driver; note CS/READY are
//! swapped vs the schematic net names):
//!   EN=IO8  CS=IO25  READY=IO9  SCLK=IO27  MOSI=IO28  MISO=IO26
//! Two gotchas found on hardware: the GPIOHS function default leaves the pad
//! input buffer off (enable FPIOA `ie_en` on READY/MISO), and the nina slave
//! shifts MISO out on the rising edge, so sample MISO *before* raising SCLK.
//!
//! nina framing: cmd = E0 <cmd> <nparams> [<len> <data..>]... EE
//!              reply = E0 <cmd|80> <nparams> [<len> <data..>]... EE
//! Handshake: READY low = slave ready; after CS low it goes high (selected/busy).

#![no_std]
#![no_main]

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const CLINT_MTIME: *const u64 = 0x0200_BFF8 as *const u64;
const MTIME_HZ: u64 = 7_800_000;
const BAUD: u32 = 115_200;

// GPIOHS channel assignments.
const EN: u32 = 0; // IO8  out (ESP32_EN)
const CS: u32 = 1; // IO25 out (chip select)
const RDY: u32 = 2; // IO9  in  (handshake/ready)
const SCLK: u32 = 3; // IO27 out
const MOSI: u32 = 4; // IO28 out
const MISO: u32 = 5; // IO26 in

const START_CMD: u8 = 0xE0;
const END_CMD: u8 = 0xEE;
const REPLY_FLAG: u8 = 0x80;
const GET_FW_VERSION: u8 = 0x37;

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
fn put_hex(b: u8) {
    let h = b"0123456789abcdef";
    putc(h[(b >> 4) as usize]);
    putc(h[(b & 0xf) as usize]);
}
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}
fn mtime() -> u64 {
    unsafe { core::ptr::read_volatile(CLINT_MTIME) }
}

fn gpiohs() -> &'static pac::gpiohs::RegisterBlock {
    unsafe { &*pac::GPIOHS::ptr() }
}
fn gpo(ch: u32, hi: bool) {
    let g = gpiohs();
    let b = g.output_val.read().bits();
    let nb = if hi { b | (1 << ch) } else { b & !(1 << ch) };
    unsafe { g.output_val.write(|w| w.bits(nb)) };
}
fn gpi(ch: u32) -> bool {
    (gpiohs().input_val.read().bits() >> ch) & 1 != 0
}

/// Bit-bang one SPI byte, MSB first; returns the byte clocked in on MISO. The
/// nina slave shifts MISO out on the rising edge, so sample it before raising.
fn xfer(byte: u8) -> u8 {
    let mut inb = 0u8;
    let mut i = 8;
    while i > 0 {
        i -= 1;
        gpo(MOSI, (byte >> i) & 1 != 0);
        delay(40);
        let bit = gpi(MISO) as u8;
        gpo(SCLK, true);
        delay(40);
        gpo(SCLK, false);
        inb = (inb << 1) | bit;
    }
    inb
}

/// Wait until READY reaches `want`, up to `ms` milliseconds. Returns success.
fn wait_ready(want: bool, ms: u64) -> bool {
    let end = mtime() + ms * (MTIME_HZ / 1000);
    while mtime() < end {
        if gpi(RDY) == want {
            return true;
        }
    }
    gpi(RDY) == want
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _en = fpioa.io8.into_function(fpioa::GPIOHS0);
    let _cs = fpioa.io25.into_function(fpioa::GPIOHS1);
    let _rdy = fpioa.io9.into_function(fpioa::GPIOHS2);
    let _sclk = fpioa.io27.into_function(fpioa::GPIOHS3);
    let _mosi = fpioa.io28.into_function(fpioa::GPIOHS4);
    let _miso = fpioa.io26.into_function(fpioa::GPIOHS5);

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(BAUD.bps(), &clocks);

    // Directions + enable the input pads (GPIOHS function default leaves ie off).
    let g = gpiohs();
    unsafe {
        g.output_en
            .write(|w| w.bits((1 << EN) | (1 << CS) | (1 << SCLK) | (1 << MOSI)));
        g.input_en.write(|w| w.bits((1 << RDY) | (1 << MISO)));
        for io in [9usize, 26] {
            (*pac::FPIOA::ptr()).io[io]
                .modify(|_, w| w.ie_en().set_bit().st().set_bit().pu().set_bit().pd().clear_bit());
        }
    }

    // Idle the bus and reset the ESP32 into nina-fw.
    gpo(SCLK, false);
    gpo(MOSI, true);
    gpo(CS, true);
    gpo(EN, false);
    delay(20_000_000);
    gpo(EN, true);
    let boot = mtime() + 2 * MTIME_HZ; // ~2 s for nina-fw to come up
    while mtime() < boot {}

    puts(b"\nnina SPI GET_FW_VERSION\n");

    // --- send command (E0 37 00 EE), then read the reply in a separate frame ---
    wait_ready(false, 1000);
    gpo(CS, false);
    wait_ready(true, 10);
    xfer(START_CMD);
    xfer(GET_FW_VERSION);
    xfer(0x00);
    xfer(END_CMD);
    gpo(CS, true);

    wait_ready(false, 1000);
    gpo(CS, false);
    wait_ready(true, 10);
    let mut start_ok = false;
    let mut tries = 0;
    while tries < 64 {
        if xfer(0xff) == START_CMD {
            start_ok = true;
            break;
        }
        tries += 1;
    }
    let cmd = xfer(0xff);
    let nparams = xfer(0xff);
    let len = xfer(0xff);
    let mut ver = [0u8; 16];
    let n = if (len as usize) < ver.len() { len as usize } else { ver.len() };
    for v in ver.iter_mut().take(n) {
        *v = xfer(0xff);
    }
    let end = xfer(0xff);
    gpo(CS, true);

    puts(b"reply: cmd=");
    put_hex(cmd);
    puts(b" nparams=");
    put_hex(nparams);
    puts(b" len=");
    put_hex(len);
    puts(b" end=");
    put_hex(end);
    putc(b'\n');

    puts(b"fw version: ");
    for &c in ver.iter().take(n) {
        if (0x20..0x7f).contains(&c) {
            putc(c);
        }
    }
    putc(b'\n');

    if start_ok && cmd == (GET_FW_VERSION | REPLY_FLAG) && nparams == 1 && end == END_CMD {
        puts(b"PASS: nina-fw responds over SPI\n");
    } else {
        puts(b"FAIL: no clean nina reply\n");
    }

    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
