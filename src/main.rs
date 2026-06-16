//! K210 + onboard ESP32 (nina-fw) over the hardware SPI0: scan for nearby WiFi
//! networks (WiFi step 3). No credentials / no association — exercises the nina
//! command path with multi-param replies, now reliable over hardware SPI.
//!
//! GET_FW_VERSION (sanity) -> START_SCAN_NETWORKS -> poll SCAN_NETWORKS until it
//! returns networks -> print each SSID and (via GET_IDX_RSSI) its RSSI. The SPI +
//! nina protocol live in `nina.rs`.

#![no_std]
#![no_main]

mod nina;

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const BAUD: u32 = 115_200;

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
fn put_int(v: i32) {
    if v < 0 {
        putc(b'-');
        put_dec((-v) as u32);
    } else {
        put_dec(v as u32);
    }
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    // nina handshake/CS/EN on GPIO; data on hardware SPI0.
    let _en = fpioa.io8.into_function(fpioa::GPIOHS0);
    let _cs = fpioa.io25.into_function(fpioa::GPIOHS1);
    let _rdy = fpioa.io9.into_function(fpioa::GPIOHS2);
    let _sclk = fpioa.io27.into_function(fpioa::SPI0_SCLK);
    let _mosi = fpioa.io28.into_function(fpioa::SPI0_D0);
    let _miso = fpioa.io26.into_function(fpioa::SPI0_D1);

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(BAUD.bps(), &clocks);

    nina::init();

    puts(b"\nESP32 WiFi scan (nina-fw over hardware SPI0)\n");

    let mut buf = [0u8; 768];
    let mut lens = [0usize; 32];

    // Sanity: firmware version.
    nina::request(nina::CMD_GET_FW_VERSION, &[], &mut buf, &mut lens);
    puts(b"fw version: ");
    for &c in buf.iter().take(lens[0]) {
        if (0x20..0x7f).contains(&c) {
            putc(c);
        }
    }
    putc(b'\n');

    // Trigger a scan, then poll until the network list is ready (~up to 10s).
    nina::request(nina::CMD_START_SCAN_NETWORKS, &[], &mut buf, &mut lens);
    puts(b"scanning...\n");

    let mut count = 0;
    let mut attempt = 0;
    while attempt < 10 {
        nina::sleep_ms(1000);
        count = nina::request(nina::CMD_SCAN_NETWORKS, &[], &mut buf, &mut lens);
        if count > 0 {
            break;
        }
        attempt += 1;
    }

    puts(b"found ");
    put_dec(count as u32);
    puts(b" networks:\n");

    // Walk the concatenated SSIDs; fetch each network's RSSI by index.
    let mut off = 0;
    for i in 0..count {
        let l = lens[i];
        let ssid_start = off;
        off += l;

        let mut rbuf = [0u8; 16];
        let mut rlens = [0usize; 4];
        let rn = nina::request(nina::CMD_GET_IDX_RSSI, &[&[i as u8]], &mut rbuf, &mut rlens);
        let rssi = if rn >= 1 && rlens[0] >= 4 {
            i32::from_le_bytes([rbuf[0], rbuf[1], rbuf[2], rbuf[3]])
        } else {
            0
        };

        puts(b"  [");
        put_dec(i as u32);
        puts(b"] ");
        for &c in buf[ssid_start..ssid_start + l].iter() {
            putc(if (0x20..0x7f).contains(&c) { c } else { b'.' });
        }
        puts(b"  (");
        put_int(rssi);
        puts(b" dBm)\n");
    }

    puts(b"[scan done]\n");

    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
