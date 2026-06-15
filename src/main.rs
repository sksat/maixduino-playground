//! K210 DVP camera, max resolution: capture an OV2640 JPEG at UXGA (1600x1200)
//! and send it over serial. The OV2640's RGB565 path tops out at SVGA, so the
//! sensor's full 2 MP resolution is only reachable in JPEG mode -- and JPEG is
//! compressed, so the transfer stays small even at 115200.
//!
//! The DVP captures the JPEG byte stream into SRAM; we scan it on-device for the
//! SOI..EOI markers (FF D8 .. FF D9) and dump only that, so the host just writes
//! the bytes to a .jpg.

#![no_std]
#![no_main]

mod dvp;

use panic_halt as _;

use dvp::{ov2640_jpeg_uxga, ov2640_read_id, Dvp, ImageFormat};
use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const UNCACHED_OFFSET: usize = 0x4000_0000;

// DVP capture buffer: fixed geometry, big enough to hold a UXGA JPEG (~100-300 KB).
const CW: usize = 1024;
const CH: usize = 512;
const NBYTES: usize = CW * CH * 2;

#[repr(C, align(64))]
struct Buf {
    w: [u32; CW * CH / 2],
}
static mut BUF: Buf = Buf { w: [0; CW * CH / 2] };

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
fn put_hex8(b: u8) {
    let h = b"0123456789abcdef";
    putc(h[(b >> 4) as usize]);
    putc(h[(b & 0xf) as usize]);
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

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
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

    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc)
            .power_sel
            .modify(|_, w| w.power_mode_sel6().clear_bit().power_mode_sel7().clear_bit());
        (*sc).clk_en_cent.modify(|_, w| w.apb2_clk_en().set_bit());
        (*sc).clk_en_peri.modify(|_, w| w.spi0_clk_en().set_bit());
        (*sc).misc.modify(|_, w| w.spi_dvp_data_enable().set_bit());
    }

    let dvp = Dvp::new(p.DVP);
    dvp.init();
    let _ = ov2640_read_id(&dvp); // ensure the sensor is present/talking
    ov2640_jpeg_uxga(&dvp);
    dvp.set_image_format(ImageFormat::RGB);
    dvp.set_image_size(false, CW as u16, CH as u16);

    let cached = unsafe { core::ptr::addr_of!(BUF.w) } as *const u32 as usize;
    let buf = (cached - UNCACHED_OFFSET) as *const u32;
    // The DVP stores each 32-bit word big-endian relative to the byte stream, so
    // read the JPEG byte at index `i` as the big-endian byte of word `i/4`.
    let byte = |i: usize| -> u8 {
        let w = unsafe { core::ptr::read_volatile(buf.add(i >> 2)) };
        (w >> (24 - 8 * (i & 3))) as u8
    };
    dvp.set_ai_addr(None);
    dvp.set_display_addr(Some(cached as u32));
    dvp.set_auto(false);

    delay(40_000_000);

    loop {
        dvp.get_image();

        // Find JPEG SOI (FF D8) and EOI (FF D9) in the big-endian byte stream.
        let mut soi = 0;
        let mut found = false;
        let mut i = 0;
        while i + 1 < NBYTES {
            if byte(i) == 0xff && byte(i + 1) == 0xd8 {
                soi = i;
                found = true;
                break;
            }
            i += 1;
        }
        let mut eoi = 0;
        if found {
            let mut j = soi + 2;
            found = false;
            while j + 1 < NBYTES {
                if byte(j) == 0xff && byte(j + 1) == 0xd9 {
                    eoi = j + 2;
                    found = true;
                    break;
                }
                j += 1;
            }
        }

        if found {
            puts(b"\r\nJPGSTART ");
            put_dec((eoi - soi) as u32);
            putc(b'\n');
            for k in soi..eoi {
                putc(byte(k));
            }
            puts(b"\r\nJPGEND\r\n");
        } else {
            puts(b"\r\nno JPEG first16=");
            for k in 0..16 {
                put_hex8(byte(k));
            }
            puts(b"\r\n");
        }
        delay(40_000_000);
    }
}
