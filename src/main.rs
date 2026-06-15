//! K210 hardware AES-128 ECB accelerator demo. HAL `aes` is a `todo!()` stub, so
//! we drive the PAC directly: load the key, mode, then feed plaintext and read
//! ciphertext through the data FIFOs.
//!
//! K210 quirks found by diffing against the FIPS-197 vector:
//!   - `endian=1` must be written BEFORE the key (order-dependent; otherwise the
//!     key is interpreted wrong and you get a valid-but-wrong ciphertext);
//!   - key words go in reversed order, all words little-endian; ciphertext words
//!     come out little-endian.
//!
//! FIPS-197: key=000102..0f, pt=00112233..ff, ct=69c4e0d86a7b0430d8cdb78070b4c55a

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

/// AES-128 ECB encrypt one 16-byte block via the K210 hardware accelerator.
fn aes128_ecb_encrypt(key: &[u8; 16], pt: &[u8; 16], ct: &mut [u8; 16]) {
    let base = pac::AES::ptr() as usize;
    let w = |off: usize, val: u32| unsafe { core::ptr::write_volatile((base + off) as *mut u32, val) };
    let r = |off: usize| -> u32 { unsafe { core::ptr::read_volatile((base + off) as *const u32) } };
    let le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);

    unsafe {
        let sc = pac::SYSCTL::ptr();
        (*sc).clk_en_peri.modify(|_, w| w.aes_clk_en().set_bit());
        (*sc).peri_reset.modify(|_, w| w.aes_reset().set_bit());
        (*sc).peri_reset.modify(|_, w| w.aes_reset().clear_bit());
    }

    w(0x28, 1); // endian = 1 -- MUST be before the key
    for i in 0..4 {
        w((3 - i) * 4, le(&key[i * 4..])); // key @0x00, reversed word order
    }
    w(0x10, 0); // encrypt_sel = encrypt
    w(0x14, 0); // mode_ctl = ECB, 128-bit key
    w(0x30, 0); // dma_sel = off
    w(0x34, 0); // aad_num
    w(0x3c, 15); // pc_num = block_len - 1
    w(0x64, 1); // enable

    for i in 0..4 {
        while r(0x4c) & 1 == 0 {} // data_in_flag
        w(0x40, le(&pt[i * 4..])); // text_data
    }
    for i in 0..4 {
        while r(0x68) & 1 == 0 {} // data_out_flag
        ct[i * 4..i * 4 + 4].copy_from_slice(&r(0x60).to_le_bytes()); // out_data
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

    let key = [
        0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    let pt = [
        0x00u8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let want = [
        0x69u8, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5,
        0x5a,
    ];
    let mut ct = [0u8; 16];
    aes128_ecb_encrypt(&key, &pt, &mut ct);
    let pass = ct == want;

    puts(b"\r\n-- K210 hardware AES-128 ECB --\r\n");
    puts(b"ct       = ");
    for &b in ct.iter() {
        put_hex_byte(b);
    }
    puts(b"\r\nexpected = ");
    for &b in want.iter() {
        put_hex_byte(b);
    }
    puts(b"\r\n");

    loop {
        puts(b"AES-128 ECB ");
        for &b in ct.iter() {
            put_hex_byte(b);
        }
        puts(if pass { b" PASS\r\n" } else { b" FAIL\r\n" });
        delay(20_000_000);
    }
}
