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
}
