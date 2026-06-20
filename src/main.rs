//! K210 camera web server over the UART WiFi path, with web-selectable resolution
//! and toggleable on-chip denoise.
//!
//! The onboard ESP32 runs the UART-modem nina-fw (esp32-modem-ninafw/): WiFi driven
//! over UART1 (IO6/IO7), independent of the camera's SPI0/DVP pads. So unlike the SPI
//! version (tag `nina-spi-camera-webserver`), a DVP capture no longer wedges the
//! network -- there is NO ~5 s EN-reset/reconnect/re-listen dance per frame. Each
//! request captures a FRESH frame and serves it live on a healthy connection.
//!
//! The page has QQVGA / QVGA / VGA buttons (`/cam.bmp?r=N`, N=0/1/2) and a Denoise
//! toggle (`&d=1`). Denoise = per-channel temporal median of 3 frames (kills the
//! random DVP salt-pepper speckle) + per-row destripe (equalizes row brightness,
//! kills the horizontal banding) -- both this board's DVP signal-quality artifacts.
//!
//! Credentials come from `wifi_creds.env` (gitignored) via `build.rs` -> `env!`.

#![no_std]
#![no_main]

mod dvp;
mod uart_wifi;

use panic_halt as _;

use dvp::{
    ov2640_init, ov2640_jpeg_qvga, ov2640_read_id, ov2640_rgb565_qqvga, ov2640_rgb565_vga, Dvp,
    ImageFormat,
};
use k210_hal::fpioa;
use k210_hal::pac;
use k210_hal::prelude::*;
use riscv_rt::entry;

const UARTHS_TXDATA: *mut u32 = 0x3800_0000 as *mut u32;
const UNCACHED_OFFSET: usize = 0x4000_0000;
const BAUD: u32 = 115_200;
const LINK_BAUD: u32 = 3_000_000; // exact K210 divisor (195MHz/48 = 4.0625); UART is the
                                  // image-transfer bottleneck, so 3M ~halves frame time.
const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASS: &str = env!("WIFI_PASS");

// Frame buffers sized for the largest resolution (VGA); smaller sizes use the front.
const MAXW: usize = 640;
const MAXH: usize = 480;

#[repr(C, align(64))]
struct Frame {
    px: [u32; MAXW * MAXH / 2],
}
// Three capture buffers: denoise OFF uses CAP[0]; denoise ON captures 3 frames and
// medians them into CAP[0]. 3 * 614 KB (VGA) = 1.84 MB, fits the 6 MB SRAM.
static mut CAP: [Frame; 3] = [
    Frame { px: [0; MAXW * MAXH / 2] },
    Frame { px: [0; MAXW * MAXH / 2] },
    Frame { px: [0; MAXW * MAXH / 2] },
];

// (w, h) per resolution index 0/1/2.
const RES: [(usize, usize); 3] = [(160, 120), (320, 240), (640, 480)];

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

// Resolution picker + denoise toggle + live <img> reload. Each control updates r/d;
// the loop re-requests /cam.bmp?r=<r>&d=<d> on load (cache-busted) so the stream just
// runs as fast as frames serve.
const HTML: &[u8] = b"<!doctype html><html><head><title>K210 cam</title><meta name=viewport content=\"width=device-width,initial-scale=1\"></head><body style=\"background:#111;color:#eee;text-align:center;font-family:sans-serif\"><h2>K210 bare-metal Rust camera (UART WiFi)</h2><div><button id=\"r0\" onclick=\"S(0)\">QQVGA</button> <button id=\"r1\" onclick=\"S(1)\">QVGA</button> <button id=\"r2\" onclick=\"S(2)\">VGA</button> <button id=\"jb\" onclick=\"J()\">JPEG</button> &nbsp; <button id=\"db\" onclick=\"D()\">Denoise: OFF</button> <button id=\"eb\" onclick=\"E()\">RGB565: OFF</button></div><div style=\"margin-top:8px\"><canvas id=\"c\" style=\"width:640px;max-width:98vw;image-rendering:pixelated\"></canvas></div><p id=\"s\">connecting...</p><div style=\"margin:6px\"><input id=\"u\" readonly style=\"width:90%;max-width:600px;background:#222;color:#9f9;border:1px solid #444;padding:4px;font-family:monospace\"> <button onclick=\"C()\">Copy URL</button> <span style=\"opacity:.6\">(curl this to keep grabbing this mode)</span></div><p>BMP = lossless RGB565. Denoise = median + destripe. RGB565 = send 2 B/px and let the ESP32 expand (33% less UART, lossless). JPEG = OV2640 hardware codec (~8 KB, fast) -- some frames colour-shift (DVP error on a DC coeff, no restart markers).</p><script>var r=1,d=0,e=0,j=0,n=0,t0=Date.now();function g(i){return document.getElementById(i)}function url(){if(j)return location.origin+'/cam.jpg';var p=['qqvga','qvga','vga'][r]+'.bmp',q=[];if(d)q.push('d=1');if(e)q.push('e=1');return location.origin+'/'+p+(q.length?'?'+q.join('&'):'')}function upd(){var b=!j,i;for(i=0;i<3;i++)g('r'+i).style.background=(b&&r==i)?'#383':'';g('jb').style.background=j?'#383':'';var x=g('db'),y=g('eb');x.disabled=y.disabled=!b;x.style.opacity=y.style.opacity=b?1:.4;x.style.background=(b&&d)?'#383':'#555';y.style.background=(b&&e)?'#383':'#555';x.textContent='Denoise '+(d?'ON':'OFF');y.textContent='RGB565 '+(e?'ON':'OFF');g('u').value=url()}function C(){var u=g('u');u.focus();u.select();u.setSelectionRange(0,99999);try{document.execCommand('copy')}catch(z){}}function S(x){r=x;j=0;n=0;t0=Date.now();upd()}function J(){j=1;n=0;t0=Date.now();upd()}function D(){if(!j){d=d?0:1;n=0;t0=Date.now();upd()}}function E(){if(!j){e=e?0:1;n=0;t0=Date.now();upd()}}function L(){var im=new Image();im.onload=function(){var c=g('c');c.width=im.naturalWidth;c.height=im.naturalHeight;c.getContext('2d').drawImage(im,0,0);n++;var s=(j?'JPEG QVGA':['QQVGA','QVGA','VGA'][r]+(d?' +denoise':'')+(e?' +RGB565':''))+'  frame '+n;if(n<2){t0=Date.now()}else{s+='  ('+((n-1)*1000/(Date.now()-t0)).toFixed(2)+' fps)'}g('s').textContent=s;setTimeout(L,40)};im.onerror=function(){setTimeout(L,800)};var u=url();im.src=u+(u.indexOf('?')<0?'?':'&')+'t='+Date.now()}upd();L()</script></body></html>";

fn sysctl() -> *const pac::sysctl::RegisterBlock {
    pac::SYSCTL::ptr()
}

const CLINT_MTIME: *const u64 = 0x0200_BFF8 as *const u64;
fn mtime_ms() -> u64 {
    unsafe { core::ptr::read_volatile(CLINT_MTIME) / 7_800 } // mtime ~7.8 MHz
}

/// Cached / uncached base of capture buffer `i`. The DVP writes via the cached addr
/// (DMA hits physical SRAM); the CPU reads/writes via the uncached alias so it sees
/// DMA data and its own writes without cache games.
fn cap_cached(i: usize) -> u32 {
    unsafe { core::ptr::addr_of!(CAP[i].px) as u32 }
}
fn cap_uncached(i: usize) -> *mut u32 {
    (cap_cached(i) as usize - UNCACHED_OFFSET) as *mut u32
}

/// Capture one frame into buffer `i`.
fn capture(dvp: &Dvp, i: usize) {
    dvp.set_display_addr(Some(cap_cached(i)));
    dvp.get_image();
}

/// Apply the OV2640 + DVP config for resolution index `r` and return (w, h). Re-runs
/// the baseline RGB565 init each time so switching is order-independent, then layers
/// the size-specific scaler delta and warms up (AE/AWB settle) for ~2 s of wall-clock.
fn configure_res(dvp: &Dvp, r: usize) -> (usize, usize) {
    ov2640_init(dvp);
    match r {
        0 => ov2640_rgb565_qqvga(dvp),
        2 => ov2640_rgb565_vga(dvp),
        _ => {} // 1 = QVGA: baseline, no delta
    }
    let (w, h) = RES[r];
    dvp.set_image_format(ImageFormat::RGB);
    dvp.set_image_size(false, w as u16, h as u16);
    // Warm-up by wall-clock, not frame count: the OV2640's AE/AWB restart on the
    // re-init and take ~1-2 s to converge. A fixed frame count isn't enough at VGA
    // (lower fps), so the first served frame after a switch came out green. Capture
    // and discard for ~2 s so the first served frame is balanced. (Only on a size
    // change, not every same-res frame.)
    let t_end = mtime_ms() + 2000;
    while mtime_ms() < t_end {
        capture(dvp, 0);
    }
    (w, h)
}

fn med3(a: u32, b: u32, c: u32) -> u32 {
    let mx = a.max(b).max(c);
    let mn = a.min(b).min(c);
    a + b + c - mx - mn
}
/// Per-channel RGB565 median of three pixels (rejects a per-frame speckle outlier).
fn med_px(p0: u32, p1: u32, p2: u32) -> u32 {
    let r = med3((p0 >> 11) & 0x1f, (p1 >> 11) & 0x1f, (p2 >> 11) & 0x1f);
    let g = med3((p0 >> 5) & 0x3f, (p1 >> 5) & 0x3f, (p2 >> 5) & 0x3f);
    let b = med3(p0 & 0x1f, p1 & 0x1f, p2 & 0x1f);
    (r << 11) | (g << 5) | b
}
/// Temporal median of CAP[0..3] -> CAP[0] (in place). The DVP speckle is random per
/// capture, so the median of 3 frames keeps the correct value at each pixel.
fn denoise_median(w: usize, h: usize) {
    let n = w * h / 2;
    let a = cap_uncached(0);
    let b = cap_uncached(1);
    let c = cap_uncached(2);
    for j in 0..n {
        unsafe {
            let w0 = core::ptr::read_volatile(a.add(j));
            let w1 = core::ptr::read_volatile(b.add(j));
            let w2 = core::ptr::read_volatile(c.add(j));
            let lo = med_px(w0 & 0xffff, w1 & 0xffff, w2 & 0xffff);
            let hi = med_px(w0 >> 16, w1 >> 16, w2 >> 16);
            core::ptr::write_volatile(a.add(j), lo | (hi << 16));
        }
    }
}

fn clamp_ch(v: i32, max: i32) -> u32 {
    if v < 0 {
        0
    } else if v > max {
        max as u32
    } else {
        v as u32
    }
}
/// Shift one RGB565 pixel's brightness by `off` (in 6-bit G units; R/B scaled by 1/2).
fn apply_off(p: u32, off: i32) -> u32 {
    let r = clamp_ch(((p >> 11) & 0x1f) as i32 + off / 2, 0x1f);
    let g = clamp_ch(((p >> 5) & 0x3f) as i32 + off, 0x3f);
    let b = clamp_ch((p & 0x1f) as i32 + off / 2, 0x1f);
    (r << 11) | (g << 5) | b
}
/// Per-row destripe on CAP[0]: shift each row's brightness toward the global mean to
/// flatten the OV2640's per-row fixed-pattern banding. Uses the 6-bit G channel as the
/// luma proxy and moves R/G/B together so colour is preserved.
fn destripe(w: usize, h: usize) {
    let buf = cap_uncached(0);
    let wpr = w / 2; // u32 words (2 px) per row
    let mut gsum: u64 = 0;
    for j in 0..(wpr * h) {
        let word = unsafe { core::ptr::read_volatile(buf.add(j)) };
        gsum += (((word >> 5) & 0x3f) + ((word >> 21) & 0x3f)) as u64;
    }
    let gmean = (gsum / (w * h) as u64) as i32;
    for row in 0..h {
        let base = row * wpr;
        let mut rsum: u32 = 0;
        for k in 0..wpr {
            let word = unsafe { core::ptr::read_volatile(buf.add(base + k)) };
            rsum += ((word >> 5) & 0x3f) + ((word >> 21) & 0x3f);
        }
        let off = gmean - (rsum / w as u32) as i32;
        if off == 0 {
            continue;
        }
        for k in 0..wpr {
            let word = unsafe { core::ptr::read_volatile(buf.add(base + k)) };
            let lo = apply_off(word & 0xffff, off);
            let hi = apply_off(word >> 16, off);
            unsafe { core::ptr::write_volatile(buf.add(base + k), lo | (hi << 16)) };
        }
    }
}

/// Parse a single decimal digit following `key` in the request (e.g. b"?r=" -> 2),
/// clamped to `maxv`; `default` if not found.
fn parse_digit(req: &[u8], key: &[u8], default: usize, maxv: usize) -> usize {
    let kl = key.len();
    if req.len() <= kl {
        return default;
    }
    let mut i = 0;
    while i + kl < req.len() {
        if &req[i..i + kl] == key {
            let d = req[i + kl];
            if d.is_ascii_digit() {
                let v = (d - b'0') as usize;
                if v <= maxv {
                    return v;
                }
            }
        }
        i += 1;
    }
    default
}

/// True if `hay` contains `needle` (substring).
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.len() <= hay.len() && hay.windows(needle.len()).any(|w| w == needle)
}

/// A 0/1 query flag that may appear as the first (`?k=`) or a later (`&k=`) param.
fn flag(req: &[u8], q: &[u8], a: &[u8]) -> usize {
    if parse_digit(req, q, 0, 1) == 1 || parse_digit(req, a, 0, 1) == 1 {
        1
    } else {
        0
    }
}

const CHUNK: usize = 1440; // <= ESP32 MAXPL (1600)
// Pipelined frames in flight. Capped at 2 by the K210's 16-byte UART1 RX FIFO: while
// the K210 busy-sends a frame it can't drain acks, and >2 pending 7-byte acks (>14 B)
// would overflow that FIFO and desync. 2 still overlaps one frame's UART send with the
// ESP32's WiFi write of the previous (the ESP32's RX ring buffer, 8 KB, easily holds 2).
const WINDOW: usize = 2;

/// Pipelined sender: pushes command frames over UART without waiting for each ack, up
/// to WINDOW in flight, so the K210's UART send of frame N+1 overlaps the ESP32's WiFi
/// write of frame N (the ESP32's UART RX ISR keeps buffering while its loop blocks in
/// client.write). The ESP32 acks each frame as it finishes; we collect acks to free
/// window slots. Flow control: the ESP32's `client.write` still blocks on lwip, and an
/// ack with sent==0 means the client went away. Removes the stop-and-wait gap (~35%).
struct Pipe {
    inflight: usize,
    ok: bool,
}
impl Pipe {
    fn new() -> Self {
        Pipe { inflight: 0, ok: true }
    }
    /// Collect one ack, freeing a window slot. Returns false on failure.
    fn ack(&mut self, reply: &mut [u8]) -> bool {
        match uart_wifi::read_reply(reply, 8000) {
            Some((b'S', n)) if n >= 2 => {
                self.inflight -= 1;
                if (reply[0] as usize) | ((reply[1] as usize) << 8) == 0 {
                    self.ok = false; // client gone
                }
                self.ok
            }
            _ => {
                self.inflight = self.inflight.saturating_sub(1);
                self.ok = false;
                false
            }
        }
    }
    /// Send one frame (<= CHUNK), waiting for a slot first. `data` is fully written to
    /// the UART before returning, so the caller may reuse its buffer immediately.
    fn push(&mut self, cmd: u8, data: &[u8], reply: &mut [u8]) {
        if !self.ok {
            return;
        }
        if self.inflight >= WINDOW && !self.ack(reply) {
            return;
        }
        uart_wifi::send_frame(cmd, data);
        self.inflight += 1;
    }
    /// Drain the remaining acks. Returns true if every frame was accepted.
    fn flush(&mut self, reply: &mut [u8]) -> bool {
        while self.inflight > 0 {
            if !self.ack(reply) {
                break;
            }
        }
        self.ok
    }
}

/// Stream contiguous `data` over the UART modem, pipelined (header/JPEG/page paths).
fn send_all(cmd: u8, data: &[u8], reply: &mut [u8]) -> bool {
    let mut p = Pipe::new();
    let mut off = 0;
    while off < data.len() {
        let end = (off + CHUNK).min(data.len());
        p.push(cmd, &data[off..end], reply);
        off = end;
    }
    p.flush(reply)
}

static mut DBG_SENT: u32 = 0;

/// Stream a `w`x`h` 24-bit BMP (bottom-up, BGR) of CAP[0] to the client.
/// Serve a `w`x`h` 24-bit BMP of CAP[0]. The HTTP+BMP header always declares 24-bit.
/// If `expand565`, the pixel data goes over UART as RGB565 (2 B/px) and the ESP32
/// expands it to BGR24 (CMD_SEND565) -- 33% less UART (the bottleneck), lossless, same
/// bytes on the wire to the browser. Else the K210 expands to BGR24 itself (CMD_SEND).
fn serve_bmp(w: usize, h: usize, expand565: bool, reply: &mut [u8]) -> bool {
    let fb = cap_uncached(0) as *const u32;
    let pixels = (w * h * 3) as u32;
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
    le32(&mut hdr[n + 18..], w as u32);
    le32(&mut hdr[n + 22..], h as u32);
    hdr[n + 26] = 1;
    hdr[n + 28] = 24;
    le32(&mut hdr[n + 34..], pixels);
    n = h0 + 54;
    // One pipeline across the header + every pixel chunk, so frame N+1's UART send
    // overlaps the ESP32's WiFi write of frame N.
    let mut pipe = Pipe::new();
    pipe.push(uart_wifi::CMD_SEND, &hdr[..n], reply);
    unsafe { DBG_SENT = n as u32 };

    // W*3 (and W*2) is a multiple of 4 for W in {160,320,640}, so rows need no padding.
    let pcmd = if expand565 { uart_wifi::CMD_SEND565 } else { uart_wifi::CMD_SEND };
    let need = if expand565 { 2 } else { 3 };
    let mut chunk = [0u8; CHUNK];
    let mut k = 0;
    let mut row = h;
    while row > 0 {
        row -= 1;
        for col in 0..w {
            if k + need > CHUNK {
                pipe.push(pcmd, &chunk[..k], reply);
                unsafe { DBG_SENT += k as u32 };
                k = 0;
                if !pipe.ok {
                    return false;
                }
            }
            let i = row * w + col;
            let word = unsafe { core::ptr::read_volatile(fb.add(i / 2)) };
            let px = if i & 1 == 0 { word & 0xffff } else { word >> 16 };
            if expand565 {
                chunk[k] = px as u8; // RGB565 little-endian; ESP32 expands to BGR24
                chunk[k + 1] = (px >> 8) as u8;
                k += 2;
            } else {
                chunk[k] = ((px & 0x1f) << 3) as u8; // B
                chunk[k + 1] = (((px >> 5) & 0x3f) << 2) as u8; // G
                chunk[k + 2] = (((px >> 11) & 0x1f) << 3) as u8; // R
                k += 3;
            }
        }
    }
    if k > 0 {
        pipe.push(pcmd, &chunk[..k], reply);
        unsafe { DBG_SENT += k as u32 };
    }
    pipe.flush(reply)
}

// ---- JPEG spike (option 2): OV2640 hardware JPEG over UART ----------------------
// Tests whether the hardware JPEG is usable on this board's DVP (which has ~1 random
// byte error per 15-30 KB). UXGA JPEG is the only config proven to capture cleanly
// (VGA JPEG hung the DVP frame-sync historically). The capture buffer = full CAP[0].
const JCAP_WORDS: usize = MAXW * MAXH / 2; // 614 KB capacity for the JPEG byte stream

/// Configure the OV2640 for JPEG (QVGA 320x240) output + a DVP geometry big enough to
/// hold it. QVGA JPEG is ~few KB, so far more likely to be free of DVP byte errors
/// than the ~105 KB UXGA stream -- this is the clean-rate spike.
fn configure_jpeg(dvp: &Dvp) {
    ov2640_jpeg_qvga(dvp);
    dvp.set_image_format(ImageFormat::RGB); // DVP just grabs the byte stream
    dvp.set_image_size(false, MAXW as u16, MAXH as u16);
    let t_end = mtime_ms() + 2000;
    while mtime_ms() < t_end {
        capture(dvp, 0);
    }
}

/// Capture a JPEG into CAP[0], byte-swap so the stream is contiguous, find SOI..EOI,
/// and serve it as image/jpeg. Returns (found, jpeg_len). The DVP packs the byte
/// stream big-endian within each 32-bit word, so swap_bytes() makes CAP[0] a plain
/// contiguous JPEG byte array.
fn serve_jpeg(dvp: &Dvp, reply: &mut [u8]) -> (bool, usize) {
    capture(dvp, 0);
    let buf = cap_uncached(0);
    for j in 0..JCAP_WORDS {
        unsafe {
            let w = core::ptr::read_volatile(buf.add(j));
            core::ptr::write_volatile(buf.add(j), w.swap_bytes());
        }
    }
    let bytes = cap_uncached(0) as *const u8;
    let nb = JCAP_WORDS * 4;
    let rd = |i: usize| -> u8 { unsafe { core::ptr::read_volatile(bytes.add(i)) } };
    // SOI (FF D8)
    let mut soi = 0usize;
    let mut found = false;
    let mut i = 0;
    while i + 1 < nb {
        if rd(i) == 0xff && rd(i + 1) == 0xd8 {
            soi = i;
            found = true;
            break;
        }
        i += 1;
    }
    if !found {
        return (false, 0);
    }
    // EOI (FF D9)
    let mut eoi = 0usize;
    found = false;
    let mut j = soi + 2;
    while j + 1 < nb {
        if rd(j) == 0xff && rd(j + 1) == 0xd9 {
            eoi = j + 2;
            found = true;
            break;
        }
        j += 1;
    }
    if !found {
        return (false, 0);
    }
    let len = eoi - soi;
    let mut hdr = [0u8; 96];
    let mut n = 0;
    n += append(&mut hdr, n, b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: ");
    n += write_dec(&mut hdr[n..], len as u32);
    n += append(&mut hdr, n, b"\r\nConnection: close\r\n\r\n");
    if !send_all(uart_wifi::CMD_SEND, &hdr[..n], reply) {
        return (true, len);
    }
    let slice = unsafe { core::slice::from_raw_parts(bytes.add(soi), len) };
    send_all(uart_wifi::CMD_SEND, slice, reply);
    (true, len)
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

    for _ in 0..20 {
        putc(b'.');
        delay(15_000_000);
    }
    puts(b"\nK210 camera web server (UART WiFi, res + denoise)\n");

    // camera up; keep spi_dvp_data_enable ON for good (no SPI0 WiFi to share)
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
    dvp.set_ai_addr(None);
    dvp.set_auto(false);
    let mut cur_res = 1usize; // default QVGA (matches the page's default r=1)
    let mut cur_jpeg = false; // JPEG mode (option-2 spike), toggled by &j=1
    let (mut w, mut h) = configure_res(&dvp, cur_res);
    puts(b"camera ready\n");

    // bring up WiFi over UART (camera-independent -- no wedge)
    uart_wifi::init(LINK_BAUD);
    puts(b"bringing up modem...\n");
    if !uart_wifi::bringup() {
        puts(b"modem marker NOT seen\n");
    }
    let mut reply = [0u8; 1024];

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

    let port = [80u8, 0u8];
    uart_wifi::cmd(uart_wifi::CMD_LISTEN, &port, &mut reply, 2000);
    puts(b"server up\n");

    let mut frame_no = 0u32;
    loop {
        let connected = matches!(
            uart_wifi::cmd(uart_wifi::CMD_ACCEPT, &[], &mut reply, 1500),
            Some((b'A', n)) if n >= 1 && reply[0] == 1
        );
        if !connected {
            uart_wifi::sleep_ms(20);
            continue;
        }

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
                    if rl >= 24 {
                        break;
                    }
                }
                _ => {}
            }
            uart_wifi::sleep_ms(12);
        }
        // Mode comes from the PATH: /cam.jpg (JPEG), /{qqvga,qvga,vga}.bmp (BMP);
        // toggles stay as query flags (?d=1 denoise, ?e=1 RGB565-direct). Old query
        // forms (?j=1, ?r=N) still work as a fallback.
        let rs = &req[..rl];
        let is_jpg = contains(rs, b".jpg") || flag(rs, b"?j=", b"&j=") == 1;
        let is_bmp = contains(rs, b".bmp");

        if is_jpg {
            // ---- JPEG mode (/cam.jpg) ----
            if !cur_jpeg {
                configure_jpeg(&dvp);
                cur_jpeg = true;
            }
            let start = mtime_ms();
            let (found, len) = serve_jpeg(&dvp, &mut reply);
            let ms = mtime_ms().wrapping_sub(start);
            frame_no += 1;
            puts(b"frame ");
            put_dec(frame_no);
            puts(if found { b" JPG " } else { b" JPG-NOTFOUND " });
            put_dec(len as u32);
            puts(b"B ");
            put_dec(ms as u32);
            puts(b"ms\n");
        } else if is_bmp {
            if cur_jpeg {
                let (nw, nh) = configure_res(&dvp, cur_res); // back to RGB
                w = nw;
                h = nh;
                cur_jpeg = false;
            }
            // resolution from the path (qqvga before qvga before vga -- substrings!),
            // else the old ?r= query, else keep current.
            let r = if contains(rs, b"qqvga") {
                0
            } else if contains(rs, b"qvga") {
                1
            } else if contains(rs, b"vga") {
                2
            } else {
                parse_digit(rs, b"?r=", cur_res, 2)
            };
            let d = flag(rs, b"?d=", b"&d=");
            let e = flag(rs, b"?e=", b"&e="); // RGB565-direct (ESP32 expands)
            if r != cur_res {
                let (nw, nh) = configure_res(&dvp, r); // ~185 SCCB writes + warm-up
                w = nw;
                h = nh;
                cur_res = r;
            }
            if d == 1 {
                capture(&dvp, 0);
                capture(&dvp, 1);
                capture(&dvp, 2);
                denoise_median(w, h);
                destripe(w, h);
            } else {
                capture(&dvp, 0);
            }
            let start = mtime_ms();
            unsafe { DBG_SENT = 0 };
            let ok = serve_bmp(w, h, e == 1, &mut reply);
            let ms = mtime_ms().wrapping_sub(start);
            frame_no += 1;
            puts(b"frame ");
            put_dec(frame_no);
            puts(b" r");
            put_dec(cur_res as u32);
            puts(b" d");
            put_dec(d as u32);
            puts(b" e");
            put_dec(e as u32);
            puts(if ok { b" ok " } else { b" abort " });
            put_dec(unsafe { DBG_SENT });
            puts(b"B ");
            put_dec(ms as u32);
            puts(b"ms\n");
        } else {
            let mut resp = [0u8; 3072];
            let mut hn = 0;
            hn += append(&mut resp, hn, b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: ");
            hn += write_dec(&mut resp[hn..], HTML.len() as u32);
            hn += append(&mut resp, hn, b"\r\nConnection: close\r\n\r\n");
            hn += append(&mut resp, hn, HTML);
            send_all(uart_wifi::CMD_SEND, &resp[..hn], &mut reply);
            puts(b"served page\n");
        }

        uart_wifi::cmd(uart_wifi::CMD_CLOSE, &[], &mut reply, 2000);
    }
}
