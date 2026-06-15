//! K210 DVP camera, step 2b: capture an OV2640 RGB565 frame and dump it raw over
//! serial so the host can save it as an image file.
//!
//! Wire format, repeated every frame:
//!   "IMGSTART <w> <h>\n"  then  w*h*2 raw little-endian RGB565 bytes (buffer order)
//! The host syncs on the header and reads exactly w*h*2 bytes.

#![no_std]
#![no_main]

mod dvp;

use panic_halt as _;

use dvp::{ov2640_init, ov2640_read_id, ov2640_rgb565_vga, Dvp, ImageFormat};
use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const UNCACHED_OFFSET: usize = 0x4000_0000;

const W: usize = 640;
const H: usize = 480;
const BAUD: u32 = 1_500_000;

#[repr(C, align(64))]
struct Frame {
    px: [u32; W * H / 2],
}
static mut FRAME: Frame = Frame { px: [0; W * H / 2] };

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
    let _serial = p.UARTHS.configure(BAUD.bps(), &clocks);

    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc)
            .power_sel
            .modify(|_, w| w.power_mode_sel6().clear_bit().power_mode_sel7().clear_bit());
        // The 8 DVP data lines come in over the SPI0-shared pads, so SPI0 must be
        // clocked for them to be driven; then route them to the DVP.
        (*sc).clk_en_cent.modify(|_, w| w.apb2_clk_en().set_bit());
        (*sc).clk_en_peri.modify(|_, w| w.spi0_clk_en().set_bit());
        (*sc).misc.modify(|_, w| w.spi_dvp_data_enable().set_bit());
    }

    let dvp = Dvp::new(p.DVP);
    dvp.init();
    let _ = ov2640_read_id(&dvp);
    ov2640_init(&dvp);
    ov2640_rgb565_vga(&dvp); // bump 320x240 -> VGA 640x480, still RGB565
    dvp.set_image_format(ImageFormat::RGB);
    dvp.set_image_size(false, W as u16, H as u16);

    // The DVP's AXI master writes to the cached SRAM address (0x8000_0000 range);
    // the CPU reads back through the uncached alias (0x4000_0000) so it sees the
    // DVP's writes instead of stale cache.
    let cached = unsafe { core::ptr::addr_of!(FRAME.px) } as *const u32 as usize;
    let buf = (cached - UNCACHED_OFFSET) as *const u32;
    dvp.set_ai_addr(None);
    dvp.set_display_addr(Some(cached as u32));
    dvp.set_auto(false);

    delay(200_000_000); // auto-exposure / white-balance settle
    for _ in 0..16 {
        dvp.get_image(); // warm-up frames (discard)
    }

    loop {
        dvp.get_image();

        puts(b"IMGSTART ");
        put_dec(W as u32);
        putc(b' ');
        put_dec(H as u32);
        putc(b'\n');
        for i in 0..(W * H / 2) {
            let word = unsafe { core::ptr::read_volatile(buf.add(i)) };
            putc((word & 0xff) as u8);
            putc((word >> 8) as u8);
            putc((word >> 16) as u8);
            putc((word >> 24) as u8);
        }
        puts(b"IMGEND\n");
        delay(40_000_000);
    }
}
