//! Spectra (src/cube.cpp): the wavelength of every band of a hyperspectral file (EnMAP, ENVI, PRISMA...), RGB
//! composites and normalized-difference indices by wavelength, and the runs a spectrum's chart draws as one line.

use gdal::{Dataset, Metadata};

use crate::CubeInfo;

/// An RGB composite of a spectrum by wavelength (nm).
pub struct SpectralComposite {
    pub name: &'static str,
    pub nm: [f64; 3],
}

pub const COMPOSITES: [SpectralComposite; 4] = [
    SpectralComposite { name: "True colour (640, 550, 470 nm)", nm: [640.0, 550.0, 470.0] },
    SpectralComposite { name: "Colour infrared (860, 650, 550 nm)", nm: [860.0, 650.0, 550.0] },
    SpectralComposite { name: "SWIR (2200, 860, 650 nm)", nm: [2200.0, 860.0, 650.0] },
    SpectralComposite { name: "Agriculture (1650, 860, 470 nm)", nm: [1650.0, 860.0, 470.0] },
];

/// The layers nearest to a composite's wavelengths; None if one is not covered (no band within 40 nm).
pub fn composite_layers(info: &CubeInfo, c: &SpectralComposite) -> Option<[usize; 3]> {
    if !info.spectral {
        return None;
    }
    let mut rgb = [0; 3];
    for (slot, &nm) in rgb.iter_mut().zip(&c.nm) {
        *slot = info.nearest_layer(nm);
        if (info.layers[*slot].time - nm).abs() > 40.0 {
            return None;
        }
    }
    Some(rgb)
}

/// A normalized difference (a - b) / (a + b) by wavelength (nm).
pub struct SpectralIndex {
    pub name: &'static str,
    pub what: &'static str,
    pub a: f64,
    pub b: f64,
    /// the nearest band must be this close (nm)
    pub tolerance: f64,
}

/// Broad indices accept a multispectral sensor's band (e.g. Landsat's SWIR1 at 1609 nm for 1650); PRI needs the
/// narrow bands it is defined on.
pub const INDICES: [SpectralIndex; 7] = [
    SpectralIndex { name: "NDVI", what: "vegetation (NIR 860, red 650 nm)", a: 860.0, b: 650.0, tolerance: 50.0 },
    SpectralIndex { name: "NDRE", what: "red edge, chlorophyll (790, 720 nm)", a: 790.0, b: 720.0, tolerance: 15.0 },
    SpectralIndex { name: "NDWI", what: "open water (green 560, NIR 860 nm)", a: 560.0, b: 860.0, tolerance: 50.0 },
    SpectralIndex {
        name: "NDMI",
        what: "vegetation moisture (NIR 860, SWIR 1650 nm)",
        a: 860.0,
        b: 1650.0,
        tolerance: 60.0,
    },
    SpectralIndex { name: "NBR", what: "burn severity (NIR 860, SWIR 2200 nm)", a: 860.0, b: 2200.0, tolerance: 60.0 },
    SpectralIndex { name: "NDSI", what: "snow (green 560, SWIR 1650 nm)", a: 560.0, b: 1650.0, tolerance: 60.0 },
    SpectralIndex { name: "PRI", what: "photosynthesis, light use (531, 570 nm)", a: 531.0, b: 570.0, tolerance: 6.0 },
];

/// The bands (1-based, in the file's order) nearest to an index's wavelengths; None if the bands do not cover it.
pub fn index_bands(band_nm: &[f64], s: &SpectralIndex) -> Option<(i32, i32)> {
    let nearest = |nm: f64| -> Option<i32> {
        let mut band = 0usize;
        for i in 0..band_nm.len() {
            if band == 0 || (band_nm[i] - nm).abs() < (band_nm[band - 1] - nm).abs() {
                band = i + 1;
            }
        }
        (band > 0 && (band_nm[band - 1] - nm).abs() <= s.tolerance).then_some(band as i32)
    };
    let (a, b) = (nearest(s.a)?, nearest(s.b)?);
    (a != b).then_some((a, b))
}

/// The factor to nanometres of a unit name ("nm", "Micrometers"...); 0 if unknown.
fn to_nanometres(u: &str) -> f64 {
    let u = u.to_lowercase();
    if u == "nm" || u.starts_with("nanomet") {
        1.0
    } else if u == "um" || u == "µm" || u.starts_with("micromet") || u.starts_with("micron") {
        1000.0
    } else {
        0.0
    }
}

/// EnMAP's product metadata next to its spectral image (the GeoTIFF delivery has no wavelengths of its own):
/// <bandCharacterisation> <bandID number="n"> <wavelengthCenterOfBand>.
fn enmap_wavelengths(path: &str, bands: usize) -> Vec<f64> {
    let p = std::path::Path::new(path);
    let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { return Vec::new() };
    let Some(at) = name.to_ascii_uppercase().rfind("SPECTRAL_IMAGE") else { return Vec::new() };
    let meta = p.with_file_name(format!("{}METADATA.XML", &name[..at]));
    let Ok(xml) = std::fs::read_to_string(&meta) else { return Vec::new() };
    let (Some(begin), Some(end)) = (xml.find("<bandCharacterisation>"), xml.find("</bandCharacterisation>")) else {
        return Vec::new();
    };
    let mut nm = vec![0.0; bands];
    let (id_tag, wl_tag) = ("<bandID number=\"", "<wavelengthCenterOfBand>");
    let mut i = xml[begin..].find(id_tag).map(|k| begin + k);
    while let Some(at) = i.filter(|&k| k < end) {
        let b: usize =
            xml[at + id_tag.len()..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
        if let Some(w) = xml[at..].find(wl_tag).map(|k| at + k)
            && b >= 1
            && b <= bands
            && w < end
        {
            let s: String =
                xml[w + wl_tag.len()..].chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
            nm[b - 1] = s.parse().unwrap_or(0.0);
        }
        i = xml[at + 1..].find(id_tag).map(|k| at + 1 + k);
    }
    nm
}

/// A band's wavelength (nm) from its metadata: ENVI's "wavelength" and "wavelength_units" (the dataset's units
/// when the band has none), or GDAL's IMAGERY domain (CENTRAL_WAVELENGTH_UM); 0 without one.
fn band_wavelength(rb: &gdal::raster::RasterBand, ds_units: Option<&String>) -> f64 {
    if let Some(w) = rb.metadata_item("wavelength", "") {
        let v = w.trim().parse::<f64>().unwrap_or(0.0);
        let u = rb.metadata_item("wavelength_units", "").or_else(|| ds_units.cloned()).unwrap_or_default();
        let mut f = to_nanometres(&u);
        if f == 0.0 {
            // no unit: micrometres are small numbers
            f = if v < 30.0 { 1000.0 } else { 1.0 };
        }
        v * f
    } else if let Some(um) = rb.metadata_item("CENTRAL_WAVELENGTH_UM", "IMAGERY") {
        um.trim().parse::<f64>().unwrap_or(0.0) * 1000.0
    } else {
        0.0
    }
}

/// The wavelength (nm) of every band of a file, from the band metadata (band_wavelength) or EnMAP's
/// METADATA.XML; empty unless each band has one, at least 3 and all different.
pub fn band_wavelengths(ds: &Dataset, path: &str) -> Vec<f64> {
    let n = ds.raster_count();
    if n < 3 {
        return Vec::new();
    }
    let ds_units = ds.metadata_item("wavelength_units", "");
    let mut nm = vec![0.0; n];
    let mut all = true;
    for b in 1..=n {
        let Ok(rb) = ds.rasterband(b) else { return Vec::new() };
        let v = band_wavelength(&rb, ds_units.as_ref());
        nm[b - 1] = v;
        all = v > 0.0;
        if !all {
            break;
        }
    }
    if !all {
        nm = enmap_wavelengths(path, n);
    }
    if nm.is_empty() || nm.iter().any(|v| v.is_nan() || *v <= 0.0) {
        return Vec::new();
    }
    let mut sorted = nm.clone();
    sorted.sort_by(f64::total_cmp);
    // e.g. one band at several dates
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Vec::new();
    }
    nm
}

/// The wavelengths of the bands of each date's file in a series of files: as band_wavelengths, but a band without
/// one (a quality band, an index) is 0 as long as two others have one (a STAC series: reflectance bands and SCL),
/// so the indices can still be picked by wavelength.
pub fn series_band_wavelengths(ds: &Dataset, path: &str) -> Vec<f64> {
    let all = band_wavelengths(ds, path);
    if !all.is_empty() {
        return all;
    }
    let ds_units = ds.metadata_item("wavelength_units", "");
    let nm: Vec<f64> = (1..=ds.raster_count())
        .map(|b| ds.rasterband(b).map_or(0.0, |rb| band_wavelength(&rb, ds_units.as_ref())))
        .map(|v| if v.is_nan() || v < 0.0 { 0.0 } else { v })
        .collect();
    let mut known: Vec<f64> = nm.iter().copied().filter(|v| *v > 0.0).collect();
    known.sort_by(f64::total_cmp);
    if known.len() < 2 || known.windows(2).any(|w| w[0] == w[1]) {
        return Vec::new();
    }
    nm
}

/// The bands' descriptions ("Band N" without one), with the wavelength when they have one and the description
/// does not say it.
pub fn band_names_with_nm(ds: &Dataset, nm: &[f64]) -> Vec<String> {
    (1..=ds.raster_count())
        .map(|b| {
            let d = ds.rasterband(b).ok().and_then(|rb| rb.description().ok()).unwrap_or_default();
            let mut n = if d.is_empty() { format!("Band {b}") } else { d };
            if b <= nm.len() && nm[b - 1] > 0.0 && !n.contains("nm") {
                n += &format!(" ({:.1} nm)", nm[b - 1]);
            }
            n
        })
        .collect()
}

/// Ranges [begin, end) of layers to draw as one line: a spectrum is broken where bands are missing (e.g. water
/// absorption: a step over 3 times the median spacing); otherwise one range.
pub fn plot_runs(info: &CubeInfo) -> Vec<(usize, usize)> {
    let t = info.t();
    if !info.spectral || t < 3 {
        return vec![(0, t)];
    }
    let steps: Vec<f64> = (1..t).map(|i| info.layers[i].time - info.layers[i - 1].time).collect();
    let mut sorted = steps.clone();
    let m = sorted.len() / 2;
    let limit = 3.0 * *sorted.select_nth_unstable_by(m, f64::total_cmp).1;
    let mut runs = Vec::new();
    let mut begin = 0;
    for i in 1..t {
        if steps[i - 1] > limit {
            runs.push((begin, i));
            begin = i;
        }
    }
    runs.push((begin, t));
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_by_wavelength() {
        // Landsat 8: blue 482, green 561, red 655, NIR 865, SWIR1 1609, SWIR2 2201
        let nm = [482.0, 561.0, 655.0, 865.0, 1609.0, 2201.0];
        assert_eq!(index_bands(&nm, &INDICES[0]), Some((4, 3)));
        assert_eq!(index_bands(&nm, &INDICES[4]), Some((4, 6)));
        // no red edge band within 15 nm
        assert_eq!(index_bands(&nm, &INDICES[1]), None);
        assert_eq!(to_nanometres("Micrometers"), 1000.0);
    }
}
