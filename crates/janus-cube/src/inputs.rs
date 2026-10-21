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
/// them (the folder as given, then the file name), which the caches' keys depend on.
pub fn expand_inputs(inputs: &[String]) -> Vec<String> {
    let mut files = Vec::new();
    for input in inputs {
        let p = Path::new(input);
        if p.is_dir() {
            let mut dir: Vec<String> = std::fs::read_dir(p)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && is_raster_path(&e.path()))
                .map(|e| path_string(&e.path()))
                .collect();
            dir.sort_by(|a, b| natural_cmp(a, b));
            files.extend(dir);
        } else if input.contains(['*', '?']) {
            let parent = p.parent().filter(|d| !d.as_os_str().is_empty());
            let parent = parent.map(Path::to_path_buf).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let pat = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut dir: Vec<String> = std::fs::read_dir(&parent)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| {
                    e.file_type().is_ok_and(|t| t.is_file())
                        && wildcard_match(pat.as_bytes(), e.file_name().to_string_lossy().as_bytes())
                })
                .map(|e| path_string(&e.path()))
                .collect();
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
    }

    #[test]
    fn names() {
        assert_eq!(display_name(r"C:\data\landsat_medoid_1985.tif"), "landsat_medoid_1985");
        assert_eq!(display_name("/x/.hidden"), ".hidden");
    }
}
