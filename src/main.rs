//! K210 DVP camera: stream OV2640 RGB565 frames over serial, with the resolution
//! selectable at runtime from the host (no reflash).
//!
//! Host -> device: a single ASCII byte picks the resolution and takes effect on
//! the next frame:
//!   '1' = QQVGA 160x120   '2' = QVGA 320x240   '3' = VGA 640x480
//! Device -> host, repeated every frame:
//!   "IMGSTART <w> <h>\n"  then  w*h*2 raw little-endian RGB565 bytes (buffer order)
//! The host syncs on the header (which carries the current w/h) and reads exactly
//! w*h*2 bytes, so it adapts automatically when the resolution changes.

#![no_std]
#![no_main]

mod dvp;

use panic_halt as _;

use dvp::{ov2640_init, ov2640_read_id, ov2640_rgb565_qqvga, ov2640_rgb565_vga, Dvp, ImageFormat};
use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const UARTHS_RXDATA: *const u32 = 0x3800_0004 as *const u32;
const UNCACHED_OFFSET: usize = 0x4000_0000;

const BAUD: u32 = 1_500_000;

// Frame buffer sized for the largest supported mode (VGA 640x480 RGB565 = 600 KB);
// smaller resolutions use a prefix of it.
const WMAX: usize = 640;
const HMAX: usize = 480;

#[repr(C, align(64))]
struct Frame {
    px: [u32; WMAX * HMAX / 2],
}
static mut FRAME: Frame = Frame { px: [0; WMAX * HMAX / 2] };

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
/// Non-blocking UARTHS read: `None` if the RX FIFO is empty (bit31 = empty flag).
fn getc_try() -> Option<u8> {
    let v = unsafe { core::ptr::read_volatile(UARTHS_RXDATA) };
    if v & 0x8000_0000 != 0 {
        None
    } else {
        Some((v & 0xff) as u8)
    }
}
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}

/// Apply the OV2640 + DVP config for `mode` ('1'/'2'/'3') and return (w, h).
/// Re-runs the baseline RGB565 init each time so switching between sizes is
/// order-independent, then layers the size-specific scaler delta on top.
fn configure_res(dvp: &Dvp, mode: u8) -> (usize, usize) {
    ov2640_init(dvp);
    let (w, h) = match mode {
        b'2' => (320, 240),                              // QVGA: baseline, no delta
        b'3' => {
            ov2640_rgb565_vga(dvp);
            (640, 480)
        }
        _ => {
            ov2640_rgb565_qqvga(dvp);
            (160, 120)
        } // '1' (default): QQVGA
    };
    dvp.set_image_format(ImageFormat::RGB);
    dvp.set_image_size(false, w as u16, h as u16);
    for _ in 0..8 {
        dvp.get_image(); // warm-up frames at the new size (discard)
    }
    (w, h)
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _rx = fpioa.io4.into_function(fpioa::UARTHS_RX);
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

    // The DVP's AXI master writes to the cached SRAM address (0x8000_0000 range);
    // the CPU reads back through the uncached alias (0x4000_0000) so it sees the
    // DVP's writes instead of stale cache.
    let cached = unsafe { core::ptr::addr_of!(FRAME.px) } as *const u32 as usize;
    let buf = (cached - UNCACHED_OFFSET) as *const u32;
    dvp.set_ai_addr(None);
    dvp.set_display_addr(Some(cached as u32));
    dvp.set_auto(false);

    delay(60_000_000); // brief settle; warm-up frames in configure_res do the rest
    let (mut w, mut h) = configure_res(&dvp, b'1'); // default: QQVGA stream

    loop {
        // Drain any pending host bytes; the last valid command wins.
        let mut cmd = None;
        while let Some(c) = getc_try() {
            if matches!(c, b'1' | b'2' | b'3') {
                cmd = Some(c);
            }
        }
        if let Some(c) = cmd {
            let (nw, nh) = configure_res(&dvp, c);
            w = nw;
            h = nh;
        }

        dvp.get_image();

        puts(b"IMGSTART ");
        put_dec(w as u32);
        putc(b' ');
        put_dec(h as u32);
        putc(b'\n');
        for i in 0..(w * h / 2) {
            let word = unsafe { core::ptr::read_volatile(buf.add(i)) };
            putc((word & 0xff) as u8);
            putc((word >> 8) as u8);
            putc((word >> 16) as u8);
            putc((word >> 24) as u8);
        }
        puts(b"IMGEND\n");
        // Minimal inter-frame gap so the host can resync; small because a QQVGA
        // frame is only ~38 KB (~0.3s at 1.5 Mbaud) and debug-build nop loops are
        // expensive (a big loop here would dominate the frame time).
        delay(200_000);
    }
}
