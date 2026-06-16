//! K210 camera web server (the goal): serve a web page over WiFi showing the onboard
//! camera as a refreshing live stream. The camera (DVP) and WiFi (nina) both use
//! SPI0, and a live DVP capture WEDGES the ESP32's network stack (the SPI link and
//! conn-status survive, but L2/TCP dies). So we never capture while replying. Instead
//! each `/cam.bmp` request serves the CURRENT frame on a healthy network, then -- after
//! the client is closed -- captures the NEXT frame (wedging the net) and immediately
//! recovers it (EN-reset + reconnect + re-open the listening socket). The served frame
//! is thus one request stale, but every reply lands on a healthy link. Running the
//! sensor continuously also lets its auto-exposure/white-balance converge (the boot
//! snapshot is green/dark; later frames are correctly balanced).
//!
//! Credentials come from `wifi_creds.env` (gitignored) via `build.rs` -> `env!`.

#![no_std]
#![no_main]

mod dvp;
mod nina;

use panic_halt as _;

use dvp::{ov2640_init, ov2640_read_id, ov2640_rgb565_qqvga, Dvp, ImageFormat};
use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const UNCACHED_OFFSET: usize = 0x4000_0000;
const BAUD: u32 = 115_200;
const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASS: &str = env!("WIFI_PASS");

const W: usize = 160; // QQVGA: small enough to stream a few frames/sec over nina
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

const HTML: &[u8] = b"<!doctype html><html><head><meta http-equiv=\"refresh\" content=\"2\"><title>K210 cam</title></head><body style=\"background:#111;color:#eee;text-align:center;font-family:sans-serif\"><h2>K210 bare-metal Rust camera</h2><img src=\"/cam.bmp\" style=\"width:640px;image-rendering:auto\"><p>OV2640 160x120 over DVP, served live by the onboard ESP32 (nina-fw). SPI0 time-multiplexed; frame captured + network recovered between requests.</p></body></html>";

fn sysctl() -> *const pac::sysctl::RegisterBlock {
    pac::SYSCTL::ptr()
}

/// Flip SPI0 to DVP-data mode, capture one frame into FRAME, flip back to the nina
/// master. This time-multiplexes SPI0 between the camera and WiFi on a live session.
fn capture(dvp: &Dvp) {
    unsafe { (*sysctl()).misc.modify(|_, w| w.spi_dvp_data_enable().set_bit()) };
    dvp.get_image();
    unsafe { (*sysctl()).misc.modify(|_, w| w.spi_dvp_data_enable().clear_bit()) };
    nina::spi_reinit();
}

static mut DBG_SENT: u32 = 0;
static mut DBG_STALLS: u32 = 0;
static mut DBG_STATE: u8 = 0; // TCP client state seen when a send stalled (4 = established)

/// Poll DATA_SENT_TCP until the socket's queued data has actually flushed to the
/// peer (or a timeout). Polls slowly (WiFiNINA uses 100 ms) so the single-threaded
/// nina-fw gets CPU between polls to run its WiFi/TCP task and drain the buffer.
fn check_data_sent(cp: &[u8], buf: &mut [u8], lens: &mut [usize]) -> bool {
    let mut i = 0;
    while i < 60 {
        let n = nina::request(nina::CMD_DATA_SENT_TCP, &[cp], buf, lens);
        if n >= 1 && buf[0] == 1 {
            return true;
        }
        nina::sleep_ms(50);
        i += 1;
    }
    false
}

/// Stream `data` over TCP socket `cp`. SEND_DATA returns how many bytes the ESP32's
/// TCP buffer accepted; we advance by that. The catch: nina-fw is single-threaded,
/// so hammering it with back-to-back SPI commands starves the task that actually
/// transmits and frees the buffer -> it wedges full at a few KB. So we YIELD a little
/// after every send (and longer when the buffer is full) to let it drain, then do one
/// patient flush at the end before the caller closes the socket (else the tail is
/// dropped).
fn send_all(cp: &[u8], data: &[u8], buf: &mut [u8], lens: &mut [usize]) -> bool {
    let mut off = 0;
    let mut stalls = 0u32;
    while off < data.len() {
        let end = (off + 1024).min(data.len());
        let n = nina::request_send(nina::CMD_SEND_DATA_TCP, &[cp, &data[off..end]], buf, lens);
        let sent = if n >= 1 && lens[0] >= 2 {
            (buf[0] as usize) | ((buf[1] as usize) << 8)
        } else {
            0
        };
        if sent == 0 {
            // Buffer full. The ESP32 frees it by processing the peer's ACKs in its
            // WiFi task -- which only runs when we're NOT hammering it over SPI. So
            // go QUIET (no SPI commands at all) to let it drain, then retry.
            stalls += 1;
            unsafe { DBG_STALLS += 1 };
            if stalls > 60 {
                unsafe {
                    DBG_STATE =
                        nina::request(nina::CMD_GET_CLIENT_STATE_TCP, &[cp], buf, lens) as u8;
                }
                return false;
            }
            nina::sleep_ms(120);
        } else {
            let adv = sent.min(end - off);
            off += adv;
            unsafe { DBG_SENT += adv as u32 };
            stalls = 0;
            // Quiet gap so the ESP32 can transmit this chunk and process ACKs before
            // we queue more -- keeps the lwip send buffer from filling and aborting.
            nina::sleep_ms(30);
        }
    }
    check_data_sent(cp, buf, lens)
}

// Served image = the native WxH capture, no downsample. We never build the whole
// BMP in memory (that'd be a ~57 KB stack array); instead we stream it: header
// first, then pixel rows in CHUNK-sized pieces, each flow-controlled by send_all.
// W*3 is a multiple of 4 (W=160 -> 480), so BMP rows need no padding.
const CHUNK: usize = 1440;

/// Stream a native-resolution 24-bit BMP of the captured frame to socket `cp`.
fn serve_bmp(cp: &[u8], buf: &mut [u8], lens: &mut [usize]) {
    let fb = (unsafe { core::ptr::addr_of!(FRAME.px) } as usize - UNCACHED_OFFSET) as *const u32;
    let pixels = (W * H * 3) as u32;
    let filesize = 54 + pixels;

    // HTTP + 54-byte BMP header (24bpp, bottom-up) in one small buffer.
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
    if !send_all(cp, &hdr[..n], buf, lens) {
        return;
    }

    // Pixels, bottom-up, BGR, streamed in CHUNK-byte pieces.
    let mut chunk = [0u8; CHUNK];
    let mut k = 0;
    let mut row = H;
    while row > 0 {
        row -= 1;
        for col in 0..W {
            if k + 3 > CHUNK {
                if !send_all(cp, &chunk[..k], buf, lens) {
                    return;
                }
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
        send_all(cp, &chunk[..k], buf, lens);
    }
}

/// (Re)connect to the AP. Returns the wl_status after polling (3 = connected).
fn wifi_connect(buf: &mut [u8], lens: &mut [usize]) -> u8 {
    nina::request(
        nina::CMD_SET_PASSPHRASE,
        &[WIFI_SSID.as_bytes(), WIFI_PASS.as_bytes()],
        buf,
        lens,
    );
    let mut stt = 0u8;
    let mut t = 0;
    while t < 20 {
        nina::sleep_ms(500);
        let n = nina::request(nina::CMD_GET_CONN_STATUS, &[], buf, lens);
        stt = if n >= 1 { buf[0] } else { 0xff };
        if stt == nina::WL_CONNECTED {
            break;
        }
        t += 1;
    }
    stt
}

/// Print the current IP as `http://a.b.c.d/`.
fn print_ip(buf: &mut [u8], lens: &mut [usize]) {
    nina::request(nina::CMD_GET_IPADDR, &[&[0xff]], buf, lens);
    puts(b"http://");
    put_dec(buf[0] as u32);
    putc(b'.');
    put_dec(buf[1] as u32);
    putc(b'.');
    put_dec(buf[2] as u32);
    putc(b'.');
    put_dec(buf[3] as u32);
    puts(b"/\n");
}

/// Open a fresh persistent listening socket on port 80. Returns its socket number.
fn start_server(buf: &mut [u8], lens: &mut [usize]) -> u8 {
    let port_be = [0u8, 80u8];
    let mode = [0u8];
    nina::request(nina::CMD_GET_SOCKET, &[], buf, lens);
    let listen = buf[0];
    let lp = [listen];
    nina::request(nina::CMD_START_SERVER_TCP, &[&port_be, &lp, &mode], buf, lens);
    listen
}

#[entry]
fn main() -> ! {
    let p = pac::Peripherals::take().unwrap();
    let mut sc = p.SYSCTL.constrain();
    let fpioa = p.FPIOA.split(&mut sc.apb0);

    let _tx = fpioa.io5.into_function(fpioa::UARTHS_TX);
    let _en = fpioa.io8.into_function(fpioa::GPIOHS0);
    let _cs = fpioa.io25.into_function(fpioa::GPIOHS1);
    let _rdy = fpioa.io9.into_function(fpioa::GPIOHS2);
    let _sclk = fpioa.io27.into_function(fpioa::SPI0_SCLK);
    let _mosi = fpioa.io28.into_function(fpioa::SPI0_D0);
    let _miso = fpioa.io26.into_function(fpioa::SPI0_D1);
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

    // brief heartbeat: gives the host serial capture time to attach (the USB
    // bridge's auto-reset makes early bytes easy to miss) and shows boot progress.
    for _ in 0..20 {
        putc(b'.');
        delay(15_000_000);
    }
    puts(b"\nK210 camera web server\n");

    // Capture ONE frame BEFORE WiFi comes up. The DVP capture wedges the ESP32's
    // network stack (the SPI link survives, but the live TCP connection dies), so we
    // grab a static snapshot first and let `nina::init()` reset the ESP32 fresh
    // afterwards. The server then serves this frozen frame -- no capture ever touches
    // a live WiFi connection.
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
        dvp.get_image();
    }
    unsafe { (*sysctl()).misc.modify(|_, w| w.spi_dvp_data_enable().clear_bit()) };
    puts(b"camera frame captured\n");

    // now bring up WiFi: nina resets the ESP32 and takes over SPI0 as its master.
    nina::init();
    let mut buf = [0u8; 1024];
    let mut lens = [0usize; 8];
    puts(b"connecting\n");
    if wifi_connect(&mut buf, &mut lens) != nina::WL_CONNECTED {
        puts(b"wifi failed\n");
        loop {
            unsafe { core::arch::asm!("wfi") };
        }
    }
    print_ip(&mut buf, &mut lens);

    let accept = [0u8];
    let mut listen = start_server(&mut buf, &mut lens);
    puts(b"server up sock=");
    put_dec(listen as u32);
    putc(b'\n');

    loop {
        let lp = [listen];
        let n = nina::request(nina::CMD_AVAIL_DATA_TCP, &[&lp, &accept], &mut buf, &mut lens);
        let client = if n >= 1 { buf[0] } else { 255 };
        if client != 255 && client != listen {
            let cp = [client];
            nina::sleep_ms(20);
            // read the request and look for "cam.bmp"
            let a = nina::request(nina::CMD_AVAIL_DATA_TCP, &[&cp], &mut buf, &mut lens);
            let have = if a >= 1 && lens[0] >= 2 {
                (buf[0] as u16) | ((buf[1] as u16) << 8)
            } else {
                0
            };
            let mut want_bmp = false;
            if have > 0 {
                let want = if have > 1024 { 1024 } else { have };
                let wl = [(want & 0xff) as u8, (want >> 8) as u8];
                let rn = nina::request_wide(nina::CMD_GET_DATABUF_TCP, &[&cp, &wl], &mut buf, &mut lens);
                if rn >= 1 {
                    let req = &buf[..lens[0].min(buf.len())];
                    // crude path check: look for ".bmp"
                    for win in req.windows(4) {
                        if win == b".bmp" {
                            want_bmp = true;
                            break;
                        }
                    }
                }
            }

            if want_bmp {
                unsafe {
                    DBG_SENT = 0;
                    DBG_STALLS = 0;
                }
                // serve the CURRENT frame on the healthy network (the capture that
                // produced it already ran + recovered on the previous request).
                serve_bmp(&cp, &mut buf, &mut lens);
                puts(b"frame: sent=");
                put_dec(unsafe { DBG_SENT });
                puts(b" stalls=");
                put_dec(unsafe { DBG_STALLS });
                puts(b" state=");
                put_dec(unsafe { DBG_STATE } as u32);
                putc(b'\n');
            } else {
                let mut resp = [0u8; 700];
                let mut hn = 0;
                hn += append(&mut resp, hn, b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: ");
                hn += write_dec(&mut resp[hn..], HTML.len() as u32);
                hn += append(&mut resp, hn, b"\r\nConnection: close\r\n\r\n");
                hn += append(&mut resp, hn, HTML);
                nina::request_send(nina::CMD_SEND_DATA_TCP, &[&cp, &resp[..hn]], &mut buf, &mut lens);
                puts(b"served page\n");
            }
            nina::sleep_ms(40);
            nina::request(nina::CMD_STOP_CLIENT_TCP, &[&cp], &mut buf, &mut lens);
            nina::wait_idle(500);

            if want_bmp {
                // Client served and closed. Now refresh the frame for next time: the
                // DVP capture WEDGES the ESP32 network (SPI link + conn-status survive
                // but L2/TCP dies), so immediately recover by resetting + reconnecting
                // the ESP32. Doing it here -- between requests, on a connection we've
                // already closed -- keeps every actual serve on a healthy network.
                capture(&dvp);
                // Recover the wedged network. The reconnect is flaky (~1/3 of EN-reset
                // attempts don't reach WL_CONNECTED in time), so retry until it sticks
                // -- otherwise we'd serve a dead 0.0.0.0 link until the next request.
                let mut st = 0u8;
                let mut tries = 0u32;
                while st != nina::WL_CONNECTED && tries < 8 {
                    nina::init(); // EN-reset the ESP32 out of its wedged state
                    st = wifi_connect(&mut buf, &mut lens);
                    tries += 1;
                }
                listen = start_server(&mut buf, &mut lens);
                nina::sleep_ms(800); // let the reconnected link settle (ARP/AP relearn)
                puts(b"recover conn=");
                put_dec(st as u32);
                puts(b" tries=");
                put_dec(tries);
                puts(b" sock=");
                put_dec(listen as u32);
                putc(b' ');
                print_ip(&mut buf, &mut lens);
            }
        }
        nina::sleep_ms(50);
    }
}
