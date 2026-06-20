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
mod jpeg;
mod uart_wifi;

use panic_halt as _;

use dvp::{
    ov2640_init, ov2640_read_id, ov2640_rgb565_qqvga, ov2640_rgb565_vga, Dvp, ImageFormat,
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
// Buffers: CAP[0]/CAP[3] are a capture ping-pong (the pipeline fills one while encoding
// the other); CAP[1] = JPEG output, CAP[2] = hart 1's segment / single-core cached copy.
// Denoise (synchronous fallback) captures into CAP[0..3] and medians into CAP[0].
// 4 * 614 KB (VGA) = 2.46 MB, plus two 512 KB stacks, fits the 6 MB SRAM.
static mut CAP: [Frame; 4] = [
    Frame { px: [0; MAXW * MAXH / 2] },
    Frame { px: [0; MAXW * MAXH / 2] },
    Frame { px: [0; MAXW * MAXH / 2] },
    Frame { px: [0; MAXW * MAXH / 2] },
];

// (w, h) per resolution index 0/1/2.
const RES: [(usize, usize); 3] = [(160, 120), (320, 240), (640, 480)];

// ---- dual-core (hart 0 = main/server, hart 1 = JPEG co-worker) --------------------
// The two K210 cores have NO L1 cache coherency, so every field shared between them is
// read/written through the UNCACHED alias (cached_addr - 0x4000_0000) which bypasses
// both L1s. SHARED is a plain [u32] indexed by the S_* field ids below.
static mut SHARED: [u32; 16] = [0; 16];
const S_READY: usize = 0; // hart0 -> hart1: full init done, worker may run
const S_STATE: usize = 1; // job handshake: 0 idle, 1 go, 2 done
const S_FB: usize = 2; // source frame (uncached ptr, low 32 bits of the 0x40.. alias)
const S_W: usize = 3;
const S_H: usize = 4;
const S_MCY0: usize = 5; // MCU-row range [mcy0, mcy1) for hart 1
const S_MCY1: usize = 6;
const S_OUT: usize = 7; // hart1 entropy output (uncached ptr)
const S_OUTCAP: usize = 8; // output capacity (bytes)
const S_LEN: usize = 9; // hart1 -> hart0: bytes written
const S_HEARTBEAT: usize = 10; // hart1 liveness counter
const S_OVF: usize = 11; // hart1 -> hart0: output overflowed
const S_SINK: usize = 12; // scratch sink for flush_l1's read (kept off S_HEARTBEAT)

#[inline]
fn shared_base() -> *mut u32 {
    (core::ptr::addr_of!(SHARED) as usize - UNCACHED_OFFSET) as *mut u32
}
#[inline]
fn sld(i: usize) -> u32 {
    unsafe { core::ptr::read_volatile(shared_base().add(i)) }
}
#[inline]
fn sst(i: usize, v: u32) {
    unsafe { core::ptr::write_volatile(shared_base().add(i), v) }
}
#[inline]
fn fence() {
    unsafe { core::arch::asm!("fence") }
}
#[inline]
fn hartid() -> usize {
    let id: usize;
    unsafe { core::arch::asm!("csrr {}, mhartid", out(reg) id) };
    id
}

// riscv-rt parks every non-boot hart in a wfi loop by default; returning here lets
// hart 1 fall through to main() (true on hart 0 = it also does .bss/.data init).
#[export_name = "_mp_hook"]
pub extern "Rust" fn mp_hook(hartid: usize) -> bool {
    hartid == 0
}

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
const HTML: &[u8] = b"<!doctype html><html><head><title>K210 cam</title><meta name=viewport content=\"width=device-width,initial-scale=1\"></head><body style=\"background:#111;color:#eee;text-align:center;font-family:sans-serif\"><h2>K210 bare-metal Rust camera (UART WiFi)</h2><div><button id=\"r0\" onclick=\"S(0)\">QQVGA</button> <button id=\"r1\" onclick=\"S(1)\">QVGA</button> <button id=\"r2\" onclick=\"S(2)\">VGA</button></div><div style=\"margin-top:4px\"><button id=\"f0\" onclick=\"F(0)\">BMP</button> <button id=\"f1\" onclick=\"F(1)\">JPEG sw</button> <button id=\"f2\" onclick=\"F(2)\">JPEG cam</button> &nbsp; <button id=\"db\" onclick=\"D()\">Denoise OFF</button> <button id=\"eb\" onclick=\"E()\">RGB565 OFF</button> <button id=\"tb\" onclick=\"T()\">2-core OFF</button></div><div style=\"margin-top:8px\"><canvas id=\"c\" style=\"width:640px;max-width:98vw;image-rendering:pixelated\"></canvas></div><p id=\"s\">connecting...</p><div style=\"margin:6px\"><input id=\"u\" readonly style=\"width:90%;max-width:600px;background:#222;color:#9f9;border:1px solid #444;padding:4px;font-family:monospace\"> <button onclick=\"C()\">Copy URL</button></div><p>BMP=lossless RGB565 (RGB565 toggle: ESP32 expands, 33% less UART). JPEG sw=on-chip software encode (clean, ~10-20x smaller, any res). JPEG cam=OV2640 hardware codec (often colour-shifts on this board). Denoise=median+destripe. 2-core=both K210 cores encode halves in parallel (one RST marker, ~1.8x faster, JPEG sw only).</p><script>var r=1,fmt=0,d=0,e=0,t=0,n=0,t0=0;function g(i){return document.getElementById(i)}function url(){if(fmt==2)return location.origin+'/cam.jpg';var rs=['qqvga','qvga','vga'][r];if(fmt==1){var qj=[];if(d)qj.push('d=1');if(t)qj.push('2=1');return location.origin+'/'+rs+'.jpg'+(qj.length?'?'+qj.join('&'):'')}var q=[];if(d)q.push('d=1');if(e)q.push('e=1');return location.origin+'/'+rs+'.bmp'+(q.length?'?'+q.join('&'):'')}function upd(){var i;for(i=0;i<3;i++)g('r'+i).style.background=(fmt!=2&&r==i)?'#383':'';for(i=0;i<3;i++)g('f'+i).style.background=fmt==i?'#383':'';var db=g('db'),eb=g('eb'),dok=fmt!=2,eok=fmt==0;db.disabled=!dok;db.style.opacity=dok?1:.4;db.style.background=(dok&&d)?'#383':'#555';db.textContent='Denoise '+(d?'ON':'OFF');eb.disabled=!eok;eb.style.opacity=eok?1:.4;eb.style.background=(eok&&e)?'#383':'#555';eb.textContent='RGB565 '+(e?'ON':'OFF');var tb=g('tb'),tok=fmt==1;tb.disabled=!tok;tb.style.opacity=tok?1:.4;tb.style.background=(tok&&t)?'#383':'#555';tb.textContent='2-core '+(t?'ON':'OFF');g('u').value=url()}function C(){var u=g('u');u.focus();u.select();u.setSelectionRange(0,99999);try{document.execCommand('copy')}catch(z){}}function S(x){r=x;if(fmt==2)fmt=0;n=0;t0=0;upd()}function F(x){fmt=x;n=0;t0=0;upd()}function D(){if(fmt!=2){d=d?0:1;n=0;t0=0;upd()}}function E(){if(fmt==0){e=e?0:1;n=0;t0=0;upd()}}function T(){if(fmt==1){t=t?0:1;n=0;t0=0;upd()}}function L(){var im=new Image();im.onload=function(){var c=g('c');c.width=im.naturalWidth;c.height=im.naturalHeight;c.getContext('2d').drawImage(im,0,0);n++;var s=(fmt==2?'JPEGcam':['QQVGA','QVGA','VGA'][r]+(fmt==1?' JPEGsw':' BMP')+(d?' +dn':'')+(e&&fmt==0?' +565':'')+(t&&fmt==1?' x2':''))+'  frame '+n;if(n<2){t0=Date.now()}else{s+='  ('+((n-1)*1000/(Date.now()-t0)).toFixed(2)+' fps)'}g('s').textContent=s;setTimeout(L,40)};im.onerror=function(){setTimeout(L,800)};var u=url();im.src=u+(u.indexOf('?')<0?'?':'&')+'t='+Date.now()}upd();L()</script></body></html>";

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

/// Capture one frame into buffer `i` (blocking).
fn capture(dvp: &Dvp, i: usize) {
    dvp.set_display_addr(Some(cap_cached(i)));
    dvp.get_image();
}

/// Start a capture into buffer `i` (DMA runs in the background; finish with `capture_wait`).
fn capture_arm(dvp: &Dvp, i: usize) {
    dvp.set_display_addr(Some(cap_cached(i)));
    dvp.capture_arm();
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

// ---- software JPEG (encode the captured RGB565 frame on-chip) -------------------
// The camera's HARDWARE JPEG was a dead end on this board (DVP byte errors + no
// restart markers corrupt it; see esp32-modem/.. and the memory). Instead we encode
// JPEG in software from the already-captured (and optionally denoised) clean frame:
// the byte stream is computed on-chip and sent byte-exact over the flow-controlled
// UART, so it's always clean, any resolution, ~10-20x smaller than the BMP. See
// src/jpeg.rs (baseline 4:2:0 integer-DCT encoder).

/// Dual-core encode: hart 1 entropy-codes the bottom half of the MCU rows into CAP[2]
/// (uncached) while hart 0 writes the headers + top half into CAP[1] (uncached); the two
/// segments are joined by one RST0 restart marker (DRI in the header). Returns the JPEG
/// length in CAP[1], or None on overflow. Halving the dominant cost (entropy + DCT) is
/// the win; the restart marker keeps the output a standard JPEG.
fn encode_dual(w: usize, h: usize, src: usize, out: &mut [u8]) -> Option<usize> {
    let mcx = (w + 15) / 16;
    let mcy = (h + 15) / 16;
    let mcy_top = (mcy + 1) / 2; // hart 0 does [0, mcy_top); hart 1 does [mcy_top, mcy)
    let ri = (mcx * mcy_top) as u16; // restart interval = MCUs in the first segment

    // Flush so no stale dirty line of hart 0's (incl. a prior single-core cached copy in
    // CAP[2]) can flush over hart 1's uncached output mid-job, then post the job.
    flush_l1();
    sst(S_FB, cap_uncached(src) as usize as u32);
    sst(S_W, w as u32);
    sst(S_H, h as u32);
    sst(S_MCY0, mcy_top as u32);
    sst(S_MCY1, mcy as u32);
    sst(S_OUT, cap_uncached(2) as usize as u32);
    sst(S_OUTCAP, (MAXW * MAXH / 2 * 4) as u32);
    sst(S_LEN, 0);
    sst(S_OVF, 0);
    fence();
    sst(S_STATE, 1); // go, hart 1
    fence();

    // hart 0: headers (with DRI) + top segment into CAP[1].
    let start = jpeg::write_headers(out, w, h, ri);
    let len0 = jpeg::encode_segment(cap_uncached(src) as *const u32, w, h, 0, mcy_top, &mut out[start..])?;
    let mut p = start + len0;
    out[p] = 0xFF;
    out[p + 1] = 0xD0; // RST0 between the two segments
    p += 2;

    // Wait for hart 1, then append its segment + EOI.
    while sld(S_STATE) != 2 {
        core::hint::spin_loop();
    }
    fence();
    if sld(S_OVF) != 0 {
        sst(S_STATE, 0);
        return None;
    }
    let len1 = sld(S_LEN) as usize;
    if p + len1 + 2 > out.len() {
        sst(S_STATE, 0);
        return None;
    }
    let src = unsafe { core::slice::from_raw_parts(cap_uncached(2) as *const u8, len1) };
    out[p..p + len1].copy_from_slice(src);
    p += len1;
    out[p] = 0xFF;
    out[p + 1] = 0xD9; // EOI
    p += 2;
    sst(S_STATE, 0); // idle, ready for the next job
    Some(p)
}

/// Encode CAP[0] (w x h RGB565) to JPEG into CAP[1] (free, 614 KB) and serve it as
/// image/jpeg. `dual` runs the two-core split encoder. Returns (ok, jpeg_len, enc_ms).
fn serve_swjpeg(w: usize, h: usize, src: usize, dual: bool, reply: &mut [u8]) -> (bool, usize, u64) {
    let out = unsafe {
        core::slice::from_raw_parts_mut(cap_uncached(1) as *mut u8, MAXW * MAXH / 2 * 4)
    };
    let t_enc = mtime_ms();
    let len = if dual {
        match encode_dual(w, h, src, out) {
            Some(n) => n,
            None => return (false, 0, 0),
        }
    } else {
        // Copy the frame uncached(src) -> cached(2) once. The encoder reads each pixel ~2x
        // (luma + chroma); reading from the cached alias makes those hit L1 instead of
        // paying the uncached-AXI latency on every access. One sequential copy (W*H/2 word
        // reads) is far cheaper than 2*W*H scattered uncached reads.
        let words = w * h / 2;
        unsafe {
            let s = core::slice::from_raw_parts(cap_uncached(src) as *const u32, words);
            let dst = core::slice::from_raw_parts_mut(cap_cached(2) as *mut u32, words);
            dst.copy_from_slice(s);
        }
        match jpeg::encode(cap_cached(2) as *const u32, w, h, out) {
            Some(n) => n,
            None => return (false, 0, 0),
        }
    };
    let enc_ms = mtime_ms().wrapping_sub(t_enc);
    let mut hdr = [0u8; 96];
    let mut n = 0;
    n += append(&mut hdr, n, b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: ");
    n += write_dec(&mut hdr[n..], len as u32);
    n += append(&mut hdr, n, b"\r\nConnection: close\r\n\r\n");
    if !send_all(uart_wifi::CMD_SEND, &hdr[..n], reply) {
        return (false, len, enc_ms);
    }
    let ok = send_all(uart_wifi::CMD_SEND, &out[..len], reply);
    (ok, len, enc_ms)
}

// ---- camera HARDWARE JPEG (OV2640) -- kept as a selectable curiosity (/cam.jpg) ----
// Mostly corrupted on this board (DVP byte errors + the OV2640 emits no restart
// markers -> a byte error on a DC coeff tints the whole frame; ~30% of frames clean).
// The software encoder (serve_swjpeg) is the clean path; this is for comparison.
const JCAP_WORDS: usize = MAXW * MAXH / 2;

fn configure_jpeg(dvp: &Dvp) {
    dvp::ov2640_jpeg_qvga(dvp);
    dvp.set_image_format(ImageFormat::RGB);
    dvp.set_image_size(false, MAXW as u16, MAXH as u16);
    let t_end = mtime_ms() + 2000;
    while mtime_ms() < t_end {
        capture(dvp, 0);
    }
}

/// Capture the OV2640 hardware JPEG into CAP[0], byte-swap to a contiguous stream, find
/// SOI..EOI, serve as image/jpeg. Returns (found, len).
fn serve_camjpeg(dvp: &Dvp, reply: &mut [u8]) -> (bool, usize) {
    capture(dvp, 0);
    let buf = cap_uncached(0);
    for j in 0..JCAP_WORDS {
        unsafe {
            let wv = core::ptr::read_volatile(buf.add(j));
            core::ptr::write_volatile(buf.add(j), wv.swap_bytes());
        }
    }
    let bytes = cap_uncached(0) as *const u8;
    let nb = JCAP_WORDS * 4;
    let rd = |i: usize| -> u8 { unsafe { core::ptr::read_volatile(bytes.add(i)) } };
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

// Distinctive "go" value for the ready gate: power-on SRAM garbage is very unlikely to
// match it, so hart 1 won't false-start on an uninitialized read.
const READY_MAGIC: u32 = 0xC0DE_CAFE;

/// Evict hart 0's entire L1 by reading a buffer several times its size, forcing every
/// dirty line to be written back to physical SRAM. The two cores have no cache coherency,
/// so this guarantees hart 0's cached writes (boot-time .bss zero of SHARED, a prior
/// single-core cached frame copy in CAP[2]) reach SRAM before hart 1 reads them uncached
/// and can't later flush over hart 1's uncached writes. Reading 128 KiB sequentially
/// touches every cache set many times over.
fn flush_l1() {
    let p = cap_cached(0) as *const u32;
    let mut acc = 0u32;
    let mut i = 0;
    while i < 32 * 1024 {
        acc = acc.wrapping_add(unsafe { core::ptr::read_volatile(p.add(i)) });
        i += 1;
    }
    sst(S_SINK, acc | 1); // sink the read so the compiler can't drop the loop
}

/// Hart 1 entry. Must NOT touch any peripheral hart 0 owns (it never calls
/// `Peripherals::take`). Waits for the ready gate, then services JPEG segment jobs: each
/// job entropy-codes an MCU-row range of the source frame into a caller-given buffer.
/// All source reads and output writes go through uncached pointers hart 0 hands over.
fn core1_main() -> ! {
    // Clear any stale job state left in SRAM by a previous boot before the gate, so a
    // warm-reboot can't fool us into running a job with garbage params. (SRAM persists
    // across reset, so READY may still hold the magic from last run — harmless: we just
    // re-enter the idle loop and wait for hart 0 to post a fresh job.)
    sst(S_STATE, 0);
    while sld(S_READY) != READY_MAGIC {
        core::hint::spin_loop();
    }
    let mut tick = 0u32;
    loop {
        // Periodically assert liveness so hart 0's readiness poll sees us even if its
        // boot-time cached zero of SHARED flushes over an earlier write. Throttled so the
        // idle uncached writes don't contend with hart 0 on the bus.
        tick = tick.wrapping_add(1);
        if tick & 0x3FFF == 0 {
            sst(S_HEARTBEAT, 1);
        }
        if sld(S_STATE) != 1 {
            core::hint::spin_loop();
            continue;
        }
        fence();
        let fb = sld(S_FB) as usize as *const u32;
        let w = sld(S_W) as usize;
        let h = sld(S_H) as usize;
        let mcy0 = sld(S_MCY0) as usize;
        let mcy1 = sld(S_MCY1) as usize;
        let out = unsafe {
            core::slice::from_raw_parts_mut(sld(S_OUT) as usize as *mut u8, sld(S_OUTCAP) as usize)
        };
        match jpeg::encode_segment(fb, w, h, mcy0, mcy1, out) {
            Some(n) => {
                sst(S_LEN, n as u32);
                sst(S_OVF, 0);
            }
            None => {
                sst(S_LEN, 0);
                sst(S_OVF, 1);
            }
        }
        fence();
        sst(S_STATE, 2); // done
    }
}

#[entry]
fn main() -> ! {
    if hartid() != 0 {
        core1_main();
    }
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
    // Capture pipeline: -1 = no capture in flight; else the CAP index (0 or 3) whose DMA
    // is filling in the background, armed with `pipe_res`. The DVP capture (~540 ms at VGA)
    // dwarfs encode+send (~250 ms), so we overlap them: while encoding+sending frame N we
    // let the DVP fill the other ping-pong buffer with N+1.
    let mut pipe_arm: i32 = -1;
    let mut pipe_res = cur_res;
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

    // Release hart 1 now that init is done. Flush first so the boot-time cached zero of
    // SHARED has reached physical SRAM and can't clobber the magic we write uncached.
    sst(S_STATE, 0);
    flush_l1();
    sst(S_READY, READY_MAGIC);
    fence();
    let mut alive = false;
    for _ in 0..50 {
        if sld(S_HEARTBEAT) == 1 {
            alive = true;
            break;
        }
        uart_wifi::sleep_ms(10);
    }
    puts(if alive { b"core1 ready\n" } else { b"core1 NOT ready\n" });

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
        // /cam.jpg = camera HARDWARE JPEG; /{res}.jpg = SOFTWARE JPEG; /{res}.bmp = BMP.
        let is_camjpg = contains(rs, b"cam.jpg") || flag(rs, b"?j=", b"&j=") == 1;
        let is_swjpg = !is_camjpg && contains(rs, b".jpg");
        let is_bmp = contains(rs, b".bmp");

        if is_camjpg {
            // ---- camera HARDWARE JPEG (/cam.jpg) ----
            if pipe_arm >= 0 {
                dvp.capture_wait(); // drain any in-flight RGB capture before reconfiguring
                pipe_arm = -1;
            }
            if !cur_jpeg {
                configure_jpeg(&dvp);
                cur_jpeg = true;
            }
            let start = mtime_ms();
            let (found, len) = serve_camjpeg(&dvp, &mut reply);
            let ms = mtime_ms().wrapping_sub(start);
            frame_no += 1;
            puts(b"frame ");
            put_dec(frame_no);
            puts(if found { b" CAMJPG " } else { b" CAMJPG-FAIL " });
            put_dec(len as u32);
            puts(b"B ");
            put_dec(ms as u32);
            puts(b"ms\n");
        } else if is_swjpg || is_bmp {
            if cur_jpeg {
                if pipe_arm >= 0 {
                    dvp.capture_wait();
                    pipe_arm = -1;
                }
                let (nw, nh) = configure_res(&dvp, cur_res); // back to RGB output
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
            let e = flag(rs, b"?e=", b"&e="); // RGB565-direct (BMP only)
            let two = flag(rs, b"?2=", b"&2="); // dual-core encode (JPEG sw only)
            if r != cur_res {
                if pipe_arm >= 0 {
                    dvp.capture_wait(); // a re-config does its own captures; drain ours first
                    pipe_arm = -1;
                }
                let (nw, nh) = configure_res(&dvp, r); // ~185 SCCB writes + warm-up
                w = nw;
                h = nh;
                cur_res = r;
            }
            // Pick the source buffer for this frame. The JPEG-sw + denoise-off path runs
            // the capture pipeline (overlap next capture with this encode+send); every
            // other path drains the pipeline and captures synchronously.
            let src;
            let tc = mtime_ms();
            if d == 1 {
                if pipe_arm >= 0 {
                    dvp.capture_wait();
                    pipe_arm = -1;
                }
                capture(&dvp, 0);
                capture(&dvp, 1);
                capture(&dvp, 2);
                denoise_median(w, h);
                destripe(w, h);
                src = 0;
            } else if is_swjpg {
                // pipeline: the buffer armed last iteration holds this frame.
                if pipe_arm >= 0 && pipe_res == cur_res {
                    dvp.capture_wait();
                    src = pipe_arm as usize;
                } else {
                    if pipe_arm >= 0 {
                        dvp.capture_wait();
                    }
                    capture(&dvp, 0);
                    src = 0;
                }
                // arm the next capture into the other ping-pong buffer so its DMA runs
                // during this frame's encode+send.
                let nxt = if src == 0 { 3 } else { 0 };
                capture_arm(&dvp, nxt);
                pipe_arm = nxt as i32;
                pipe_res = cur_res;
            } else {
                // BMP: synchronous, no pipeline.
                if pipe_arm >= 0 {
                    dvp.capture_wait();
                    pipe_arm = -1;
                }
                capture(&dvp, 0);
                src = 0;
            }
            let cap_ms = mtime_ms().wrapping_sub(tc);
            puts(b"[cap ");
            put_dec(cap_ms as u32);
            puts(b"ms] ");
            frame_no += 1;
            let start = mtime_ms();
            if is_swjpg {
                let (ok, len, enc_ms) = serve_swjpeg(w, h, src, two == 1, &mut reply); // CAP[1]=output
                let ms = mtime_ms().wrapping_sub(start);
                puts(b"frame ");
                put_dec(frame_no);
                puts(b" r");
                put_dec(cur_res as u32);
                puts(b" d");
                put_dec(d as u32);
                puts(if two == 1 { b" x2" } else { b" x1" });
                puts(if ok { b" SWJPG " } else { b" SWJPG-FAIL " });
                put_dec(len as u32);
                puts(b"B enc");
                put_dec(enc_ms as u32);
                puts(b"ms tot");
                put_dec(ms as u32);
                puts(b"ms\n");
            } else {
                unsafe { DBG_SENT = 0 };
                let ok = serve_bmp(w, h, e == 1, &mut reply);
                let ms = mtime_ms().wrapping_sub(start);
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
            }
        } else {
            let mut resp = [0u8; 4096];
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
