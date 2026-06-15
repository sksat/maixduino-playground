//! K210 hardware FFT accelerator demo, driven by the DMA.
//!
//! The K210 FFT has no MMIO data path: it exchanges samples with the DMA via
//! TX/RX request handshakes only (MMIO writes to its FIFOs are silently dropped,
//! confirmed on hardware). So we feed/drain it with two DMA channels running
//! concurrently -- send (RAM -> input FIFO) and receive (output FIFO -> RAM) --
//! exactly as the Kendryte SDK's `fft_complex_uint16_dma` does.
//!
//! Test: a 64-point forward FFT of a pure real tone x[n] = 10000*cos(2*pi*8*n/64)
//! must peak at bin 8 (and its mirror, bin 56). Input is an exact integer number
//! of cycles, so there is no leakage and the peak is unambiguous.

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

// FFT peripheral FIFOs (base 0x4200_0000).
const FFT_INPUT_FIFO: u64 = 0x4200_0000;
const FFT_OUTPUT_FIFO: u64 = 0x4200_0038;
// sysctl DMA request lines (from k210-pac DMA_SEL0_A).
const FFT_RX_REQ: u8 = 23;
const FFT_TX_REQ: u8 = 24;
// DMA channels.
const TX_CH: usize = 0;
const RX_CH: usize = 1;

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
fn isqrt(v: u64) -> u32 {
    let mut op = v;
    let mut res: u64 = 0;
    let mut one: u64 = 1u64 << 62;
    while one > op {
        one >>= 2;
    }
    while one != 0 {
        if op >= res + one {
            op -= res + one;
            res = (res >> 1) + one;
        } else {
            res >>= 1;
        }
        one >>= 2;
    }
    res as u32
}
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}

/// Uncached alias of a cached SRAM pointer (0x8000_0000 -> 0x4000_0000), so the
/// DMA and CPU agree on the buffer contents.
fn uncached(p: *const u64) -> *mut u64 {
    (p as usize - UNCACHED_OFFSET) as *mut u64
}

const N: usize = 64;
const NW: usize = N / 2; // 64-bit words: two complex samples each

/// 64-point forward FFT via dual-DMA. `in_u`/`out_u` are uncached-alias pointers
/// to NW-word buffers; `in_u` must already hold the packed input.
fn fft64_dma(dma: &Dmac, in_u: *mut u64, out_u: *mut u64) {
    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc).clk_en_peri.modify(|_, w| w.fft_clk_en().set_bit());
        (*sc).peri_reset.modify(|_, w| w.fft_reset().set_bit());
        (*sc).peri_reset.modify(|_, w| w.fft_reset().clear_bit());
    }

    let fft = unsafe { &*pac::FFT::ptr() };
    fft.ctrl.write(|w| unsafe {
        w.point()
            .p64()
            .mode()
            .fft()
            .shift()
            .bits(0x3f)
            .input_mode()
            .riri()
            .data_mode()
            .width_64()
            .dma_send()
            .set_bit()
            .enable()
            .set_bit()
    });

    dma.set_select_request(RX_CH, FFT_RX_REQ);
    dma.set_select_request(TX_CH, FFT_TX_REQ);

    // Receive: output FIFO (fixed addr) -> RAM (incrementing). Arm before send.
    dma.start_single(
        RX_CH,
        FFT_OUTPUT_FIFO,
        out_u as u64,
        AddressInc::NOCHANGE,
        AddressInc::INCREMENT,
        BurstLen::LENGTH_4,
        TransWidth::WIDTH_64,
        NW as u32,
    );
    // Send: RAM (incrementing) -> input FIFO (fixed addr).
    dma.start_single(
        TX_CH,
        in_u as u64,
        FFT_INPUT_FIFO,
        AddressInc::INCREMENT,
        AddressInc::NOCHANGE,
        BurstLen::LENGTH_4,
        TransWidth::WIDTH_64,
        NW as u32,
    );

    dma.wait_done(TX_CH);
    dma.wait_done(RX_CH);
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(115_200.bps(), &clocks);

    let dma = Dmac::new(p.DMAC);

    // x[n] = 10000 * cos(2*pi*8*n/64) = 10000 * cos(pi*n/4); period 8, exact bins.
    const TONE: [i16; 8] = [10000, 7071, 0, -7071, -10000, -7071, 0, 7071];

    let in_buf = [0u64; NW];
    let out_buf = [0u64; NW];
    let in_u = uncached(in_buf.as_ptr());
    let out_u = uncached(out_buf.as_ptr());

    // Pack two real samples (imag = 0) per 64-bit word, RIRI: [re0|im0|re1|im1].
    for wi in 0..NW {
        let s0 = TONE[(2 * wi) % 8] as u16 as u64;
        let s1 = TONE[(2 * wi + 1) % 8] as u16 as u64;
        unsafe { core::ptr::write_volatile(in_u.add(wi), s0 | (s1 << 32)) };
    }

    fft64_dma(&dma, in_u, out_u);

    // Unpack two complex bins per word and compute |X[k]|^2.
    let mut mag2 = [0i64; N];
    for wi in 0..NW {
        let word = unsafe { core::ptr::read_volatile(out_u.add(wi)) };
        let re0 = (word & 0xffff) as u16 as i16 as i64;
        let im0 = ((word >> 16) & 0xffff) as u16 as i16 as i64;
        let re1 = ((word >> 32) & 0xffff) as u16 as i16 as i64;
        let im1 = ((word >> 48) & 0xffff) as u16 as i16 as i64;
        mag2[2 * wi] = re0 * re0 + im0 * im0;
        mag2[2 * wi + 1] = re1 * re1 + im1 * im1;
    }

    let mut peak = 1usize;
    let mut best = -1i64;
    for k in 1..N {
        if mag2[k] > best {
            best = mag2[k];
            peak = k;
        }
    }
    let pass = peak == 8;

    puts(b"\r\n-- K210 hardware FFT via DMA (64-pt, real tone @ bin 8) --\r\n");
    puts(b"magnitude spectrum bins 0..16:\r\n");
    for k in 0..16usize {
        puts(b"  bin ");
        put_dec(k as u32);
        puts(b" = ");
        put_dec(isqrt(mag2[k] as u64));
        puts(b"\r\n");
    }

    loop {
        // peak at bin 8, its mirror at bin 56, neighbours ~0 -> a clean line.
        puts(b"FFT/DMA 64pt tone@8 -> peak bin=");
        put_dec(peak as u32);
        puts(b" mag=");
        put_dec(isqrt(best as u64));
        puts(b" (mirror56=");
        put_dec(isqrt(mag2[56] as u64));
        puts(b" bin7=");
        put_dec(isqrt(mag2[7] as u64));
        putc(b')');
        puts(if pass { b" PASS\r\n" } else { b" FAIL\r\n" });
        delay(20_000_000);
    }
}
