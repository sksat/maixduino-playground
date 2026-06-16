//! K210 + onboard ESP32 (nina-fw) over hardware SPI0: a tiny HTTP server (WiFi
//! step 6). Connect to WiFi, listen on port 80, and serve a static page. A first
//! step toward serving the camera stream.
//!
//! Credentials come from `wifi_creds.env` (gitignored) via `build.rs` -> `env!`.

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
    let mut b = [0u8; 10];
    let mut i = 0;
    while v > 0 {
        b[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        putc(b[i]);
    }
}

const BODY: &[u8] = b"<!doctype html><html><body><h1>Hello from K210 bare-metal Rust!</h1><p>Served by the onboard ESP32 (nina-fw) over hardware SPI0.</p></body></html>\n";

/// Write `v` as decimal into `out`, returning the number of bytes written.
fn write_dec(out: &mut [u8], mut v: u32) -> usize {
    if v == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut i = 0;
    while v > 0 {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    for k in 0..i {
        out[k] = tmp[i - 1 - k];
    }
    i
}

fn append(out: &mut [u8], at: usize, s: &[u8]) -> usize {
    for (k, &b) in s.iter().enumerate() {
        out[at + k] = b;
    }
    s.len()
}

/// Build an HTTP 200 response (with Content-Length) for `body` into `out`.
fn build_response(out: &mut [u8], body: &[u8]) -> usize {
    let mut n = 0;
    n += append(out, n, b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: ");
    n += write_dec(&mut out[n..], body.len() as u32);
    n += append(out, n, b"\r\nConnection: close\r\n\r\n");
    n += append(out, n, body);
    n
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

    let mut buf = [0u8; 700];
    let mut lens = [0usize; 8];

    puts(b"\nESP32 HTTP server (nina-fw over hardware SPI0)\n");

    // 1. associate
    nina::request(
        nina::CMD_SET_PASSPHRASE,
        &[WIFI_SSID.as_bytes(), WIFI_PASS.as_bytes()],
        &mut buf,
        &mut lens,
    );
    puts(b"connecting");
    let mut t = 0;
    let mut status = 0u8;
    while t < 15 {
        nina::sleep_ms(1000);
        putc(b'.');
        let n = nina::request(nina::CMD_GET_CONN_STATUS, &[], &mut buf, &mut lens);
        status = if n >= 1 { buf[0] } else { 0xff };
        if status == nina::WL_CONNECTED {
            break;
        }
        t += 1;
    }
    putc(b'\n');
    if status != nina::WL_CONNECTED {
        puts(b"wifi failed\n");
        loop {
            unsafe { core::arch::asm!("wfi") };
        }
    }
    nina::request(nina::CMD_GET_IPADDR, &[&[0xff]], &mut buf, &mut lens);
    let ip = [buf[0], buf[1], buf[2], buf[3]];

    puts(b"server up: http://");
    put_dec(ip[0] as u32);
    putc(b'.');
    put_dec(ip[1] as u32);
    putc(b'.');
    put_dec(ip[2] as u32);
    putc(b'.');
    put_dec(ip[3] as u32);
    puts(b"/\n");

    // 2. serve loop: a fresh listening socket per request. availServer =
    //    AVAIL_DATA_TCP with an accept flag -> the new client socket (255 = none);
    //    read/reply on that client socket, then close both and re-listen.
    let port_be = [0u8, 80u8];
    let mode = [0u8];
    let accept = [1u8];
    loop {
        nina::request(nina::CMD_GET_SOCKET, &[], &mut buf, &mut lens);
        let listen = buf[0];
        let lp = [listen];
        nina::request(nina::CMD_START_SERVER_TCP, &[&port_be, &lp, &mode], &mut buf, &mut lens);

        // accept a client (poll ~15s)
        let mut client = 255u8;
        let mut w = 0;
        while w < 150 {
            let n = nina::request(nina::CMD_AVAIL_DATA_TCP, &[&lp, &accept], &mut buf, &mut lens);
            let c = if n >= 1 { buf[0] } else { 255 };
            if c != 255 && c != listen {
                client = c;
                break;
            }
            nina::sleep_ms(100);
            w += 1;
        }

        if client != 255 {
            let cp = [client];
            nina::sleep_ms(30);
            let a = nina::request(nina::CMD_AVAIL_DATA_TCP, &[&cp], &mut buf, &mut lens);
            let have = if a >= 1 && lens[0] >= 2 {
                (buf[0] as u16) | ((buf[1] as u16) << 8)
            } else {
                0
            };
            if have > 0 {
                let want = if have > 512 { 512 } else { have };
                let wl = [(want & 0xff) as u8, (want >> 8) as u8];
                nina::request_wide(nina::CMD_GET_DATABUF_TCP, &[&cp, &wl], &mut buf, &mut lens);
            }
            let mut resp = [0u8; 512];
            let rn = build_response(&mut resp, BODY);
            nina::request_wide(nina::CMD_SEND_DATA_TCP, &[&cp, &resp[..rn]], &mut buf, &mut lens);
            nina::sleep_ms(50);
            nina::request(nina::CMD_STOP_CLIENT_TCP, &[&cp], &mut buf, &mut lens);
            puts(b"served sock ");
            put_dec(client as u32);
            puts(b" req ");
            put_dec(have as u32);
            puts(b"B\n");
        }
        nina::request(nina::CMD_STOP_CLIENT_TCP, &[&lp], &mut buf, &mut lens);
        nina::sleep_ms(30);
    }
}
