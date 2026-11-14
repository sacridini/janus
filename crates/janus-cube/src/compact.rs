//! Zeit's compact (int8) copies of embedding cubes (zeit.save_raster(emb, path, compact=True)), read as the float32
//! cube they stand for. Every band is Int8 with NoData -128; the file's ZEIT_EMBEDDING_ENCODING tag says how the
//! codes read:
//!
//! - `aef-int8` (AlphaEarth): the same bands as the float32 file, x = sign(v) * (v / 127.5)^2.
//! - `scaled-int8` (TESSERA and the rest): per date, D dimension bands q and then one band c described `scale`
//!   (`<date>_scale`), x = q * 2^(c / 8): D + 1 bands a date.
//!
//! The decoding is Zeit's (zeit/_compact.py, zeit/_alphaearth.py), bit for bit: AlphaEarth in float32, the scaled
//! one in float64 cast to float32.

use std::fmt;

/// The tag of a compact file (GDAL metadata, default domain).
pub const ENCODING_TAG: &str = "ZEIT_EMBEDDING_ENCODING";

/// The no-data code of both encodings.
pub const NODATA: i8 = -128;

/// How a compact file's codes read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// AlphaEarth's own curve (`aef-int8`)
    Aef,
    /// a scale code per pixel and date (`scaled-int8`), one more band a date
    Scaled,
}

impl Encoding {
    /// The encoding named by a file's tag; an error for one this Janus does not know.
    pub fn parse(name: &str) -> Result<Encoding, String> {
        match name.trim() {
            "aef-int8" => Ok(Encoding::Aef),
            "scaled-int8" => Ok(Encoding::Scaled),
            other => Err(format!("unknown embedding encoding {other:?} in the file; update Janus to read it")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Encoding::Aef => "aef-int8",
            Encoding::Scaled => "scaled-int8",
        }
    }

    /// Bands of the file a date holds besides its D dimensions.
    pub fn extra_bands(self) -> usize {
        usize::from(self == Encoding::Scaled)
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A band described as a date's scale band: `scale` or `<date>_scale`.
pub fn is_scale_label(desc: &str) -> bool {
    desc == "scale" || desc.ends_with("_scale")
}

/// An AlphaEarth code as the embedding's value (float32, as Zeit computes it); NaN for -128.
#[inline]
pub fn decode_aef(code: i8) -> f32 {
    if code == NODATA {
        return f32::NAN;
    }
    let r = f32::from(code) / 127.5f32;
    let sq = r * r;
    match code {
        0 => 0.0,
        c if c > 0 => sq,
        _ => -sq,
    }
}

/// A scaled code with its date's scale code (float64, then float32); NaN where either is -128.
#[inline]
pub fn decode_scaled(q: i8, c: i8) -> f32 {
    if q == NODATA || c == NODATA {
        return f32::NAN;
    }
    (f64::from(q) * (f64::from(c) / 8.0).exp2()) as f32
}

/// `values` (Int8 codes read as float32) of AlphaEarth bands, in place.
pub fn decode_aef_in_place(values: &mut [f32]) {
    for v in values {
        *v = decode_aef(*v as i8);
    }
}

/// `values` (Int8 codes read as float32) of dimension bands with the same pixels of the date's scale band, in place.
pub fn decode_scaled_in_place(values: &mut [f32], scale: &[f32]) {
    for (v, &c) in values.iter_mut().zip(scale) {
        *v = decode_scaled(*v as i8, c as i8);
    }
}

fn scale_limit() -> f64 {
    (127.0f64 / 8.0).exp2()
}

/// AlphaEarth's own curve, as Zeit encodes it (zeit/_compact.py, encode_aef): `block` is [dimension][plane] floats,
/// `out` the same shape as Int8: v = sign(x) * round(sqrt(|x|) * 127.5) clipped to [-127, 127], and -128 in all the
/// dimensions of a pixel where one is NaN.
pub fn encode_aef(block: &[f32], d: usize, plane: usize, out: &mut [i8]) {
    for p in 0..plane {
        let bad = (0..d).any(|b| block[b * plane + p].is_nan());
        for b in 0..d {
            let x = if bad { 0.0 } else { f64::from(block[b * plane + p]) };
            let v = if bad {
                NODATA
            } else {
                let r = (x.abs().sqrt() * 127.5).round_ties_even();
                let sign = if x < 0.0 {
                    -1.0
                } else if x > 0.0 {
                    1.0
                } else {
                    0.0
                };
                (sign * r).clamp(-127.0, 127.0) as i8
            };
            out[b * plane + p] = v;
        }
    }
}

/// The scaled encoding, as Zeit's encode_scaled: `block` is [dimension][plane] floats, `out` is [dimension + 1][plane]
/// Int8, the last band the scale code c = ceil(8 * log2(m / 127)) of the pixel's largest |x| m (-127 when 0), and
/// q = round(x / 2^(c / 8)) in [-127, 127]; -128 in every band of a pixel where one dimension is NaN. An error when
/// a value is too large for the scale.
pub fn encode_scaled(block: &[f32], d: usize, plane: usize, out: &mut [i8]) -> Result<(), String> {
    let limit = scale_limit();
    for p in 0..plane {
        if (0..d).any(|b| block[b * plane + p].is_nan()) {
            for b in 0..=d {
                out[b * plane + p] = NODATA;
            }
            continue;
        }
        let m = (0..d).map(|b| f64::from(block[b * plane + p]).abs()).fold(0.0, f64::max);
        if m / 127.0 > limit {
            return Err(format!(
                "the embeddings are too large for the compact encoding (a value of {m:.6e}; the largest that fits is {:.6e})",
                127.0 * limit
            ));
        }
        let mut c = if m > 0.0 { (8.0 * (m / 127.0).log2()).ceil().clamp(-127.0, 127.0) } else { -127.0 };
        // float rounding at a power-of-two edge
        if (m / (c / 8.0).exp2()).round_ties_even() > 127.0 {
            c += 1.0;
        }
        let c = c.min(127.0);
        let s = (c / 8.0).exp2();
        for b in 0..d {
            out[b * plane + p] = (f64::from(block[b * plane + p]) / s).round_ties_even().clamp(-127.0, 127.0) as i8;
        }
        out[d * plane + p] = c as i8;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aef_curve() {
        assert!(decode_aef(-128).is_nan());
        assert_eq!(decode_aef(0), 0.0);
        assert_eq!(decode_aef(127), (127.0f32 / 127.5) * (127.0f32 / 127.5));
        assert_eq!(decode_aef(-127), -decode_aef(127));
    }

    #[test]
    fn scaled_codes() {
        assert!(decode_scaled(-128, 0).is_nan() && decode_scaled(5, -128).is_nan());
        assert_eq!(decode_scaled(3, 0), 3.0);
        assert_eq!(decode_scaled(-4, 16), -16.0);
        assert_eq!(decode_scaled(0, 7), 0.0);
    }

    #[test]
    fn unknown_encoding_is_an_error() {
        assert_eq!(Encoding::parse("aef-int8"), Ok(Encoding::Aef));
        assert_eq!(Encoding::parse("scaled-int8"), Ok(Encoding::Scaled));
        let e = Encoding::parse("wavelet-int4").unwrap_err();
        assert!(e.contains("unknown embedding encoding") && e.contains("update Janus"), "{e}");
    }

    #[test]
    fn aef_codes_round_trip_exactly() {
        // every code of the curve, one pixel each, 3 dimensions
        let codes: Vec<i8> = (-127..=127).collect();
        let plane = codes.len();
        let block: Vec<f32> = (0..3).flat_map(|_| codes.iter().map(|&c| decode_aef(c))).collect();
        let mut out = vec![0i8; 3 * plane];
        encode_aef(&block, 3, plane, &mut out);
        for b in 0..3 {
            assert_eq!(&out[b * plane..(b + 1) * plane], &codes[..]);
        }
        // a NaN dimension: the whole pixel is nodata
        let mut nan = block.clone();
        nan[plane + 5] = f32::NAN;
        encode_aef(&nan, 3, plane, &mut out);
        assert!((0..3).all(|b| out[b * plane + 5] == NODATA) && out[4] == codes[4]);
    }

    #[test]
    fn scaled_round_trip_within_its_bound() {
        let (d, plane) = (6, 40);
        let mut block = vec![0f32; d * plane];
        for p in 0..plane {
            let mag = 10f32.powf(-2.0 + 5.0 * p as f32 / plane as f32); // (below ~3e-3 the finest step, 2^(-127/8), is coarser)
            for b in 0..d {
                block[b * plane + p] = (b as f32 - 2.3) * mag * 0.37;
            }
        }
        // an all-zero pixel and a NaN one
        for b in 0..d {
            block[b * plane] = 0.0;
            block[b * plane + 1] = f32::NAN;
        }
        let mut out = vec![0i8; (d + 1) * plane];
        encode_scaled(&block, d, plane, &mut out).unwrap();
        assert_eq!(out[d * plane], -127);
        assert!((0..=d).all(|b| out[b * plane + 1] == NODATA));
        for p in 2..plane {
            let m = (0..d).map(|b| block[b * plane + p].abs()).fold(0.0, f32::max);
            for b in 0..d {
                let x = decode_scaled(out[b * plane + p], out[d * plane + p]);
                assert!((x - block[b * plane + p]).abs() <= 0.0043 * m, "{p} {b}: {x} vs {}", block[b * plane + p]);
            }
        }
        // too large
        let big = vec![1e12f32; d];
        let mut o = vec![0i8; d + 1];
        assert!(encode_scaled(&big, d, 1, &mut o).unwrap_err().contains("too large"));
    }
}
