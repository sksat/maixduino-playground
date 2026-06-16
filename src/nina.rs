//! nina-fw (WiFiNINA) command protocol for the Maixduino's onboard ESP32, over the
//! K210 **hardware SPI0**. (An earlier bit-bang version worked for a single command
//! but the unoptimized-build clock jitter corrupted multi-command traffic and wedged
//! the ESP32; the hardware SSI gives a clean, consistent clock.)
//!
//! SPI0 drives SCLK=IO27, MOSI=IO28 (SPI0_D0), MISO=IO26 (SPI0_D1); the real chip
//! select and handshake stay on GPIO (CS=IO25, READY=IO9, EN=IO8) so we can hold CS
//! low across a whole framed transaction. The SSI's own SS0 is selected (so writes
//! clock) but left unmuxed, so it doesn't touch the ESP32.
//!
//! nina framing: cmd = E0 <cmd> <nparams> [<len><data>..] EE  (padded to a mult. of 4)
//!              reply = E0 <cmd|0x80> <nparams> [<len><data>..] EE
//! Handshake: READY low = slave ready; after CS low it goes high (selected/busy);
//! command and reply go in separate CS frames.

#![allow(dead_code)]

use k210_hal::pac;

// GPIOHS channels (CS/READY/EN only; SCLK/MOSI/MISO are on hardware SPI0).
const EN: u32 = 0; // IO8
const CS: u32 = 1; // IO25
const RDY: u32 = 2; // IO9

const START: u8 = 0xE0;
const END: u8 = 0xEE;
pub const REPLY_FLAG: u8 = 0x80;

// nina command numbers (subset).
pub const CMD_GET_FW_VERSION: u8 = 0x37;
pub const CMD_START_SCAN_NETWORKS: u8 = 0x36;
pub const CMD_SCAN_NETWORKS: u8 = 0x27;
pub const CMD_GET_IDX_RSSI: u8 = 0x32;
pub const CMD_GET_IDX_ENCT: u8 = 0x33;

const CLINT_MTIME: *const u64 = 0x0200_BFF8 as *const u64;
const MTIME_HZ: u64 = 7_800_000;

/// Retries the last `request` needed (diagnostics).
pub static mut RETRIES: u32 = 0;

fn mtime() -> u64 {
    unsafe { core::ptr::read_volatile(CLINT_MTIME) }
}
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}
pub fn sleep_ms(ms: u64) {
    let end = mtime() + ms * (MTIME_HZ / 1000);
    while mtime() < end {}
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

fn spi() -> &'static pac::spi0::RegisterBlock {
    unsafe { &*pac::SPI0::ptr() }
}

/// Full-duplex 8-bit SPI byte over hardware SPI0 (writing clocks one byte out on
/// MOSI and one in on MISO). The real CS is held by GPIO around the framed call.
pub fn ready() -> bool {
    gpi(RDY)
}

fn xfer(b: u8) -> u8 {
    let s = spi();
    unsafe { s.dr[0].write(|w| w.bits(b as u32)) };
    while s.rxflr.read().bits() == 0 {}
    (s.dr[0].read().bits() & 0xff) as u8
}

fn wait_ready(want: bool, ms: u64) -> bool {
    let end = mtime() + ms * (MTIME_HZ / 1000);
    while mtime() < end {
        if gpi(RDY) == want {
            return true;
        }
    }
    gpi(RDY) == want
}

fn frame_begin() {
    wait_ready(false, 100);
    gpo(CS, false);
    wait_ready(true, 10);
}
fn frame_end() {
    gpo(CS, true);
}

/// Configure SPI0 (mode 0, full duplex, 8-bit) and select its (unused) SS0.
fn spi_init() {
    use pac::spi0::ctrlr0::{FRAME_FORMAT_A, TMOD_A, WORK_MODE_A};
    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc).clk_en_cent.modify(|_, w| w.apb0_clk_en().set_bit());
        (*sc).clk_en_peri.modify(|_, w| w.spi0_clk_en().set_bit());
        let s = spi();
        s.ssienr.write(|w| w.bits(0)); // disable while configuring
        s.ctrlr0.write(|w| {
            w.work_mode()
                .variant(WORK_MODE_A::MODE0)
                .tmod()
                .variant(TMOD_A::TRANS_RECV)
                .frame_format()
                .variant(FRAME_FORMAT_A::STANDARD)
                .data_length()
                .bits(7) // 8-bit frames
        });
        s.spi_ctrlr0.reset();
        s.endian.write(|w| w.bits(0));
        s.baudr.write(|w| w.bits(100)); // sclk = ssi_clk / 100 (moderate, consistent)
        s.txftlr.write(|w| w.bits(0));
        s.rxftlr.write(|w| w.bits(0));
        s.imr.write(|w| w.bits(0));
        s.dmacr.write(|w| w.bits(0));
        s.ser.write(|w| w.bits(1)); // select SS0 (unmuxed) so writes actually clock
        s.ssienr.write(|w| w.bits(1)); // enable
    }
}

/// GPIOHS directions + input pad + SPI0 + reset the ESP32 into nina-fw. The caller
/// muxes IO8/IO25/IO9 to GPIOHS0/1/2 and IO27/IO28/IO26 to SPI0_SCLK/D0/D1.
pub fn init() {
    let g = gpiohs();
    unsafe {
        g.output_en.write(|w| w.bits((1 << EN) | (1 << CS)));
        g.input_en.write(|w| w.bits(1 << RDY));
        // READY=IO9: enable the pad input buffer (off by default for GPIOHS).
        (*pac::FPIOA::ptr()).io[9]
            .modify(|_, w| w.ie_en().set_bit().st().set_bit().pu().set_bit().pd().clear_bit());
    }
    spi_init();

    gpo(CS, true);
    gpo(EN, false);
    delay(20_000_000);
    gpo(EN, true);
    sleep_ms(2000); // nina-fw boot
}

/// One send+receive of a nina command; returns (nparams, framing_valid).
fn request_once(cmd: u8, params: &[&[u8]], resp: &mut [u8], lens: &mut [usize]) -> (usize, bool) {
    // send: E0 <cmd> <nparams> [<len><data>..] EE, padded to a multiple of 4
    frame_begin();
    xfer(START);
    xfer(cmd & 0x7f);
    xfer(params.len() as u8);
    let mut sent = 3u32;
    for p in params {
        xfer(p.len() as u8);
        sent += 1;
        for &b in *p {
            xfer(b);
            sent += 1;
        }
    }
    xfer(END);
    sent += 1;
    while sent % 4 != 0 {
        xfer(0xff);
        sent += 1;
    }
    frame_end();

    // receive: sync on START, then read framed params
    frame_begin();
    let mut ok = false;
    let mut tries = 0;
    while tries < 300 {
        if xfer(0xff) == START {
            ok = true;
            break;
        }
        tries += 1;
    }
    if !ok {
        frame_end();
        return (0, false);
    }
    let rcmd = xfer(0xff);
    let nparams = xfer(0xff) as usize;
    let mut off = 0;
    for i in 0..nparams {
        let l = xfer(0xff) as usize;
        for _ in 0..l {
            let b = xfer(0xff);
            if off < resp.len() {
                resp[off] = b;
                off += 1;
            }
        }
        if i < lens.len() {
            lens[i] = l;
        }
    }
    let end = xfer(0xff);
    frame_end();
    let valid = rcmd == (cmd | REPLY_FLAG) && end == END;
    (nparams, valid)
}

/// Send a nina command and read the reply, retrying a few times if the framing
/// doesn't validate (cheap insurance; the hardware SPI should rarely need it). The
/// reads here are idempotent. Returns the number of reply params (0 on failure).
pub fn request(cmd: u8, params: &[&[u8]], resp: &mut [u8], lens: &mut [usize]) -> usize {
    let mut attempt = 0;
    while attempt < 8 {
        let (n, valid) = request_once(cmd, params, resp, lens);
        if valid {
            unsafe { RETRIES = attempt };
            return n;
        }
        attempt += 1;
        sleep_ms(2);
    }
    unsafe { RETRIES = attempt };
    0
}
