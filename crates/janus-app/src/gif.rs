//! Animated GIF, written without an image library (src/gif.cpp, byte for byte): each frame gets its own palette of
//! up to 256 colours (exact when the frame has no more, else median cut), LZW-compressed, looping forever
//! (NETSCAPE2.0). A small decoder reads them back (the self-test). The median cut sorts its bins stably, as the C++
//! now does: the same palette on every platform and compiler.

#![allow(clippy::needless_range_loop)] // the C++'s loops over channels and codes

use std::io::Write;
use std::time::Instant;

pub struct GifPalette {
    /// colours used (1..256)
    pub n: usize,
    pub rgb: [u8; 256 * 3],
}

impl Default for GifPalette {
    fn default() -> Self {
        GifPalette { n: 0, rgb: [0; 256 * 3] }
    }
}

#[inline]
fn rgb_of(p: &[u8]) -> u32 {
    (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2])
}

/// Histogram bins: 6 bits per channel (5 bits merged neighbouring colours of smooth colour maps into visible
/// bands).
#[inline]
fn bin_of(r: i32, g: i32, b: i32) -> usize {
    (((r >> 2) << 12) | ((g >> 2) << 6) | (b >> 2)) as usize
}

/// Up to 256 distinct colours: the palette and the indices, in one pass (an open-addressing table of 1024 slots);
/// false as soon as a 257th appears.
fn exact_palette(rgba: &[u8], n: usize, pal: &mut GifPalette, index: &mut [u8]) -> bool {
    const EMPTY: u32 = 0xFFFF_FFFF;
    let mut key = [EMPTY; 1024];
    let mut val = [0u8; 1024];
    pal.n = 0;
    let (mut last, mut last_idx) = (EMPTY, 0u8);
    for i in 0..n {
        let c = rgb_of(&rgba[i * 4..]);
        if c != last {
            let mut h = (c.wrapping_mul(2654435761) >> 22) as usize;
            while key[h] != EMPTY && key[h] != c {
                h = (h + 1) & 1023;
            }
            if key[h] == EMPTY {
                if pal.n == 256 {
                    return false;
                }
                key[h] = c;
                val[h] = pal.n as u8;
                pal.rgb[pal.n * 3] = (c >> 16) as u8;
                pal.rgb[pal.n * 3 + 1] = (c >> 8) as u8;
                pal.rgb[pal.n * 3 + 2] = c as u8;
                pal.n += 1;
            }
            last = c;
            last_idx = val[h];
        }
        index[i] = last_idx;
    }
    true
}

/// The frame's colours (RGBA8 rows, alpha ignored) as palette indices: an exact palette when there are 256 colours
/// or fewer, else a median cut of a 6-bit histogram; `dither` diffuses the error (Floyd-Steinberg).
pub fn quantize(rgba: &[u8], w: usize, h: usize, dither: bool, pal: &mut GifPalette, index: &mut Vec<u8>) {
    let n = w * h;
    index.resize(n, 0);
    if !dither && exact_palette(rgba, n, pal, index) {
        return;
    }
    // histogram on 6 bits per channel, with the sums for each bin's true mean
    const BINS: usize = 1 << 18;
    let mut count = vec![0u32; BINS];
    let mut sum = vec![0u64; BINS * 3];
    for p in rgba[..n * 4].as_chunks::<4>().0 {
        let b = bin_of(i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
        count[b] += 1;
        sum[b * 3] += u64::from(p[0]);
        sum[b * 3 + 1] += u64::from(p[1]);
        sum[b * 3 + 2] += u64::from(p[2]);
    }
    let mut bins: Vec<usize> = (0..BINS).filter(|&b| count[b] != 0).collect();

    // median cut: the box with the most pixels x its longest side is split at the median of that side, until 256
    // boxes (or no box can be split)
    #[derive(Clone, Copy)]
    struct Bx {
        begin: usize,
        end: usize,
        pixels: u64,
        axis: usize,
        range: i32,
    }
    let chan = |bin: usize, axis: usize| ((bin >> (12 - 6 * axis)) & 63) as i32;
    let measure = |bx: &mut Bx, bins: &[usize]| {
        let (mut lo, mut hi) = ([63; 3], [0; 3]);
        bx.pixels = 0;
        for &b in &bins[bx.begin..bx.end] {
            for a in 0..3 {
                lo[a] = lo[a].min(chan(b, a));
                hi[a] = hi[a].max(chan(b, a));
            }
            bx.pixels += u64::from(count[b]);
        }
        bx.axis = 0;
        for a in 1..3 {
            if hi[a] - lo[a] > hi[bx.axis] - lo[bx.axis] {
                bx.axis = a;
            }
        }
        bx.range = hi[bx.axis] - lo[bx.axis];
    };
    let mut boxes = vec![Bx { begin: 0, end: bins.len(), pixels: 0, axis: 0, range: 0 }];
    measure(&mut boxes[0], &bins);
    while boxes.len() < 256 {
        let mut best = None;
        let mut best_score = 0.0;
        for (k, b) in boxes.iter().enumerate() {
            let score = b.pixels as f64 * f64::from(b.range);
            if b.end - b.begin > 1 && b.range > 0 && score > best_score {
                best_score = score;
                best = Some(k);
            }
        }
        let Some(best) = best else { break };
        let bx = boxes[best];
        bins[bx.begin..bx.end].sort_by_key(|&b| chan(b, bx.axis)); // stable
        let mut acc = 0u64;
        let mut cut = bx.begin + 1;
        for i in bx.begin..bx.end - 1 {
            acc += u64::from(count[bins[i]]);
            cut = i + 1;
            if acc * 2 >= bx.pixels {
                break;
            }
        }
        let mut a = Bx { begin: bx.begin, end: cut, pixels: 0, axis: 0, range: 0 };
        let mut b = Bx { begin: cut, end: bx.end, pixels: 0, axis: 0, range: 0 };
        measure(&mut a, &bins);
        measure(&mut b, &bins);
        boxes[best] = a;
        boxes.push(b);
    }
    pal.n = boxes.len();
    for (k, bx) in boxes.iter().enumerate() {
        let (mut s, mut c) = ([0u64; 3], 0u64);
        for &b in &bins[bx.begin..bx.end] {
            for a in 0..3 {
                s[a] += sum[b * 3 + a];
            }
            c += u64::from(count[b]);
        }
        for a in 0..3 {
            pal.rgb[k * 3 + a] = (s[a] + c / 2).checked_div(c).unwrap_or(0) as u8;
        }
    }

    // bin -> nearest palette colour (from the bin's mean; bins only reached by dithering from their centre),
    // computed once per bin
    let mut lut = vec![-1i16; BINS];
    let mut nearest = |bin: usize| -> usize {
        if lut[bin] >= 0 {
            return lut[bin] as usize;
        }
        let (r, g, b) = if count[bin] > 0 {
            let c = u64::from(count[bin]);
            ((sum[bin * 3] / c) as i32, (sum[bin * 3 + 1] / c) as i32, (sum[bin * 3 + 2] / c) as i32)
        } else {
            (chan(bin, 0) * 4 + 2, chan(bin, 1) * 4 + 2, chan(bin, 2) * 4 + 2)
        };
        let (mut best, mut best_d) = (0, 1 << 30);
        for k in 0..pal.n {
            let dr = r - i32::from(pal.rgb[k * 3]);
            let dg = g - i32::from(pal.rgb[k * 3 + 1]);
            let db = b - i32::from(pal.rgb[k * 3 + 2]);
            let d = dr * dr + dg * dg + db * db;
            if d < best_d {
                best_d = d;
                best = k;
            }
        }
        lut[bin] = best as i16;
        best
    };
    if !dither {
        for i in 0..n {
            let p = &rgba[i * 4..];
            index[i] = nearest(bin_of(i32::from(p[0]), i32::from(p[1]), i32::from(p[2]))) as u8;
        }
        return;
    }
    // Floyd-Steinberg: the error of each pixel spread to its neighbours
    let row = (w + 2) * 3;
    let mut err = vec![0f32; row * 2];
    for y in 0..h {
        let (cur_off, nxt_off) = ((y & 1) * row, ((y + 1) & 1) * row);
        err[nxt_off..nxt_off + row].fill(0.0);
        for x in 0..w {
            let p = &rgba[(y * w + x) * 4..];
            let mut c = [0i32; 3];
            for a in 0..3 {
                c[a] = ((f32::from(p[a]) + err[cur_off + (x + 1) * 3 + a]).round() as i32).clamp(0, 255);
            }
            let k = nearest(bin_of(c[0], c[1], c[2]));
            index[y * w + x] = k as u8;
            for a in 0..3 {
                let e = (c[a] - i32::from(pal.rgb[k * 3 + a])) as f32;
                err[cur_off + (x + 2) * 3 + a] += e * 7.0 / 16.0;
                err[nxt_off + x * 3 + a] += e * 3.0 / 16.0;
                err[nxt_off + (x + 1) * 3 + a] += e * 5.0 / 16.0;
                err[nxt_off + (x + 2) * 3 + a] += e * 1.0 / 16.0;
            }
        }
    }
}

/// A GIF being written.
pub struct GifWriter {
    f: std::io::BufWriter<std::fs::File>,
    w: usize,
    h: usize,
    ok: bool,
    index: Vec<u8>,
    out: Vec<u8>,
    pub last_quantize_ms: f64,
    pub last_encode_ms: f64,
}

impl GifWriter {
    /// Header, logical screen and the loop extension (loops: 0 = forever).
    pub fn open(path: &str, w: usize, h: usize, loops: u16) -> Result<GifWriter, String> {
        if w == 0 || h == 0 || w > 65535 || h > 65535 {
            return Err("a GIF is at most 65535 pixels per side".into());
        }
        let file = std::fs::File::create(path).map_err(|_| format!("could not create {path}"))?;
        let mut g = GifWriter {
            f: std::io::BufWriter::new(file),
            w,
            h,
            ok: true,
            index: Vec::new(),
            out: Vec::new(),
            last_quantize_ms: 0.0,
            last_encode_ms: 0.0,
        };
        // logical screen without a global colour table (each frame has its own)
        let (w8, h8) = (w as u16, h as u16);
        g.put(&[b'G', b'I', b'F', b'8', b'9', b'a', w8 as u8, (w8 >> 8) as u8, h8 as u8, (h8 >> 8) as u8, 0x70, 0, 0]);
        g.put(&[
            0x21,
            0xFF,
            11,
            b'N',
            b'E',
            b'T',
            b'S',
            b'C',
            b'A',
            b'P',
            b'E',
            b'2',
            b'.',
            b'0',
            3,
            1,
            loops as u8,
            (loops >> 8) as u8,
            0,
        ]);
        if !g.ok {
            return Err(format!("could not write {path}"));
        }
        Ok(g)
    }

    fn put(&mut self, b: &[u8]) {
        if self.ok && self.f.write_all(b).is_err() {
            self.ok = false;
        }
    }

    /// A full frame (w x h RGBA8, rows top-down) shown for `delay_cs` hundredths of a second.
    pub fn add_frame(&mut self, rgba: &[u8], delay_cs: u16, dither: bool) -> Result<(), String> {
        let t0 = Instant::now();
        let mut pal = GifPalette::default();
        let mut index = std::mem::take(&mut self.index);
        quantize(rgba, self.w, self.h, dither, &mut pal, &mut index);
        self.last_quantize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let t0 = Instant::now();

        // graphic control: the delay; disposal 1 (each frame covers the whole screen)
        self.put(&[0x21, 0xF9, 4, 0x04, delay_cs as u8, (delay_cs >> 8) as u8, 0, 0]);
        // image descriptor with a local colour table of 256 entries (unused ones black)
        let (w8, h8) = (self.w as u16, self.h as u16);
        self.put(&[0x2C, 0, 0, 0, 0, w8 as u8, (w8 >> 8) as u8, h8 as u8, (h8 >> 8) as u8, 0x87]);
        let mut table = [0u8; 256 * 3];
        table[..pal.n * 3].copy_from_slice(&pal.rgb[..pal.n * 3]);
        self.put(&table);

        // LZW, 8-bit minimum code size (codes 9 to 12 bits), the dictionary in an open-addressing hash of (prefix
        // code, byte) as in compress/giflib
        const MIN: u32 = 8;
        const CLEAR: i32 = 1 << MIN;
        const EOI: i32 = CLEAR + 1;
        const HASH: usize = 5003;
        let mut hkey = vec![-1i32; HASH];
        let mut hcode = vec![0i16; HASH];
        let mut out = std::mem::take(&mut self.out);
        out.clear();
        let (mut acc, mut bits) = (0u32, 0u32);
        let mut emit = |out: &mut Vec<u8>, code: i32, size: u32| {
            acc |= (code as u32) << bits;
            bits += size;
            while bits >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        };
        let (mut code_size, mut max_code) = (MIN + 1, EOI);
        emit(&mut out, CLEAR, code_size);
        let mut cur = i32::from(index[0]);
        for &ci in &index[1..] {
            let c = i32::from(ci);
            let key = (cur << 8) | c;
            let mut h = (((c as u32) << 12) ^ (cur as u32)) as usize % HASH;
            while hkey[h] != -1 && hkey[h] != key {
                h = if h + 1 == HASH { 0 } else { h + 1 };
            }
            if hkey[h] == key {
                cur = i32::from(hcode[h]);
                continue;
            }
            emit(&mut out, cur, code_size);
            hkey[h] = key;
            max_code += 1;
            hcode[h] = max_code as i16;
            if max_code >= (1 << code_size) {
                code_size += 1;
            }
            if max_code == 4095 {
                emit(&mut out, CLEAR, code_size);
                hkey.fill(-1);
                code_size = MIN + 1;
                max_code = EOI;
            }
            cur = c;
        }
        emit(&mut out, cur, code_size);
        // a decoder adds one more entry on reading that last code (and widens its codes one entry before the
        // encoder does): the end code at its width
        let end_size = if max_code + 1 == (1 << code_size) && code_size < 12 { code_size + 1 } else { code_size };
        emit(&mut out, EOI, end_size);
        if bits > 0 {
            out.push(acc as u8);
        }

        self.put(&[MIN as u8]);
        for chunk in out.chunks(255) {
            self.put(&[chunk.len() as u8]);
            self.put(chunk);
        }
        self.put(&[0]);
        self.index = index;
        self.out = out;
        self.last_encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if !self.ok {
            return Err("could not write the GIF (disk full?)".into());
        }
        Ok(())
    }

    /// The trailer; an error if anything could not be written.
    pub fn close(mut self) -> Result<(), String> {
        self.put(&[0x3B]);
        if self.f.flush().is_err() {
            self.ok = false;
        }
        if !self.ok {
            return Err("could not write the GIF (disk full?)".into());
        }
        Ok(())
    }
}

/// A decoded GIF: every frame composed onto the canvas (RGBA8, w x h).
pub struct GifImage {
    pub w: usize,
    pub h: usize,
    /// NETSCAPE2.0 loop count (0 = forever), -1 without the extension
    pub loops: i32,
    pub frames: Vec<(u16, Vec<u8>)>,
}

pub fn decode(path: &str) -> Result<GifImage, String> {
    let d = std::fs::read(path).map_err(|_| format!("could not open {path}"))?;
    let need = |p: usize, k: usize| p + k <= d.len();
    let u16_at = |at: usize| usize::from(d[at]) | (usize::from(d[at + 1]) << 8);
    if !need(0, 13) || &d[..4] != b"GIF8" {
        return Err("not a GIF".into());
    }
    let mut img = GifImage { w: u16_at(6), h: u16_at(8), loops: -1, frames: Vec::new() };
    let flags = d[10];
    let mut p = 13;
    let mut global = Vec::new();
    if flags & 0x80 != 0 {
        let k = 3usize << ((flags & 7) + 1);
        if !need(p, k) {
            return Err("truncated colour table".into());
        }
        global = d[p..p + k].to_vec();
        p += k;
    }
    let mut canvas = vec![0u8; img.w * img.h * 4];
    let (mut delay, mut transparent) = (0u16, -1i32);
    let skip_blocks = |p: &mut usize| {
        while need(*p, 1) && d[*p] != 0 {
            *p += 1 + usize::from(d[*p]);
        }
        *p += 1;
    };
    while need(p, 1) {
        let tag = d[p];
        p += 1;
        if tag == 0x3B {
            return Ok(img);
        }
        if tag == 0x21 {
            if !need(p, 1) {
                break;
            }
            let label = d[p];
            p += 1;
            if label == 0xF9 && need(p, 6) {
                delay = u16_at(p + 2) as u16;
                transparent = if d[p + 1] & 1 != 0 { i32::from(d[p + 4]) } else { -1 };
                p += 1 + usize::from(d[p]);
                skip_blocks(&mut p);
            } else if label == 0xFF && need(p, 12) && &d[p + 1..p + 12] == b"NETSCAPE2.0" {
                p += 1 + usize::from(d[p]);
                if need(p, 4) && d[p] == 3 && d[p + 1] == 1 {
                    img.loops = u16_at(p + 2) as i32;
                }
                skip_blocks(&mut p);
            } else {
                skip_blocks(&mut p);
            }
            continue;
        }
        if tag != 0x2C || !need(p, 9) {
            return Err("unexpected block in the GIF".into());
        }
        let (fx, fy, fw, fh) = (u16_at(p), u16_at(p + 2), u16_at(p + 4), u16_at(p + 6));
        let lf = d[p + 8];
        p += 9;
        let mut table = global.clone();
        if lf & 0x80 != 0 {
            let k = 3usize << ((lf & 7) + 1);
            if !need(p, k) {
                return Err("truncated colour table".into());
            }
            table = d[p..p + k].to_vec();
            p += k;
        }
        if lf & 0x40 != 0 {
            return Err("interlaced frames are not decoded".into());
        }
        if !need(p, 1) {
            break;
        }
        let min_size = u32::from(d[p]);
        p += 1;
        let mut data = Vec::new();
        while need(p, 1) && d[p] != 0 {
            let k = usize::from(d[p]);
            if !need(p, 1 + k) {
                return Err("truncated image data".into());
            }
            data.extend_from_slice(&d[p + 1..p + 1 + k]);
            p += 1 + k;
        }
        p += 1;
        // LZW decode
        let clear = 1i32 << min_size;
        let eoi = clear + 1;
        let mut prefix = vec![0u16; 4096];
        let mut suffix = vec![0u8; 4096];
        let mut stack = vec![0u8; 4097];
        for i in 0..clear as usize {
            suffix[i] = i as u8;
        }
        let mut indices: Vec<u8> = Vec::with_capacity(fw * fh);
        let (mut size, mut next, mut prev) = (min_size + 1, eoi + 1, -1i32);
        let mut first = 0u8;
        let (mut acc, mut bits, mut q) = (0u32, 0u32, 0usize);
        loop {
            while bits < size && q < data.len() {
                acc |= u32::from(data[q]) << bits;
                q += 1;
                bits += 8;
            }
            if bits < size {
                break;
            }
            let code = (acc & ((1u32 << size) - 1)) as i32;
            acc >>= size;
            bits -= size;
            if code == clear {
                size = min_size + 1;
                next = eoi + 1;
                prev = -1;
                continue;
            }
            if code == eoi {
                break;
            }
            let (mut c, mut sp) = (code, 0usize);
            if prev < 0 {
                if code >= clear {
                    return Err("bad LZW stream".into());
                }
                indices.push(code as u8);
                first = code as u8;
                prev = code;
                continue;
            }
            if code > next || code >= 4096 {
                return Err("bad LZW code".into());
            }
            if code == next {
                stack[sp] = first;
                sp += 1;
                c = prev;
            }
            while c >= clear {
                stack[sp] = suffix[c as usize];
                sp += 1;
                c = i32::from(prefix[c as usize]);
            }
            stack[sp] = c as u8;
            sp += 1;
            first = c as u8;
            while sp > 0 {
                sp -= 1;
                indices.push(stack[sp]);
            }
            if next < 4096 {
                prefix[next as usize] = prev as u16;
                suffix[next as usize] = first;
                next += 1;
                if next == (1 << size) && size < 12 {
                    size += 1;
                }
            }
            prev = code;
        }
        if indices.len() < fw * fh {
            return Err("short image data".into());
        }
        for y in 0..fh {
            for x in 0..fw {
                let k = usize::from(indices[y * fw + x]);
                let (cx, cy) = (fx + x, fy + y);
                if k as i32 == transparent || cx >= img.w || cy >= img.h || k * 3 + 2 >= table.len() {
                    continue;
                }
                let o = (cy * img.w + cx) * 4;
                canvas[o..o + 3].copy_from_slice(&table[k * 3..k * 3 + 3]);
                canvas[o + 3] = 255;
            }
        }
        img.frames.push((delay, canvas.clone()));
        delay = 0;
        transparent = -1;
    }
    Err("the GIF has no trailer".into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The C++'s codec test (selfTestGifCodec): 251 colours of odd size (LZW resets), a gradient by median cut,
    /// then dithered.
    pub(crate) fn test_images() -> (usize, usize, Vec<u8>, Vec<u8>) {
        let (w, h) = (173usize, 91usize);
        let mut img = vec![0u8; w * h * 4];
        let mut smooth = img.clone();
        for y in 0..h {
            for x in 0..w {
                let p = &mut img[(y * w + x) * 4..][..4];
                let k = (x * 7 + y * 13 + (x * y) % 11) % 251;
                p.copy_from_slice(&[(k * 37) as u8, (k * 11 + 3) as u8, (255 - k) as u8, 255]);
                let q = &mut smooth[(y * w + x) * 4..][..4];
                q.copy_from_slice(&[
                    (x * 255 / (w - 1)) as u8,
                    (y * 255 / (h - 1)) as u8,
                    ((x + y) * 255 / (w + h - 2)) as u8,
                    255,
                ]);
            }
        }
        (w, h, img, smooth)
    }

    fn compare(a: &[u8], b: &[u8]) -> (f64, i32) {
        let (mut s, mut n, mut m) = (0f64, 0usize, 0);
        for (pa, pb) in a.chunks(4).zip(b.chunks(4)) {
            for c in 0..3 {
                let d = (i32::from(pa[c]) - i32::from(pb[c])).abs();
                s += f64::from(d);
                m = m.max(d);
                n += 1;
            }
        }
        (s / n as f64, m)
    }

    #[test]
    fn codec() {
        let (w, h, img, smooth) = test_images();
        let path = std::env::temp_dir().join("janus-gif-codec.gif").to_string_lossy().into_owned();
        let mut g = GifWriter::open(&path, w, h, 0).unwrap();
        g.add_frame(&img, 7, false).unwrap();
        g.add_frame(&smooth, 8, false).unwrap();
        g.add_frame(&smooth, 9, true).unwrap();
        g.close().unwrap();
        let d = decode(&path).unwrap();
        assert_eq!((d.w, d.h, d.frames.len(), d.loops), (w, h, 3, 0));
        assert_eq!(d.frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![7, 8, 9]);
        assert_eq!(compare(&d.frames[0].1, &img).1, 0, "251 colours come back exact");
        let (mean, maxd) = compare(&d.frames[1].1, &smooth);
        println!("gradient within {mean:.2} mean, {maxd} max");
        assert!(mean <= 4.0 && maxd <= 24);
        let (mean, _) = compare(&d.frames[2].1, &smooth);
        assert!(mean <= 8.0, "dithered {mean}");
        let _ = std::fs::remove_file(&path);
    }

    /// The same frames through the C++ writer (JANUS_GIF_CHECK: a build of src/gif.cpp with a driver that reads
    /// frames.bin) give the same bytes: the codec's frames and a map's screenshot (JANUS_GIF_IMAGE, a PNG), plain
    /// and dithered. `cargo test -p janus-app --release -- --ignored gif_against_the_cpp --nocapture`
    #[test]
    #[ignore]
    fn gif_against_the_cpp() {
        let Some(exe) = std::env::var_os("JANUS_GIF_CHECK") else { return };
        let dir = std::env::temp_dir().join("janus-gif-check");
        let _ = std::fs::create_dir_all(&dir);
        // width, height, and each frame's delay, dither and pixels
        type Set = (usize, usize, Vec<(u16, bool, Vec<u8>)>);
        let mut sets: Vec<Set> = Vec::new();
        let (w, h, img, smooth) = test_images();
        sets.push((w, h, vec![(7, false, img), (8, false, smooth.clone()), (9, true, smooth)]));
        if let Some(png) = std::env::var_os("JANUS_GIF_IMAGE") {
            let im = image::open(png).unwrap().to_rgba8();
            let (w, h) = (im.width() as usize, im.height() as usize);
            let px = im.into_raw();
            sets.push((w, h, vec![(33, false, px.clone()), (33, true, px)]));
        }
        for (k, (w, h, frames)) in sets.iter().enumerate() {
            let bin = dir.join(format!("frames{k}.bin"));
            let mut raw = Vec::new();
            for v in [*w as u32, *h as u32, frames.len() as u32] {
                raw.extend_from_slice(&v.to_le_bytes());
            }
            for (delay, dither, px) in frames {
                raw.extend_from_slice(&delay.to_le_bytes());
                raw.push(u8::from(*dither));
                raw.extend_from_slice(px);
            }
            std::fs::write(&bin, raw).unwrap();
            let rust = dir.join(format!("rust{k}.gif")).to_string_lossy().into_owned();
            let cpp = dir.join(format!("cpp{k}.gif"));
            let t = std::time::Instant::now();
            let mut g = GifWriter::open(&rust, *w, *h, 0).unwrap();
            for (delay, dither, px) in frames {
                g.add_frame(px, *delay, *dither).unwrap();
            }
            g.close().unwrap();
            let rust_ms = t.elapsed().as_secs_f64() * 1000.0;
            let t = std::time::Instant::now();
            let out = std::process::Command::new(&exe).arg(&bin).arg(&cpp).output().unwrap();
            let cpp_ms = t.elapsed().as_secs_f64() * 1000.0;
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
            let (a, b) = (std::fs::read(&rust).unwrap(), std::fs::read(&cpp).unwrap());
            let differ = a.iter().zip(&b).position(|(x, y)| x != y);
            println!(
                "set {k}: {w} x {h}, {} frames: {} bytes (Rust {rust_ms:.0} ms) vs {} bytes (C++ {cpp_ms:.0} ms, a process); first difference {differ:?}",
                frames.len(),
                a.len(),
                b.len()
            );
            assert_eq!(a, b, "the GIFs differ");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
