//! K210 camera web server over the UART WiFi path (the goal, faster).
//!
//! The onboard ESP32 runs the UART-modem nina-fw (esp32-modem-ninafw/): WiFi driven
//! over UART1 (IO6/IO7), independent of the camera's SPI0/DVP pads. So unlike the SPI
//! version (tag `nina-spi-camera-webserver`), a DVP capture no longer wedges the
//! network -- there is NO ~5 s EN-reset/reconnect/re-listen dance per frame. Each
//! request captures a FRESH frame and serves it live on a healthy connection.
//!
//! Credentials come from `wifi_creds.env` (gitignored) via `build.rs` -> `env!`.

#![no_std]
#![no_main]

mod dvp;
mod uart_wifi;

use panic_halt as _;

use dvp::{ov2640_init, ov2640_read_id, ov2640_rgb565_qqvga, Dvp, ImageFormat};
use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const UNCACHED_OFFSET: usize = 0x4000_0000;
const BAUD: u32 = 115_200;
const LINK_BAUD: u32 = 921_600;
const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASS: &str = env!("WIFI_PASS");

const W: usize = 160; // QQVGA
const H: usize = 120;

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
fn le32(out: &mut [u8], v: u32) {
    out[0] = v as u8;
    out[1] = (v >> 8) as u8;
    out[2] = (v >> 16) as u8;
    out[3] = (v >> 24) as u8;
}

// The <img> reloads itself via JS (cache-busted) on load+error. Over UART there is no
// capture/recovery gap, so the reload cadence is just "as fast as a frame serves".
const HTML: &[u8] = b"<!doctype html><html><head><title>K210 cam</title></head><body style=\"background:#111;color:#eee;text-align:center;font-family:sans-serif\"><h2>K210 bare-metal Rust camera (UART WiFi)</h2><div><img id=\"c\" style=\"width:640px\"></div><p id=\"s\">connecting...</p><p>OV2640 160x120 over DVP, served live by the onboard ESP32 (nina-fw over UART). The camera no longer shares SPI0 with WiFi, so every request captures and serves a fresh frame.</p><script>var n=0,t0=Date.now();function L(){var i=document.getElementById('c');i.onload=function(){n++;var f=(n*1000/(Date.now()-t0)).toFixed(2);document.getElementById('s').textContent='frame '+n+'  ('+f+' fps)';setTimeout(L,50)};i.onerror=function(){setTimeout(L,800)};i.src='/cam.bmp?'+Date.now()}L()</script></body></html>";

fn sysctl() -> *const pac::sysctl::RegisterBlock {
    pac::SYSCTL::ptr()
}

/// Stream `data` to the TCP client over the UART modem. The ESP32's `client.write()`
/// blocks until lwip accepts the bytes, so each `S` reply IS the flow control -- no
/// nina-style quiet-gap dance. Returns false if the client went away.
const CHUNK: usize = 1440; // <= ESP32 MAXPL (1600); W*3=480 divides it, no BMP padding
fn send_all(data: &[u8], reply: &mut [u8]) -> bool {
    let mut off = 0;
    while off < data.len() {
        let end = (off + CHUNK).min(data.len());
        match uart_wifi::cmd(uart_wifi::CMD_SEND, &data[off..end], reply, 6000) {
            Some((b'S', n)) if n >= 2 => {
                let sent = (reply[0] as usize) | ((reply[1] as usize) << 8);
                if sent == 0 {
                    return false; // client gone / buffer refused
                }
                off += sent.min(end - off);
            }
            _ => return false,
        }
    }
    true
}

static mut DBG_SENT: u32 = 0;

/// Stream a native-resolution 24-bit BMP (bottom-up, BGR) of FRAME to the client.
fn serve_bmp(reply: &mut [u8]) -> bool {
    let fb = (unsafe { core::ptr::addr_of!(FRAME.px) } as usize - UNCACHED_OFFSET) as *const u32;
    let pixels = (W * H * 3) as u32;
    let filesize = 54 + pixels;

    let mut hdr = [0u8; 182];
    let mut n = 0;
    n += append(&mut hdr, n, b"HTTP/1.1 200 OK\r\nContent-Type: image/bmp\r\nContent-Length: ");
    n += write_dec(&mut hdr[n..], filesize);
    n += append(&mut hdr, n, b"\r\nConnection: close\r\n\r\n");
    let h0 = n;
    hdr[n] = b'B';
    hdr[n + 1] = b'M';
    le32(&mut hdr[n + 2..], filesize);
    le32(&mut hdr[n + 10..], 54);
    le32(&mut hdr[n + 14..], 40);
    le32(&mut hdr[n + 18..], W as u32);
    le32(&mut hdr[n + 22..], H as u32);
    hdr[n + 26] = 1;
    hdr[n + 28] = 24;
    le32(&mut hdr[n + 34..], pixels);
    n = h0 + 54;
    if !send_all(&hdr[..n], reply) {
        return false;
    }
    unsafe { DBG_SENT = n as u32 };

    let mut chunk = [0u8; CHUNK];
    let mut k = 0;
    let mut row = H;
    while row > 0 {
        row -= 1;
        for col in 0..W {
            if k + 3 > CHUNK {
                if !send_all(&chunk[..k], reply) {
                    return false;
                }
                unsafe { DBG_SENT += k as u32 };
                k = 0;
            }
            let i = row * W + col;
            let word = unsafe { core::ptr::read_volatile(fb.add(i / 2)) };
            let px = if i & 1 == 0 { word & 0xffff } else { word >> 16 };
            chunk[k] = ((px & 0x1f) << 3) as u8; // B
            chunk[k + 1] = (((px >> 5) & 0x3f) << 2) as u8; // G
            chunk[k + 2] = (((px >> 11) & 0x1f) << 3) as u8; // R
            k += 3;
        }
    }
    if k > 0 {
        if !send_all(&chunk[..k], reply) {
            return false;
        }
        unsafe { DBG_SENT += k as u32 };
    }
    true
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sc = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sc.apb0);

    // debug serial + ESP32 EN + UART1 modem link
    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _en = fpioa.io8.into_function(fpioa::GPIOHS0); // ESP32 EN
    let _u1rx = fpioa.io6.into_function(fpioa::UART1_RX); // <- ESP32 U0TXD
    let _u1tx = fpioa.io7.into_function(fpioa::UART1_TX); // -> ESP32 U0RXD
    // camera DVP data rides the SPI0 pads (spi_dvp_data_enable); WiFi no longer uses
    // SPI0, so these pads stay in DVP mode the whole time.
    let _sclk = fpioa.io27.into_function(fpioa::SPI0_SCLK);
    let _mosi = fpioa.io28.into_function(fpioa::SPI0_D0);
    let _miso = fpioa.io26.into_function(fpioa::SPI0_D1);
    // camera control (SCCB + CMOS)
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

    for _ in 0..20 {
        putc(b'.');
        delay(15_000_000);
    }
    puts(b"\nK210 camera web server (UART WiFi)\n");

    // bring up the camera; keep spi_dvp_data_enable ON for good (no SPI0 WiFi to share)
    unsafe {
        (*sysctl())
            .power_sel
            .modify(|_, w| w.power_mode_sel6().clear_bit().power_mode_sel7().clear_bit());
        (*sysctl()).clk_en_cent.modify(|_, w| w.apb2_clk_en().set_bit());
        (*sysctl()).clk_en_peri.modify(|_, w| w.spi0_clk_en().set_bit());
        (*sysctl()).misc.modify(|_, w| w.spi_dvp_data_enable().set_bit());
    }
    let dvp = Dvp::new(p.DVP);
    dvp.init();
    let _ = ov2640_read_id(&dvp);
    ov2640_init(&dvp);
    ov2640_rgb565_qqvga(&dvp);
    dvp.set_image_format(ImageFormat::RGB);
    dvp.set_image_size(false, W as u16, H as u16);
    let cached = unsafe { core::ptr::addr_of!(FRAME.px) } as *const u32 as usize;
    dvp.set_ai_addr(None);
    dvp.set_display_addr(Some(cached as u32));
    dvp.set_auto(false);
    delay(5_000_000);
    for _ in 0..3 {
        dvp.get_image(); // warm up auto-exposure / white-balance
    }
    puts(b"camera ready\n");

    // bring up WiFi over UART (camera-independent -- no wedge)
    uart_wifi::init(LINK_BAUD);
    puts(b"bringing up modem...\n");
    if !uart_wifi::bringup() {
        puts(b"modem marker NOT seen\n");
    }
    let mut reply = [0u8; 1024];

    // connect: payload = ssid '\0' pass
    let ssid = WIFI_SSID.as_bytes();
    let pass = WIFI_PASS.as_bytes();
    let mut cbuf = [0u8; 160];
    let mut cn = 0;
    for &b in ssid {
        cbuf[cn] = b;
        cn += 1;
    }
    cbuf[cn] = 0;
    cn += 1;
    for &b in pass {
        cbuf[cn] = b;
        cn += 1;
    }
    puts(b"connecting WiFi...\n");
    match uart_wifi::cmd(uart_wifi::CMD_CONNECT, &cbuf[..cn], &mut reply, 35000) {
        Some((b'I', 4)) => {
            puts(b"http://");
            put_dec(reply[0] as u32);
            putc(b'.');
            put_dec(reply[1] as u32);
            putc(b'.');
            put_dec(reply[2] as u32);
            putc(b'.');
            put_dec(reply[3] as u32);
            puts(b"/\n");
        }
        _ => {
            puts(b"wifi connect failed\n");
            loop {
                unsafe { core::arch::asm!("wfi") };
            }
        }
    }

    // listen on port 80
    let port = [80u8, 0u8]; // 80 LE
    uart_wifi::cmd(uart_wifi::CMD_LISTEN, &port, &mut reply, 2000);
    puts(b"server up\n");

    let mut frame_no = 0u32;
    loop {
        // poll for a client
        let connected = matches!(
            uart_wifi::cmd(uart_wifi::CMD_ACCEPT, &[], &mut reply, 1500),
            Some((b'A', n)) if n >= 1 && reply[0] == 1
        );
        if !connected {
            uart_wifi::sleep_ms(20);
            continue;
        }

        // read the request (poll a few times; browsers send it promptly)
        let mut req = [0u8; 256];
        let mut rl = 0usize;
        for _ in 0..24 {
            match uart_wifi::cmd(uart_wifi::CMD_RECV, &[], &mut reply, 1000) {
                Some((b'R', n)) if n > 0 => {
                    for i in 0..n {
                        if rl < req.len() {
                            req[rl] = reply[i];
                            rl += 1;
                        }
                    }
                    if rl >= 16 {
                        break;
                    }
                }
                _ => {}
            }
            uart_wifi::sleep_ms(12);
        }
        let want_bmp = req[..rl].windows(4).any(|w| w == b".bmp");

        if want_bmp {
            // capture a FRESH frame -- harmless to WiFi now -- and serve it live
            dvp.get_image();
            let start = mtime_ms();
            unsafe { DBG_SENT = 0 };
            let ok = serve_bmp(&mut reply);
            let ms = mtime_ms().wrapping_sub(start);
            frame_no += 1;
            puts(b"frame ");
            put_dec(frame_no);
            puts(if ok { b" ok " } else { b" abort " });
            put_dec(unsafe { DBG_SENT });
            puts(b"B ");
            put_dec(ms as u32);
            puts(b"ms\n");
        } else {
            let mut resp = [0u8; 1024];
            let mut hn = 0;
            hn += append(&mut resp, hn, b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: ");
            hn += write_dec(&mut resp[hn..], HTML.len() as u32);
            hn += append(&mut resp, hn, b"\r\nConnection: close\r\n\r\n");
            hn += append(&mut resp, hn, HTML);
            send_all(&resp[..hn], &mut reply);
            puts(b"served page\n");
        }

        uart_wifi::cmd(uart_wifi::CMD_CLOSE, &[], &mut reply, 2000);
    }
}

const CLINT_MTIME: *const u64 = 0x0200_BFF8 as *const u64;
fn mtime_ms() -> u64 {
    unsafe { core::ptr::read_volatile(CLINT_MTIME) / 7_800 } // mtime ~7.8 MHz
}
