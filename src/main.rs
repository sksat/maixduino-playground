//! K210 + onboard ESP32 (nina-fw) over hardware SPI0: connect to a WiFi AP and
//! print the assigned IP (WiFi step 4).
//!
//! Credentials come from `wifi_creds.env` (gitignored) via `build.rs` -> `env!`.
//! They are passed to SET_PASSPHRASE but NEVER printed over serial; the output is
//! only the connection status and the assigned IP address.
//!
//! Flow: SET_PASSPHRASE(ssid, pass) -> poll GET_CONN_STATUS until WL_CONNECTED ->
//! GET_IPADDR. SPI + nina protocol live in `nina.rs`.

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

// From wifi_creds.env via build.rs (never printed).
const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASS: &str = env!("WIFI_PASS");

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

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sysctl = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sysctl.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _en = fpioa.io8.into_function(fpioa::GPIOHS0);
    let _cs = fpioa.io25.into_function(fpioa::GPIOHS1);
    let _rdy = fpioa.io9.into_function(fpioa::GPIOHS2);
    let _sclk = fpioa.io27.into_function(fpioa::SPI0_SCLK);
    let _mosi = fpioa.io28.into_function(fpioa::SPI0_D0);
    let _miso = fpioa.io26.into_function(fpioa::SPI0_D1);

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(BAUD.bps(), &clocks);

    nina::init();

    let mut buf = [0u8; 64];
    let mut lens = [0usize; 8];

    puts(b"\nESP32 WiFi connect (nina-fw over hardware SPI0)\n");
    // Confirm creds loaded without revealing them: just their lengths.
    puts(b"creds: ssid_len=");
    put_dec(WIFI_SSID.len() as u32);
    puts(b" pass_len=");
    put_dec(WIFI_PASS.len() as u32);
    putc(b'\n');

    // Kick off the association (ssid + passphrase are sent, never printed).
    nina::request(
        nina::CMD_SET_PASSPHRASE,
        &[WIFI_SSID.as_bytes(), WIFI_PASS.as_bytes()],
        &mut buf,
        &mut lens,
    );

    // Poll the connection status (wl_status_t) until connected, ~15s budget.
    // 0=idle 1=no-ssid 2=scan-done 3=CONNECTED 4=conn-failed 5=lost 6=disconnected.
    puts(b"connecting (status: ");
    let mut status = 0u8;
    let mut tries = 0;
    while tries < 15 {
        nina::sleep_ms(1000);
        let n = nina::request(nina::CMD_GET_CONN_STATUS, &[], &mut buf, &mut lens);
        status = if n >= 1 { buf[0] } else { 0xff };
        put_dec(status as u32);
        putc(b' ');
        if status == nina::WL_CONNECTED {
            break;
        }
        tries += 1;
    }
    puts(b")\n");

    puts(b"status=");
    put_dec(status as u32);
    if status == nina::WL_CONNECTED {
        puts(b" (CONNECTED)\n");
        // Fetch the assigned IP: GET_IPADDR with a dummy param; reply[0] is the IP.
        let n = nina::request(nina::CMD_GET_IPADDR, &[&[0xff]], &mut buf, &mut lens);
        if n >= 1 && lens[0] >= 4 {
            puts(b"IP: ");
            for k in 0..4 {
                put_dec(buf[k] as u32);
                if k < 3 {
                    putc(b'.');
                }
            }
            putc(b'\n');
        }
        puts(b"PASS: associated to AP and got an IP\n");
    } else {
        puts(b" (not connected - check wifi_creds.env)\n");
    }

    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
