//! K210 DMAC memory-to-memory demo. The Synopsys DesignWare AXI DMA has no HAL,
//! so we drive it via the PAC (see `dmac.rs`, adapted from laanwj/k210-sdk-stuff).
//! This is the groundwork for the FFT demo: the K210 FFT accelerator has no MMIO
//! data path and can only be fed/read by the DMA.
//!
//! Coherency note: K210 SRAM is cached at 0x8000_0000 and uncached at
//! 0x4000_0000. The DMA sees physical memory, so the buffers are filled, copied,
//! and verified entirely through the uncached alias to avoid stale cache lines.

#![no_std]
#![no_main]

mod dmac;

use panic_halt as _;

use dmac::{AddressInc, BurstLen, Dmac, TransWidth, UNCACHED_OFFSET};
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
fn put_hex_u32(v: u32) {
    let h = b"0123456789abcdef";
    for s in (0..8).rev() {
        putc(h[((v >> (s * 4)) & 0xf) as usize]);
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

/// Uncached alias of a cached SRAM pointer (0x8000_0000 -> 0x4000_0000).
fn uncached(p: *const u32) -> *mut u32 {
    (p as usize - UNCACHED_OFFSET) as *mut u32
}

const N: usize = 64;

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(115_200.bps(), &clocks);

    let dma = Dmac::new(p.DMAC);

    let src = [0u32; N];
    let dst = [0u32; N];
    let src_u = uncached(src.as_ptr());
    let dst_u = uncached(dst.as_ptr());

    puts(b"\r\n-- K210 DMAC memory-to-memory --\r\n");
    puts(b"dmac id=");
    put_hex_u32((dma.id() >> 32) as u32);
    put_hex_u32(dma.id() as u32);
    puts(b"\r\n");

    loop {
        // Fill src with a known pattern and clear dst, both via the uncached view.
        for i in 0..N {
            unsafe {
                core::ptr::write_volatile(src_u.add(i), 0xc0de_0000 + i as u32);
                core::ptr::write_volatile(dst_u.add(i), 0);
            }
        }

        dma.start_single(
            0,
            src_u as u64,
            dst_u as u64,
            AddressInc::INCREMENT,
            AddressInc::INCREMENT,
            BurstLen::LENGTH_1,
            TransWidth::WIDTH_32,
            N as u32,
        );
        dma.wait_done(0);

        // Verify dst == src.
        let mut ok = true;
        let mut first_bad = 0usize;
        for i in 0..N {
            let got = unsafe { core::ptr::read_volatile(dst_u.add(i)) };
            if got != 0xc0de_0000 + i as u32 {
                ok = false;
                first_bad = i;
                break;
            }
        }

        let last = unsafe { core::ptr::read_volatile(dst_u.add(N - 1)) };
        puts(b"copied ");
        put_dec(N as u32);
        puts(b" words, dst[0]=");
        put_hex_u32(unsafe { core::ptr::read_volatile(dst_u) });
        puts(b" dst[63]=");
        put_hex_u32(last);
        if ok {
            puts(b" PASS\r\n");
        } else {
            puts(b" FAIL @");
            put_dec(first_bad as u32);
            puts(b"\r\n");
        }
        delay(20_000_000);
    }
}
