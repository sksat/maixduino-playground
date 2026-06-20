//! Minimal baseline JPEG encoder (4:2:0, integer DCT), no_std, no alloc.
//!
//! Encodes an RGB565 frame into a caller-provided buffer as a JFIF JPEG. Standard quant
//! (quality 80) + standard Huffman tables, a fixed-point 8x8 DCT (libjpeg's LL&M/Loeffler
//! `jpeg_fdct_islow`, 12 mults/pass), reciprocal quantization (multiply+shift, no divides), no restart
//! markers (the byte stream is clean: computed on-chip and sent byte-exact over the
//! flow-controlled UART). The point is to shrink what crosses the UART (the bottleneck)
//! ~10-20x; the browser decodes natively. The caller copies the frame into a cached
//! buffer first, so the ~2 reads/pixel here hit L1 rather than the uncached AXI alias.
//!
//! Tables (quant/DCT/Huffman/DHT) are generated; see the git history for the script.

#![allow(dead_code)]

use core::ptr::read_volatile;

// ---- generated tables (quality 80) -------------------------------------------------
static QUANT_L: [u8; 64] = [6,4,4,6,10,16,20,24,5,5,6,8,10,23,24,22,6,5,6,10,16,23,28,22,6,7,9,12,20,35,32,25,7,9,15,22,27,44,41,31,10,14,22,26,32,42,45,37,20,26,31,35,41,48,48,40,29,37,38,39,45,40,41,40];
static QUANT_C: [u8; 64] = [7,7,10,19,40,40,40,40,7,8,10,26,40,40,40,40,10,10,22,40,40,40,40,40,19,26,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40,40];
static ZIGZAG: [u8; 64] = [0,1,8,16,9,2,3,10,17,24,32,25,18,11,4,5,12,19,26,33,40,48,41,34,27,20,13,6,7,14,21,28,35,42,49,56,57,50,43,36,29,22,15,23,30,37,44,51,58,59,52,45,38,31,39,46,53,60,61,54,47,55,62,63];
// Loeffler-Ligtenberg-Moschytz fixed-point DCT constants (libjpeg jpeg_fdct_islow,
// CONST_BITS=13): FIX(c) = round(c * 2^13). 12 mults/8-point vs 32 for the direct sum.
const C_BITS: i32 = 13;
const P1_BITS: i32 = 2;
const F_0_298631336: i32 = 2446;
const F_0_390180644: i32 = 3196;
const F_0_541196100: i32 = 4433;
const F_0_765366865: i32 = 6270;
const F_0_899976223: i32 = 7373;
const F_1_175875602: i32 = 9633;
const F_1_501321110: i32 = 12299;
const F_1_847759065: i32 = 15137;
const F_1_961570560: i32 = 16069;
const F_2_053119869: i32 = 16819;
const F_2_562915447: i32 = 20995;
const F_3_072711026: i32 = 25172;
static DC_L: [(u16,u8); 12] = [(0,2),(2,3),(3,3),(4,3),(5,3),(6,3),(14,4),(30,5),(62,6),(126,7),(254,8),(510,9)];
static DC_C: [(u16,u8); 12] = [(0,2),(1,2),(2,2),(6,3),(14,4),(30,5),(62,6),(126,7),(254,8),(510,9),(1022,10),(2046,11)];
static AC_L: [(u16,u8); 251] = [(10,4),(0,2),(1,2),(4,3),(11,4),(26,5),(120,7),(248,8),(1014,10),(65410,16),(65411,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(12,4),(27,5),(121,7),(502,9),(2038,11),(65412,16),(65413,16),(65414,16),(65415,16),(65416,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(28,5),(249,8),(1015,10),(4084,12),(65417,16),(65418,16),(65419,16),(65420,16),(65421,16),(65422,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(58,6),(503,9),(4085,12),(65423,16),(65424,16),(65425,16),(65426,16),(65427,16),(65428,16),(65429,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(59,6),(1016,10),(65430,16),(65431,16),(65432,16),(65433,16),(65434,16),(65435,16),(65436,16),(65437,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(122,7),(2039,11),(65438,16),(65439,16),(65440,16),(65441,16),(65442,16),(65443,16),(65444,16),(65445,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(123,7),(4086,12),(65446,16),(65447,16),(65448,16),(65449,16),(65450,16),(65451,16),(65452,16),(65453,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(250,8),(4087,12),(65454,16),(65455,16),(65456,16),(65457,16),(65458,16),(65459,16),(65460,16),(65461,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(504,9),(32704,15),(65462,16),(65463,16),(65464,16),(65465,16),(65466,16),(65467,16),(65468,16),(65469,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(505,9),(65470,16),(65471,16),(65472,16),(65473,16),(65474,16),(65475,16),(65476,16),(65477,16),(65478,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(506,9),(65479,16),(65480,16),(65481,16),(65482,16),(65483,16),(65484,16),(65485,16),(65486,16),(65487,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(1017,10),(65488,16),(65489,16),(65490,16),(65491,16),(65492,16),(65493,16),(65494,16),(65495,16),(65496,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(1018,10),(65497,16),(65498,16),(65499,16),(65500,16),(65501,16),(65502,16),(65503,16),(65504,16),(65505,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(2040,11),(65506,16),(65507,16),(65508,16),(65509,16),(65510,16),(65511,16),(65512,16),(65513,16),(65514,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(65515,16),(65516,16),(65517,16),(65518,16),(65519,16),(65520,16),(65521,16),(65522,16),(65523,16),(65524,16),(0,0),(0,0),(0,0),(0,0),(0,0),(2041,11),(65525,16),(65526,16),(65527,16),(65528,16),(65529,16),(65530,16),(65531,16),(65532,16),(65533,16),(65534,16)];
static AC_C: [(u16,u8); 251] = [(0,2),(1,2),(4,3),(10,4),(24,5),(25,5),(56,6),(120,7),(500,9),(1014,10),(4084,12),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(11,4),(57,6),(246,8),(501,9),(2038,11),(4085,12),(65416,16),(65417,16),(65418,16),(65419,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(26,5),(247,8),(1015,10),(4086,12),(32706,15),(65420,16),(65421,16),(65422,16),(65423,16),(65424,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(27,5),(248,8),(1016,10),(4087,12),(65425,16),(65426,16),(65427,16),(65428,16),(65429,16),(65430,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(58,6),(502,9),(65431,16),(65432,16),(65433,16),(65434,16),(65435,16),(65436,16),(65437,16),(65438,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(59,6),(1017,10),(65439,16),(65440,16),(65441,16),(65442,16),(65443,16),(65444,16),(65445,16),(65446,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(121,7),(2039,11),(65447,16),(65448,16),(65449,16),(65450,16),(65451,16),(65452,16),(65453,16),(65454,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(122,7),(2040,11),(65455,16),(65456,16),(65457,16),(65458,16),(65459,16),(65460,16),(65461,16),(65462,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(249,8),(65463,16),(65464,16),(65465,16),(65466,16),(65467,16),(65468,16),(65469,16),(65470,16),(65471,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(503,9),(65472,16),(65473,16),(65474,16),(65475,16),(65476,16),(65477,16),(65478,16),(65479,16),(65480,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(504,9),(65481,16),(65482,16),(65483,16),(65484,16),(65485,16),(65486,16),(65487,16),(65488,16),(65489,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(505,9),(65490,16),(65491,16),(65492,16),(65493,16),(65494,16),(65495,16),(65496,16),(65497,16),(65498,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(506,9),(65499,16),(65500,16),(65501,16),(65502,16),(65503,16),(65504,16),(65505,16),(65506,16),(65507,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(2041,11),(65508,16),(65509,16),(65510,16),(65511,16),(65512,16),(65513,16),(65514,16),(65515,16),(65516,16),(0,0),(0,0),(0,0),(0,0),(0,0),(0,0),(16352,14),(65517,16),(65518,16),(65519,16),(65520,16),(65521,16),(65522,16),(65523,16),(65524,16),(65525,16),(0,0),(0,0),(0,0),(0,0),(0,0),(1018,10),(32707,15),(65526,16),(65527,16),(65528,16),(65529,16),(65530,16),(65531,16),(65532,16),(65533,16),(65534,16)];
static DHT_DC_L_BITS: [u8; 16] = [0,1,5,1,1,1,1,1,1,0,0,0,0,0,0,0];
static DHT_DC_L_VAL: [u8; 12] = [0,1,2,3,4,5,6,7,8,9,10,11];
static DHT_AC_L_BITS: [u8; 16] = [0,2,1,3,3,2,4,3,5,5,4,4,0,0,1,125];
static DHT_AC_L_VAL: [u8; 162] = [1,2,3,0,4,17,5,18,33,49,65,6,19,81,97,7,34,113,20,50,129,145,161,8,35,66,177,193,21,82,209,240,36,51,98,114,130,9,10,22,23,24,25,26,37,38,39,40,41,42,52,53,54,55,56,57,58,67,68,69,70,71,72,73,74,83,84,85,86,87,88,89,90,99,100,101,102,103,104,105,106,115,116,117,118,119,120,121,122,131,132,133,134,135,136,137,138,146,147,148,149,150,151,152,153,154,162,163,164,165,166,167,168,169,170,178,179,180,181,182,183,184,185,186,194,195,196,197,198,199,200,201,202,210,211,212,213,214,215,216,217,218,225,226,227,228,229,230,231,232,233,234,241,242,243,244,245,246,247,248,249,250];
static DHT_DC_C_BITS: [u8; 16] = [0,3,1,1,1,1,1,1,1,1,1,0,0,0,0,0];
static DHT_DC_C_VAL: [u8; 12] = [0,1,2,3,4,5,6,7,8,9,10,11];
static DHT_AC_C_BITS: [u8; 16] = [0,2,1,2,4,4,3,4,7,5,4,4,0,1,2,119];
static DHT_AC_C_VAL: [u8; 162] = [0,1,2,3,17,4,5,33,49,6,18,65,81,7,97,113,19,34,50,129,8,20,66,145,161,177,193,9,35,51,82,240,21,98,114,209,10,22,36,52,225,37,241,23,24,25,26,38,39,40,41,42,53,54,55,56,57,58,67,68,69,70,71,72,73,74,83,84,85,86,87,88,89,90,99,100,101,102,103,104,105,106,115,116,117,118,119,120,121,122,130,131,132,133,134,135,136,137,138,146,147,148,149,150,151,152,153,154,162,163,164,165,166,167,168,169,170,178,179,180,181,182,183,184,185,186,194,195,196,197,198,199,200,201,202,210,211,212,213,214,215,216,217,218,226,227,228,229,230,231,232,233,234,242,243,244,245,246,247,248,249,250];

// ---- bit writer with FF byte-stuffing ----------------------------------------------
struct BitW<'a> {
    buf: &'a mut [u8],
    pos: usize,
    acc: u32,
    n: u32,
    overflow: bool,
}
impl<'a> BitW<'a> {
    fn byte(&mut self, b: u8) {
        if self.pos < self.buf.len() {
            self.buf[self.pos] = b;
        } else {
            self.overflow = true;
        }
        self.pos += 1;
    }
    fn put(&mut self, code: u32, size: u32) {
        if size == 0 {
            return;
        }
        self.acc |= (code & ((1u32 << size) - 1)) << (32 - self.n - size);
        self.n += size;
        while self.n >= 8 {
            let b = (self.acc >> 24) as u8;
            self.byte(b);
            if b == 0xFF {
                self.byte(0); // stuff a zero after every 0xFF in entropy data
            }
            self.acc <<= 8;
            self.n -= 8;
        }
    }
    fn flush(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1u32 << pad) - 1, pad); // pad with 1-bits to a byte boundary
        }
    }
}

/// Magnitude category + the value bits for a (signed) coefficient.
fn cat(v: i32) -> (u32, u32) {
    if v == 0 {
        return (0, 0);
    }
    let mut t = if v < 0 { -v } else { v };
    let mut size = 0u32;
    while t > 0 {
        size += 1;
        t >>= 1;
    }
    let bits = if v >= 0 { v as u32 } else { (v - 1) as u32 };
    (size, bits & ((1u32 << size) - 1))
}

#[inline]
fn descale(x: i32, n: i32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

/// In-place forward 8x8 DCT (libjpeg's LL&M/Loeffler `jpeg_fdct_islow`): a column pass
/// then a row pass, each the same 1D flowgraph (3 even + 9 odd = 12 mults). Pass 1 keeps
/// `P1_BITS` extra fraction bits for accuracy; pass 2 removes them. The output is the
/// JPEG-normalized DCT scaled by 8 (the factor is divided out in `code_block`'s quant).
fn fdct(b: &mut [i32; 64]) {
    // Pass 1: columns (stride 8), results left at scale 2^P1_BITS.
    for c in 0..8 {
        let (d0, d1, d2, d3) = (b[c], b[8 + c], b[16 + c], b[24 + c]);
        let (d4, d5, d6, d7) = (b[32 + c], b[40 + c], b[48 + c], b[56 + c]);
        let (tmp0, tmp7) = (d0 + d7, d0 - d7);
        let (tmp1, tmp6) = (d1 + d6, d1 - d6);
        let (tmp2, tmp5) = (d2 + d5, d2 - d5);
        let (tmp3, tmp4) = (d3 + d4, d3 - d4);
        let (tmp10, tmp13) = (tmp0 + tmp3, tmp0 - tmp3);
        let (tmp11, tmp12) = (tmp1 + tmp2, tmp1 - tmp2);
        b[c] = (tmp10 + tmp11) << P1_BITS;
        b[32 + c] = (tmp10 - tmp11) << P1_BITS;
        let z1 = (tmp12 + tmp13) * F_0_541196100;
        b[16 + c] = descale(z1 + tmp13 * F_0_765366865, C_BITS - P1_BITS);
        b[48 + c] = descale(z1 - tmp12 * F_1_847759065, C_BITS - P1_BITS);
        let (p1, p2) = (tmp4 + tmp7, tmp5 + tmp6);
        let (p3, p4) = (tmp4 + tmp6, tmp5 + tmp7);
        let z5 = (p3 + p4) * F_1_175875602;
        let q1 = p1 * -F_0_899976223;
        let q2 = p2 * -F_2_562915447;
        let q3 = p3 * -F_1_961570560 + z5;
        let q4 = p4 * -F_0_390180644 + z5;
        b[56 + c] = descale(tmp4 * F_0_298631336 + q1 + q3, C_BITS - P1_BITS);
        b[40 + c] = descale(tmp5 * F_2_053119869 + q2 + q4, C_BITS - P1_BITS);
        b[24 + c] = descale(tmp6 * F_3_072711026 + q2 + q3, C_BITS - P1_BITS);
        b[8 + c] = descale(tmp7 * F_1_501321110 + q1 + q4, C_BITS - P1_BITS);
    }
    // Pass 2: rows (stride 1), remove the P1_BITS added in pass 1.
    for r in 0..8 {
        let o = r * 8;
        let (d0, d1, d2, d3) = (b[o], b[o + 1], b[o + 2], b[o + 3]);
        let (d4, d5, d6, d7) = (b[o + 4], b[o + 5], b[o + 6], b[o + 7]);
        let (tmp0, tmp7) = (d0 + d7, d0 - d7);
        let (tmp1, tmp6) = (d1 + d6, d1 - d6);
        let (tmp2, tmp5) = (d2 + d5, d2 - d5);
        let (tmp3, tmp4) = (d3 + d4, d3 - d4);
        let (tmp10, tmp13) = (tmp0 + tmp3, tmp0 - tmp3);
        let (tmp11, tmp12) = (tmp1 + tmp2, tmp1 - tmp2);
        b[o] = descale(tmp10 + tmp11, P1_BITS);
        b[o + 4] = descale(tmp10 - tmp11, P1_BITS);
        let z1 = (tmp12 + tmp13) * F_0_541196100;
        b[o + 2] = descale(z1 + tmp13 * F_0_765366865, C_BITS + P1_BITS);
        b[o + 6] = descale(z1 - tmp12 * F_1_847759065, C_BITS + P1_BITS);
        let (p1, p2) = (tmp4 + tmp7, tmp5 + tmp6);
        let (p3, p4) = (tmp4 + tmp6, tmp5 + tmp7);
        let z5 = (p3 + p4) * F_1_175875602;
        let q1 = p1 * -F_0_899976223;
        let q2 = p2 * -F_2_562915447;
        let q3 = p3 * -F_1_961570560 + z5;
        let q4 = p4 * -F_0_390180644 + z5;
        b[o + 7] = descale(tmp4 * F_0_298631336 + q1 + q3, C_BITS + P1_BITS);
        b[o + 5] = descale(tmp5 * F_2_053119869 + q2 + q4, C_BITS + P1_BITS);
        b[o + 3] = descale(tmp6 * F_3_072711026 + q2 + q3, C_BITS + P1_BITS);
        b[o + 1] = descale(tmp7 * F_1_501321110 + q1 + q4, C_BITS + P1_BITS);
    }
}

/// Quantize a DCT'd block, then Huffman-code DC (differential) + AC (run/size).
fn code_block(
    coef: &[i32; 64],
    recip: &[i32; 64], // round(2^16 / quant[i]); quantize by multiply + shift, no divides
    dc: &[(u16, u8); 12],
    ac: &[(u16, u8); 251],
    prev_dc: &mut i32,
    bw: &mut BitW,
) {
    let mut qz = [0i32; 64];
    for i in 0..64 {
        let c = coef[i];
        let a = (if c < 0 { -c } else { c }) as i64;
        // coef is 8x the DCT (the islow scale), so divide by 8*quant: |c| * (2^16/quant)
        // >> 19 = |c| / (8*quant). i64 product avoids overflow on the large DC term.
        let q = ((a * recip[i] as i64 + (1 << 18)) >> 19) as i32;
        qz[i] = if c < 0 { -q } else { q };
    }
    // DC: differential
    let diff = qz[0] - *prev_dc;
    *prev_dc = qz[0];
    let (s, bits) = cat(diff);
    let (code, len) = dc[s as usize];
    bw.put(code as u32, len as u32);
    bw.put(bits, s);
    // AC: zigzag run-length
    let mut run = 0u32;
    for k in 1..64 {
        let c = qz[ZIGZAG[k] as usize];
        if c == 0 {
            run += 1;
        } else {
            while run > 15 {
                let (cc, ll) = ac[0xF0]; // ZRL (16 zeros)
                bw.put(cc as u32, ll as u32);
                run -= 16;
            }
            let (s, bits) = cat(c);
            let (cc, ll) = ac[((run << 4) | s) as usize];
            bw.put(cc as u32, ll as u32);
            bw.put(bits, s);
            run = 0;
        }
    }
    if run > 0 {
        let (cc, ll) = ac[0x00]; // EOB
        bw.put(cc as u32, ll as u32);
    }
}

/// RGB888 of source pixel (x, y), clamped to the image (for edge padding to 16).
#[inline]
fn rgb(fb: *const u32, w: usize, h: usize, x: usize, y: usize) -> (i32, i32, i32) {
    let xx = if x < w { x } else { w - 1 };
    let yy = if y < h { y } else { h - 1 };
    let i = yy * w + xx;
    let word = unsafe { read_volatile(fb.add(i >> 1)) };
    let px = if i & 1 == 0 { word & 0xffff } else { word >> 16 };
    let r5 = (px >> 11) & 0x1f;
    let g6 = (px >> 5) & 0x3f;
    let b5 = px & 0x1f;
    (
        ((r5 << 3) | (r5 >> 2)) as i32,
        ((g6 << 2) | (g6 >> 4)) as i32,
        ((b5 << 3) | (b5 >> 2)) as i32,
    )
}

// Header byte appends ---------------------------------------------------------------
fn w8(out: &mut [u8], p: &mut usize, b: u8) {
    if *p < out.len() {
        out[*p] = b;
    }
    *p += 1;
}
fn seg(out: &mut [u8], p: &mut usize, marker: u8, body: &[u8]) {
    w8(out, p, 0xFF);
    w8(out, p, marker);
    let len = body.len() + 2;
    w8(out, p, (len >> 8) as u8);
    w8(out, p, len as u8);
    for &b in body {
        w8(out, p, b);
    }
}

/// Write the JFIF headers up to (and including) SOS. `ri` is the restart interval in
/// MCUs (0 = no DRI segment / no restart markers). Returns the byte offset where the
/// entropy-coded scan begins.
pub fn write_headers(out: &mut [u8], w: usize, h: usize, ri: u16) -> usize {
    let mut p = 0usize;
    w8(out, &mut p, 0xFF);
    w8(out, &mut p, 0xD8); // SOI
    seg(out, &mut p, 0xE0, &[b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]); // APP0/JFIF
    if ri != 0 {
        seg(out, &mut p, 0xDD, &[(ri >> 8) as u8, ri as u8]); // DRI (restart interval)
    }
    // DQT luma (id 0) + chroma (id 1), each in zigzag order
    let mut qz = [0u8; 65];
    qz[0] = 0x00;
    for k in 0..64 {
        qz[k + 1] = QUANT_L[ZIGZAG[k] as usize];
    }
    seg(out, &mut p, 0xDB, &qz);
    qz[0] = 0x01;
    for k in 0..64 {
        qz[k + 1] = QUANT_C[ZIGZAG[k] as usize];
    }
    seg(out, &mut p, 0xDB, &qz);
    // SOF0 baseline, 4:2:0
    let sof = [
        8,
        (h >> 8) as u8, h as u8,
        (w >> 8) as u8, w as u8,
        3,
        1, 0x22, 0, // Y: 2x2 sampling, quant 0
        2, 0x11, 1, // Cb
        3, 0x11, 1, // Cr
    ];
    seg(out, &mut p, 0xC0, &sof);
    // DHT x4 (DC luma, AC luma, DC chroma, AC chroma)
    write_dht(out, &mut p, 0x00, &DHT_DC_L_BITS, &DHT_DC_L_VAL);
    write_dht(out, &mut p, 0x10, &DHT_AC_L_BITS, &DHT_AC_L_VAL);
    write_dht(out, &mut p, 0x01, &DHT_DC_C_BITS, &DHT_DC_C_VAL);
    write_dht(out, &mut p, 0x11, &DHT_AC_C_BITS, &DHT_AC_C_VAL);
    // SOS
    let sos = [3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 0x3F, 0];
    seg(out, &mut p, 0xDA, &sos);
    p
}

fn write_dht(out: &mut [u8], p: &mut usize, cls: u8, bits: &[u8; 16], vals: &[u8]) {
    w8(out, p, 0xFF);
    w8(out, p, 0xC4);
    let len = 2 + 1 + 16 + vals.len();
    w8(out, p, (len >> 8) as u8);
    w8(out, p, len as u8);
    w8(out, p, cls);
    for &b in bits {
        w8(out, p, b);
    }
    for &b in vals {
        w8(out, p, b);
    }
}

/// Entropy-code MCU rows `[mcy0, mcy1)` of the `w`x`h` RGB565 frame at `fb` into `out`
/// (which is just the scan region, written from offset 0). DC predictors start at 0, so
/// the output is a self-contained restart segment; it is flushed to a byte boundary.
/// Returns the byte length, or None on overflow. Splitting an image into row-ranges and
/// concatenating their segments with RST markers (see `encode` for single-segment) lets
/// the two cores encode halves in parallel.
pub fn encode_segment(
    fb: *const u32,
    w: usize,
    h: usize,
    mcy0: usize,
    mcy1: usize,
    out: &mut [u8],
) -> Option<usize> {
    let cap = out.len();
    let mut bw = BitW { buf: out, pos: 0, acc: 0, n: 0, overflow: false };
    let (mut dc_y, mut dc_cb, mut dc_cr) = (0i32, 0i32, 0i32);
    // reciprocals once per segment (round(2^16/quant)); turns the per-coeff divide into
    // a multiply + shift.
    let mut recip_l = [0i32; 64];
    let mut recip_c = [0i32; 64];
    for i in 0..64 {
        recip_l[i] = (65536 + QUANT_L[i] as i32 / 2) / QUANT_L[i] as i32;
        recip_c[i] = (65536 + QUANT_C[i] as i32 / 2) / QUANT_C[i] as i32;
    }
    let mcx = (w + 15) / 16;
    for my in mcy0..mcy1 {
        for mx in 0..mcx {
            // four 8x8 luma blocks
            for by in 0..2 {
                for bx in 0..2 {
                    let mut blk = [0i32; 64];
                    for r in 0..8 {
                        for c in 0..8 {
                            let (rr, gg, bb) =
                                rgb(fb, w, h, mx * 16 + bx * 8 + c, my * 16 + by * 8 + r);
                            blk[r * 8 + c] = ((77 * rr + 150 * gg + 29 * bb) >> 8) - 128;
                        }
                    }
                    fdct(&mut blk);
                    code_block(&blk, &recip_l, &DC_L, &AC_L, &mut dc_y, &mut bw);
                }
            }
            // one Cb + one Cr block, each from 2x2-averaged RGB
            let mut cb = [0i32; 64];
            let mut cr = [0i32; 64];
            for r in 0..8 {
                for c in 0..8 {
                    let (mut sr, mut sg, mut sb) = (0i32, 0i32, 0i32);
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let (rr, gg, bb) =
                                rgb(fb, w, h, mx * 16 + 2 * c + dx, my * 16 + 2 * r + dy);
                            sr += rr;
                            sg += gg;
                            sb += bb;
                        }
                    }
                    let (rr, gg, bb) = (sr >> 2, sg >> 2, sb >> 2);
                    cb[r * 8 + c] = (-43 * rr - 85 * gg + 128 * bb) >> 8; // = Cb - 128
                    cr[r * 8 + c] = (128 * rr - 107 * gg - 21 * bb) >> 8; // = Cr - 128
                }
            }
            fdct(&mut cb);
            code_block(&cb, &recip_c, &DC_C, &AC_C, &mut dc_cb, &mut bw);
            fdct(&mut cr);
            code_block(&cr, &recip_c, &DC_C, &AC_C, &mut dc_cr, &mut bw);
        }
    }
    bw.flush();
    if bw.overflow || bw.pos > cap {
        None
    } else {
        Some(bw.pos)
    }
}

/// Single-core convenience: full JPEG (headers + one scan segment + EOI) of the whole
/// `w`x`h` frame at `fb` into `out`. Returns the byte length, or None if `out` was too small.
pub fn encode(fb: *const u32, w: usize, h: usize, out: &mut [u8]) -> Option<usize> {
    let start = write_headers(out, w, h, 0);
    let mcy = (h + 15) / 16;
    let n = encode_segment(fb, w, h, 0, mcy, &mut out[start..])?;
    let mut p = start + n;
    w8(out, &mut p, 0xFF);
    w8(out, &mut p, 0xD9); // EOI
    if p > out.len() {
        None
    } else {
        Some(p)
    }
}
