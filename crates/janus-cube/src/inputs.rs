//! What the inputs of a series name: a folder, a wildcard pattern, a list of files (src/cube.cpp, expandInputs).

use std::cmp::Ordering;
use std::path::Path;

const RASTER_EXTENSIONS: [&str; 15] =
    ["tif", "tiff", "vrt", "img", "jp2", "nc", "hdf", "h5", "dat", "bil", "bsq", "bip", "asc", "envi", "grd"];

/// File names with a raster extension Janus lists by default.
pub fn is_raster_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RASTER_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// File names of vector data (points, lines, polygons) Janus opens as vector layers (src/vectors.cpp).
pub fn is_vector_path(path: &Path) -> bool {
    const EXTS: [&str; 12] =
        ["geojson", "json", "shp", "gpkg", "kml", "kmz", "gml", "fgb", "csv", "gpx", "tab", "geojsonl"];
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// "Natural" order, img2 before img10: runs of digits by their value, the rest by letter without case.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (i0, j0) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let trim = |s: &[u8]| -> Vec<u8> {
                let z = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
                s[z..].to_vec()
            };
            let (na, nb) = (trim(&a[i0..i]), trim(&b[j0..j]));
            if na.len() != nb.len() {
                return na.len().cmp(&nb.len());
            }
            if na != nb {
                return na.cmp(&nb);
            }
        } else {
            let (ca, cb) = (a[i].to_ascii_lowercase(), b[j].to_ascii_lowercase());
            if ca != cb {
                return ca.cmp(&cb);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Case-insensitive `*` and `?` wildcards.
fn wildcard_match(pat: &[u8], s: &[u8]) -> bool {
    match pat.first() {
        None => s.is_empty(),
        Some(b'*') => wildcard_match(&pat[1..], s) || (!s.is_empty() && wildcard_match(pat, &s[1..])),
        Some(&p) => !s.is_empty() && (p == b'?' || p.eq_ignore_ascii_case(&s[0])) && wildcard_match(&pat[1..], &s[1..]),
    }
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The files of the inputs, each folder's and pattern's in natural order. The paths are spelt as the C++ spells
/// them (the folder as given, then the file name), which the caches' keys depend on. URLs, subdataset names, vrt://
/// connections and Zarr stores (folders) are one input each; an EnMAP product's folder opens its spectral image alone.
pub fn expand_inputs(inputs: &[String]) -> Vec<String> {
    use crate::remote::{dataset_file, gdal_path, is_enmap_spectral_image, is_remote_path, is_zarr_store};
    let list = |dir: &Path, keep: &dyn Fn(&Path) -> bool| -> Vec<String> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && keep(&e.path()))
            .map(|e| path_string(&e.path()))
            .collect()
    };
    let mut files = Vec::new();
    for input in inputs {
        // GDAL's vrt:// connection (a band of a file: vrt://f.tif?bands=3), its "?" not a wildcard
        if input.starts_with("vrt://") {
            files.push(input.clone());
            continue;
        }
        if is_remote_path(input) || dataset_file(input) != input {
            files.push(gdal_path(input));
            continue;
        }
        let p = Path::new(input);
        if is_zarr_store(p) {
            files.push(input.clone());
        } else if p.is_dir() {
            let mut dir = list(p, &is_raster_path);
            // an EnMAP product folder: its spectral image, not the quality layers beside it
            if let Some(s) = dir.iter().find(|f| is_enmap_spectral_image(Path::new(f))) {
                dir = vec![s.clone()];
            }
            // no rasters, but EnMAP products in subfolders: where a zip extracts to (one product: its spectrum;
            // several: a series of scenes)
            if dir.is_empty() {
                for sub in std::fs::read_dir(p).into_iter().flatten().flatten() {
                    if sub.file_type().is_ok_and(|t| t.is_dir()) {
                        dir.extend(list(&sub.path(), &is_enmap_spectral_image));
                    }
                }
            }
            dir.sort_by(|a, b| natural_cmp(a, b));
            files.extend(dir);
        } else if input.contains(['*', '?']) {
            let parent = p.parent().filter(|d| !d.as_os_str().is_empty());
            let parent = parent.map(Path::to_path_buf).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let pat = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut dir = list(&parent, &|f: &Path| {
                wildcard_match(pat.as_bytes(), f.file_name().unwrap_or_default().to_string_lossy().as_bytes())
            });
            dir.sort_by(|a, b| natural_cmp(a, b));
            files.extend(dir);
        } else {
            files.push(input.clone());
        }
    }
    files
}

/// A short name for the interface: the file's name without its extension.
pub fn display_name(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[..dot].to_string(),
        _ if name.is_empty() => path.to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["img10.tif", "img2.tif", "IMG1.tif", "img02b.tif", "img2.tif"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["IMG1.tif", "img2.tif", "img2.tif", "img02b.tif", "img10.tif"]);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match(b"S2_*_NDVI.tif", b"s2_20230811_ndvi.TIF"));
        assert!(wildcard_match(b"a?c", b"abc"));
        assert!(!wildcard_match(b"a?c", b"ac"));
        // a band of a file through vrt://: one input, its "?" not a wildcard
        let band = "vrt://D:/data/stack.tif?bands=3".to_string();
        assert_eq!(expand_inputs(std::slice::from_ref(&band)), [band]);
    }

    #[test]
    fn names() {
        assert_eq!(display_name(r"C:\data\landsat_medoid_1985.tif"), "landsat_medoid_1985");
        assert_eq!(display_name("/x/.hidden"), ".hidden");
    }
}
