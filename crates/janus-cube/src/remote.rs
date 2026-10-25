//! Paths as GDAL opens them (src/cube.cpp): URLs and cloud buckets read in place through GDAL's network file
//! systems, subdataset names (NETCDF:"f.nc":t2m) and the file holding them, the names shown for them.

use std::path::Path;

/// The file (or URL) holding a dataset: the path inside a subdataset name (NETCDF:"f.nc":t2m -> f.nc), else the
/// input itself. A Windows drive letter ("C:\...") has a single character before the colon.
pub fn dataset_file(path: &str) -> &str {
    if let Some(colon) = path.find(':')
        && colon > 1
        && path[colon + 1..].starts_with('"')
        && let Some(end) = path[colon + 2..].find('"')
    {
        return &path[colon + 2..colon + 2 + end];
    }
    path
}

/// Cloud-native access: a URL (http(s)://, s3://, gs://, az://) or a path in one of GDAL's network file systems
/// (/vsicurl/, /vsis3/...), also inside a subdataset name.
pub fn is_remote_path(path: &str) -> bool {
    let p = dataset_file(path).to_ascii_lowercase();
    ["http://", "https://", "ftp://", "s3://", "gs://", "az://"].iter().any(|u| p.starts_with(u))
        || ["/vsicurl", "/vsis3", "/vsigs", "/vsiaz", "/vsiadls", "/vsioss", "/vsiswift", "/vsiwebhdfs"]
            .iter()
            .any(|v| p.contains(v))
}

/// What GDAL opens for an input: a URL becomes its network file system path (https://... -> /vsicurl/https://...,
/// s3://b/k -> /vsis3/b/k; a remote Zarr store also gets the ZARR: prefix); anything else is returned as is.
pub fn gdal_path(input: &str) -> String {
    let l = input.to_ascii_lowercase();
    let p = if l.starts_with("http://") || l.starts_with("https://") || l.starts_with("ftp://") {
        format!("/vsicurl/{input}")
    } else if l.starts_with("s3://") {
        format!("/vsis3/{}", &input[5..])
    } else if l.starts_with("gs://") {
        format!("/vsigs/{}", &input[5..])
    } else if l.starts_with("az://") {
        format!("/vsiaz/{}", &input[5..])
    } else {
        return input.to_string();
    };
    // a remote Zarr store is a folder: GDAL needs to be told the driver
    let bare = l.split('?').next().unwrap_or("").trim_end_matches('/');
    if bare.ends_with(".zarr") { format!("ZARR:\"{p}\"") } else { p }
}

/// A Zarr store: a folder named *.zarr or holding Zarr's metadata files.
pub fn is_zarr_store(p: &Path) -> bool {
    p.is_dir()
        && (p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zarr"))
            || [".zgroup", ".zarray", "zarr.json"].iter().any(|f| p.join(f).exists()))
}

/// A short name for the interface: the file's stem (a URL's, without its query or network prefix), followed by
/// the variable of a subdataset ("clim t2m").
pub fn display_name(path: &str) -> String {
    let mut file = dataset_file(path).to_string();
    let mut var = String::new();
    if file != path {
        let colon = path.find(':').unwrap_or(0);
        if let Some(end) = path[colon + 2..].find('"').map(|e| colon + 2 + e)
            && end + 1 < path.len()
        {
            var = path[end + 1..].trim_start_matches([':', '/']).to_string();
        }
    }
    if is_remote_path(&file) {
        // a signed URL's token
        file = file.split('?').next().unwrap_or("").trim_end_matches('/').to_string();
    }
    let mut name = file.rsplit(['/', '\\']).next().unwrap_or(&file).to_string();
    if let Some(dot) = name.rfind('.')
        && dot > 0
    {
        name.truncate(dot);
    }
    if name.is_empty() {
        name = file;
    }
    if var.is_empty() { name } else { format!("{name} {var}") }
}

/// An EnMAP product's spectral image (ENMAP01-...-SPECTRAL_IMAGE.TIF/.BSQ...): opening its folder opens it alone.
pub fn is_enmap_spectral_image(path: &Path) -> bool {
    let n = path.file_name().map(|n| n.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
    n.starts_with("ENMAP") && n.contains("SPECTRAL_IMAGE") && crate::inputs::is_raster_path(path)
}

/// GDAL's options for network reads (retries, larger block cache, merged ranges, anonymous S3/GCS when no
/// credentials are configured), unless the environment sets them. Called once at startup.
pub fn configure_remote_access() {
    use gdal::config::{get_config_option, set_config_option};
    let set_default = |k: &str, v: &str| {
        if get_config_option(k, "").unwrap_or_default().is_empty() {
            let _ = set_config_option(k, v);
        }
    };
    set_default("GDAL_HTTP_MAX_RETRY", "3");
    set_default("GDAL_HTTP_RETRY_DELAY", "1");
    set_default("GDAL_HTTP_MERGE_CONSECUTIVE_RANGES", "YES");
    set_default("CPL_VSIL_CURL_CACHE_SIZE", "268435456"); // 256 MB of downloaded blocks (default 16 MB)
    set_default("CPL_VSIL_CURL_CHUNK_SIZE", "65536");
    // remote files opened by others than open_raster (the sources of a VRT: a STAC series) are not looked for
    // side-cars either: a listing of their "folder" can take seconds a file
    for prefix in ["/vsicurl/", "/vsis3/", "/vsigs/", "/vsiaz/"] {
        if get_config_option("GDAL_DISABLE_READDIR_ON_OPEN", "").unwrap_or_default().is_empty() {
            janus_gdal::set_path_option(prefix, "GDAL_DISABLE_READDIR_ON_OPEN", Some("EMPTY_DIR"));
        }
    }
    // Public buckets (Landsat, Sentinel-2 COGs...) without an account: unsigned requests unless credentials are
    // configured.
    let has = |k: &str| !get_config_option(k, "").unwrap_or_default().is_empty();
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    let aws_files = home.is_some_and(|h| {
        let h = Path::new(&h).join(".aws");
        h.join("credentials").exists() || h.join("config").exists()
    });
    if !(has("AWS_ACCESS_KEY_ID") || has("AWS_PROFILE") || has("AWS_DEFAULT_PROFILE") || aws_files) {
        set_default("AWS_NO_SIGN_REQUEST", "YES");
    }
    if !(has("GOOGLE_APPLICATION_CREDENTIALS") || has("GS_SECRET_ACCESS_KEY") || has("GS_OAUTH2_REFRESH_TOKEN")) {
        set_default("GS_NO_SIGN_REQUEST", "YES");
    }
}

/// Whether a local VRT reads files over the network (its sources' paths in GDAL's network file systems or URLs):
/// the first 64 KB of it looked at.
pub fn vrt_reads_network(path: &str) -> bool {
    if !path.to_ascii_lowercase().ends_with(".vrt") || is_remote_path(path) {
        return false;
    }
    let Ok(f) = std::fs::File::open(path) else { return false };
    let mut text = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::Read::take(f, 1 << 16), &mut text);
    ["/vsicurl/", "/vsis3/", "/vsigs/", "/vsiaz/", "/vsiadls/", "/vsioss/", "/vsiswift/", "http://", "https://"]
        .iter()
        .any(|p| text.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(dataset_file(r#"NETCDF:"C:\d\clim.nc":t2m"#), r"C:\d\clim.nc");
        assert_eq!(dataset_file(r"C:\d\a.tif"), r"C:\d\a.tif");
        assert_eq!(gdal_path("s3://bucket/key.tif"), "/vsis3/bucket/key.tif");
        assert_eq!(gdal_path("https://h/x.zarr/"), "ZARR:\"/vsicurl/https://h/x.zarr/\"");
        assert!(is_remote_path("/vsicurl/https://h/a.tif"));
        assert!(!is_remote_path(r"D:\data\a.tif"));
        assert_eq!(display_name(r#"NETCDF:"clim.nc":t2m"#), "clim t2m");
        assert_eq!(display_name("/vsicurl/https://h/S2_B04.tif?sig=1"), "S2_B04");
    }
}
