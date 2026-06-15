//! K210 DVP camera, step 1: bring up the DVP + SCCB bus and read the OV2640
//! sensor's chip ID over serial. A correct ID (manuf 0x7fa2, product 0x2642)
//! proves the SCCB bus works and the camera is attached -- the prerequisite for
//! capturing frames.
//!
//! Board notes (Maixduino): the camera connector wires the DVP signals to fixed
//! K210 IOs (40..47), muxed to the CMOS_*/SCCB_* functions. The DVP data pins are
//! 3.3 V (banks 6 & 7), and the 24-pin camera FFC must be inserted the right way
//! up -- a reversed cable leaves the sensor unpowered and every SCCB read returns
//! 0xff (no device ACK), which looks exactly like a missing camera.

#![no_std]
#![no_main]

mod dvp;

use panic_halt as _;

use dvp::{ov2640_read_id, Dvp};
use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;

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
fn put_hex16(v: u16) {
    let h = b"0123456789abcdef";
    for s in (0..4).rev() {
        putc(h[((v >> (s * 4)) & 0xf) as usize]);
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

    // Serial.
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    // DVP / camera pins.
    let _sda = fpioa.io40.into_function(fpioa::SCCB_SDA);
    let _scl = fpioa.io41.into_function(fpioa::SCCB_SCLK);
    let _rst = fpioa.io42.into_function(fpioa::CMOS_RST);
    let _vsync = fpioa.io43.into_function(fpioa::CMOS_VSYNC);
    let _pwdn = fpioa.io44.into_function(fpioa::CMOS_PWDN);
    let _href = fpioa.io45.into_function(fpioa::CMOS_HREF);
    let _xclk = fpioa.io46.into_function(fpioa::CMOS_XCLK);
    let _pclk = fpioa.io47.into_function(fpioa::CMOS_PCLK);

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(115_200.bps(), &clocks);

    // DVP IO is 3.3 V on the Maixduino (banks 6 & 7); route the DVP data lines.
    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc)
            .power_sel
            .modify(|_, w| w.power_mode_sel6().clear_bit().power_mode_sel7().clear_bit());
        (*sc).misc.modify(|_, w| w.spi_dvp_data_enable().set_bit());
    }

    let dvp = Dvp::new(p.DVP);
    dvp.init();

    let (manuf, pid) = ov2640_read_id(&dvp);
    let ok = manuf == 0x7fa2 && pid == 0x2642;

    puts(b"\r\n-- K210 DVP / OV2640 chip ID --\r\n");
    loop {
        puts(b"manuf=0x");
        put_hex16(manuf);
        puts(b" product=0x");
        put_hex16(pid);
        if ok {
            puts(b"  -> OV2640 detected, SCCB OK\r\n");
        } else {
            puts(b"  -> no ACK (0xffff); check the camera FFC orientation\r\n");
        }
        delay(20_000_000);
    }
}
