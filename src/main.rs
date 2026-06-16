//! TEMP bring-up test: talk to the ESP32 "WiFi modem" over UART1 (IO6/IO7) and ping
//! it. Verifies the new UART WiFi path (esp32-modem fw) before rewiring the camera
//! web server onto it. The nina-SPI camera web server is at tag
//! `nina-spi-camera-webserver`.

#![no_std]
#![no_main]

mod uart_wifi;

use panic_halt as _;

use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const BAUD: u32 = 115_200;
const LINK_BAUD: u32 = 921_600;
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
fn delay(n: u32) {
    for _ in 0..n {
        unsafe { core::arch::asm!("nop") };
    }
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sc = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sc.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _en = fpioa.io8.into_function(fpioa::GPIOHS0); // ESP32 EN
    let _u1rx = fpioa.io6.into_function(fpioa::UART1_RX); // <- ESP32 U0TXD
    let _u1tx = fpioa.io7.into_function(fpioa::UART1_TX); // -> ESP32 U0RXD

    let clocks = k210_hal::clock::Clocks::new();
    let _serial = p.UARTHS.configure(BAUD.bps(), &clocks);

    for _ in 0..20 {
        putc(b'.');
        delay(15_000_000);
    }
    puts(b"\nK210 UART WiFi modem test\n");

    uart_wifi::init(LINK_BAUD);
    puts(b"bringing up modem...\n");
    let up = uart_wifi::bringup(); // EN pulse(s) until the ready marker appears
    puts(if up {
        b"modem READY (marker seen)\n" as &[u8]
    } else {
        b"modem marker NOT seen\n"
    });
    puts(b"pinging modem...\n");

    let mut reply = [0u8; 16];
    let mut ok = 0u32;
    for i in 0..6u32 {
        puts(b"ping ");
        put_dec(i);
        puts(b" -> ");
        match uart_wifi::cmd(uart_wifi::CMD_PING, &[], &mut reply, 1000) {
            Some((tag, n)) => {
                puts(b"reply tag='");
                putc(tag);
                puts(b"' len=");
                put_dec(n as u32);
                putc(b'\n');
                if tag == b'O' {
                    ok += 1;
                }
            }
            None => puts(b"timeout\n"),
        }
        uart_wifi::sleep_ms(300);
    }
    puts(b"ping ok=");
    put_dec(ok);
    puts(b"/6\n");

    // connect to WiFi over the modem: payload = ssid '\0' pass
    let ssid = WIFI_SSID.as_bytes();
    let pass = WIFI_PASS.as_bytes();
    puts(b"connecting WiFi (ssid_len=");
    put_dec(ssid.len() as u32);
    puts(b" pass_len=");
    put_dec(pass.len() as u32);
    puts(b")...\n");
    let mut cbuf = [0u8; 160];
    let mut n = 0;
    for &b in ssid {
        cbuf[n] = b;
        n += 1;
    }
    cbuf[n] = 0;
    n += 1;
    for &b in pass {
        cbuf[n] = b;
        n += 1;
    }
    match uart_wifi::cmd(uart_wifi::CMD_CONNECT, &cbuf[..n], &mut reply, 35000) {
        Some((b'I', 4)) => {
            puts(b"IP ");
            put_dec(reply[0] as u32);
            putc(b'.');
            put_dec(reply[1] as u32);
            putc(b'.');
            put_dec(reply[2] as u32);
            putc(b'.');
            put_dec(reply[3] as u32);
            putc(b'\n');
        }
        Some((b'E', m)) if m >= 3 => {
            let mut cs = 0u16;
            for &b in &cbuf[..n] {
                cs = cs.wrapping_add(b as u16);
            }
            let esp_cs = (reply[1] as u16) | ((reply[2] as u16) << 8);
            puts(b"connect failed wl=");
            put_dec(reply[0] as u32);
            puts(b" csum k210=");
            put_dec(cs as u32);
            puts(b" esp=");
            put_dec(esp_cs as u32);
            if m >= 5 {
                puts(b" esp_ssidlen=");
                put_dec(reply[3] as u32);
                puts(b" esp_passlen=");
                put_dec(reply[4] as u32);
            }
            if m >= 6 {
                puts(b" disc_reason=");
                put_dec(reply[5] as u32);
            }
            if m >= 9 {
                puts(b" ap_seen=");
                put_dec(reply[6] as u32);
                puts(b" ch=");
                put_dec(reply[7] as u32);
                puts(b" rssi=-");
                put_dec((256 - reply[8] as u32) & 0xff); // rssi is negative dBm
            }
            if m >= 10 {
                puts(b" enc=");
                put_dec(reply[9] as u32); // 3=WPA2 6=WPA3 7=WPA2/WPA3-mixed
            }
            if m >= 11 {
                puts(b" assoc=");
                put_dec(reply[10] as u32); // 1 = association succeeded (then 4-way/pass)
            }
            puts(if cs == esp_cs { b" [PAYLOAD OK]\n" as &[u8] } else { b" [PAYLOAD CORRUPT]\n" });
        }
        Some((b'E', _)) => {
            puts(b"connect failed, wl_status=");
            put_dec(reply[0] as u32);
            putc(b'\n');
        }
        Some((tag, _)) => {
            puts(b"connect failed tag='");
            putc(tag);
            puts(b"'\n");
        }
        None => puts(b"connect timeout\n"),
    }
    puts(b"test done\n");

    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
