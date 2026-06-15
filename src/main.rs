//! K210 hardware SHA256 accelerator demo. k210-hal's `sha256` module is a
//! `todo!()` stub, so this drives the peripheral directly via the PAC: set the
//! padded-block count, feed the software-padded message as little-endian u32
//! words into `data_in` (polling `fifo_in_full`), then read the 8 result words.
//!
//! Two K210 quirks found the hard way:
//!   - the result comes out word-reversed AND byte-swapped, so
//!     `digest_word[i] = result[7-i]` read little-endian (`to_le_bytes`);
//!   - `function_reg_0.en` doesn't reliably clear on done, so we just give the
//!     block a moment and read (the hash is ready right after feeding).
//!
//! Unambiguous: SHA256("abc") is computed in hardware and checked against the
//! known vector ba7816bf...20015ad -> PASS/FAIL over serial.

#![no_std]
#![no_main]

use panic_halt as _;

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
fn put_hex_byte(b: u8) {
    let h = b"0123456789abcdef";
    putc(h[(b >> 4) as usize]);
    putc(h[(b & 0xf) as usize]);
}
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}

/// SHA256 of `input` via the K210 hardware accelerator (output = 32 bytes).
fn sha256_hard(input: &[u8], out: &mut [u8; 32]) {
    let sha = pac::SHA256::ptr();
    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc).clk_en_peri.modify(|_, w| w.sha_clk_en().set_bit());
        (*sc).peri_reset.modify(|_, w| w.sha_reset().set_bit());
        (*sc).peri_reset.modify(|_, w| w.sha_reset().clear_bit());

        let blocks = ((input.len() + 64 + 8) / 64) as u16; // 512-bit blocks incl. padding
        let total = blocks as usize * 64;

        (*sha).num_reg.write(|w| w.data_cnt().bits(blocks));
        (*sha).function_reg_1.modify(|_, w| w.dma_en().clear_bit());
        (*sha)
            .function_reg_0
            .modify(|_, w| w.endian().set_bit().en().set_bit());

        // SHA padding done in software: msg + 0x80 + zeros + 64-bit big-endian length.
        let lenb = ((input.len() as u64) * 8).to_be_bytes();
        let byte_at = |idx: usize| -> u8 {
            if idx < input.len() {
                input[idx]
            } else if idx == input.len() {
                0x80
            } else if idx >= total - 8 {
                lenb[idx - (total - 8)]
            } else {
                0
            }
        };
        for i in 0..(total / 4) {
            let word = u32::from_le_bytes([
                byte_at(i * 4),
                byte_at(i * 4 + 1),
                byte_at(i * 4 + 2),
                byte_at(i * 4 + 3),
            ]);
            while (*sha).function_reg_1.read().fifo_in_full().bit_is_set() {}
            (*sha).data_in.write(|w| w.bits(word));
        }

        delay(100_000); // let the last block finish
        for i in 0..8 {
            // word-reversed + byte-swapped: out word i = result[7-i], little-endian
            out[i * 4..i * 4 + 4].copy_from_slice(&(*sha).result[7 - i].read().bits().to_le_bytes());
        }
    }
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(115_200.bps(), &clocks);

    let want: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];
    let mut digest = [0u8; 32];
    sha256_hard(b"abc", &mut digest);
    let pass = digest == want;

    puts(b"\r\n-- K210 hardware SHA256 --\r\n");
    puts(b"SHA256(\"abc\") = ");
    for &b in digest.iter() {
        put_hex_byte(b);
    }
    puts(b"\r\nexpected      = ");
    for &b in want.iter() {
        put_hex_byte(b);
    }
    puts(b"\r\n");

    loop {
        puts(b"SHA256(abc)=");
        for &b in digest.iter() {
            put_hex_byte(b);
        }
        puts(if pass { b" PASS\r\n" } else { b" FAIL\r\n" });
        delay(20_000_000);
    }
}
