//! Each date's sensor in FORCE's names (`LND04` .. `LND09`, `MOD01` Terra, `MOD02` Aqua, `SEN2` any Sentinel-2 unit),
//! for the tools that depend on it (Tasseled Cap's per-sensor matrices, Zeit's `coefficients="sensor"`, and the
//! spectral adjustment's tables, Zeit's `sensor=`). Read from the scene a date came from, as Harmonize sensors does
//! (the STAC platform and item its VRT records, else the file's name), but telling Landsat 4 from 5 and 8 from 9,
//! which the harmonization does not need.

use std::path::Path;

use gdal::Metadata;
use janus_cube::CubeInfo;

/// The tools that read each date's sensor (the co-registration's composite reference of the dates of one sensor).
pub fn needs(tool: &str) -> bool {
    matches!(tool, "tasseled_cap" | "spectral_adjust" | "coregister")
}

/// The sensor of a platform name ("sentinel-2a", "landsat-8", "LANDSAT_7"), a scene id or a file name
/// ("LC09_L2SP_...", "S2B_MSIL2A_...", "HLS.L30..."), or a FORCE name; None: none known (a bare "TM" does not say
/// whether it is Landsat 4 or 5).
pub fn force_name(name: &str) -> Option<&'static str> {
    let low = name.to_ascii_lowercase();
    // the letters and digits only ("sentinel-2a" and "Sentinel_2A" alike), and the words
    let compact: String = low.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let words: Vec<&str> = low.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let first = words.first().copied().unwrap_or("");
    let word = |w: &str| words.contains(&w);
    let has = |keys: &[&str]| keys.iter().any(|k| compact.contains(k));
    let one_of = |names: &[&str]| names.contains(&first);
    // FORCE's own names, alone
    for (n, force) in [("4", "LND04"), ("5", "LND05"), ("7", "LND07"), ("8", "LND08"), ("9", "LND09")] {
        if compact == format!("lnd0{n}") || compact == format!("lnd{n}") {
            return Some(force);
        }
    }
    if compact == "mod01" || compact == "mod1" {
        return Some("MOD01");
    }
    if compact == "mod02" || compact == "mod2" {
        return Some("MOD02");
    }
    // HLS: its Sentinel-2 product, and its Landsat one (OLI)
    if first == "hls" {
        match words.get(1).copied() {
            Some("s30") => return Some("SEN2"),
            Some("l30") => return Some("LND08"),
            _ => {}
        }
    }
    if has(&["sentinel2"]) || one_of(&["s2", "s2a", "s2b", "s2c", "s2d"]) || word("msi") {
        return Some("SEN2");
    }
    if has(&["landsat9", "landsat09"]) || one_of(&["lc09", "lc9", "lo09"]) || word("oli2") {
        return Some("LND09");
    }
    if has(&["landsat8", "landsat08"]) || one_of(&["lc08", "lc8", "lo08"]) || word("oli") {
        return Some("LND08");
    }
    if has(&["landsat7", "landsat07"]) || one_of(&["le07", "le7"]) || word("etm") {
        return Some("LND07");
    }
    if has(&["landsat5", "landsat05"]) || one_of(&["lt05", "lt5"]) {
        return Some("LND05");
    }
    if has(&["landsat4", "landsat04"]) || one_of(&["lt04", "lt4"]) {
        return Some("LND04");
    }
    if word("terra") {
        return Some("MOD01");
    }
    if word("aqua") || first.starts_with("myd") {
        return Some("MOD02");
    }
    if first.starts_with("mod") && first.len() >= 5 {
        return Some("MOD01");
    }
    None
}

/// The sensor of every date of a series ("" where none is known): from the STAC platform and first item its VRT
/// records, else the date's file name.
pub fn of_series(info: &CubeInfo) -> Vec<String> {
    (0..info.t())
        .map(|t| {
            let Some(layer) = info.layers.get(t) else { return String::new() };
            let mut names: Vec<String> = Vec::new();
            if let Ok(ds) = gdal::Dataset::open(&layer.path) {
                names.extend(ds.metadata_item("STAC_PLATFORM", ""));
                names.extend(
                    ds.metadata_item("STAC_ITEMS", "").and_then(|s| s.split_whitespace().next().map(String::from)),
                );
            }
            names.extend(Path::new(&layer.path).file_name().map(|f| f.to_string_lossy().into_owned()));
            names.iter().find_map(|n| force_name(n)).map_or_else(String::new, String::from)
        })
        .collect()
}

/// The sensors a tool of `tool` needs of a series (None for the others).
pub fn for_tool(tool: &str, info: &CubeInfo) -> Option<Vec<String>> {
    needs(tool).then(|| of_series(info))
}

/// The date of index `t` as a message names it: its ISO date, else its number.
pub(crate) fn date_text(input: &crate::pixel::PixelInput, t: usize) -> String {
    match input.days.as_ref().and_then(|d| d.get(t)) {
        Some(&d) => {
            let (y, m, dd) = crate::numeric::civil(d);
            format!("{y:04}-{m:02}-{dd:02}")
        }
        None => format!("date {}", t + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_say_the_sensor() {
        for (name, want) in [
            ("sentinel-2a", "SEN2"),
            ("Sentinel_2B", "SEN2"),
            ("S2B_MSIL2A_20200101T000000_N0213_R043_T22KDV_20200101T000000", "SEN2"),
            ("landsat-8", "LND08"),
            ("LANDSAT_9", "LND09"),
            ("LC09_L2SP_222082_20220101_20220102_02_T1_SR", "LND09"),
            ("LC08_L2SP_222082_20200101_20200102_02_T1_SR", "LND08"),
            ("LE07_L2SP_222082_20200101_20200102_02_T1_SR", "LND07"),
            ("LT05_L2SP_222082_20100101_20200102_02_T1_SR", "LND05"),
            ("LT04_L2SP_222082_19900101_20200102_02_T1_SR", "LND04"),
            ("landsat-7", "LND07"),
            ("Landsat 5", "LND05"),
            ("lnd08", "LND08"),
            ("LND05", "LND05"),
            ("HLS.S30.T22KDV.2020001.v2.0", "SEN2"),
            ("HLS.L30.T22KDV.2020001.v2.0", "LND08"),
            ("terra", "MOD01"),
            ("MOD09GA.A2020001.h12v10", "MOD01"),
            ("MYD09GA.A2020001.h12v10", "MOD02"),
            ("aqua", "MOD02"),
            ("MOD02", "MOD02"),
        ] {
            assert_eq!(force_name(name), Some(want), "{name}");
        }
        for name in ["TM", "scene_2020_01_01.tif", "", "sentinel-1"] {
            assert_eq!(force_name(name), None, "{name}");
        }
    }

    /// A one-pixel GeoTIFF at `path`.
    fn tiff(path: &Path) {
        let driver = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        driver.create_with_band_type::<f32, _>(path, 1, 1, 1).unwrap();
    }

    /// A VRT of `source` carrying `metadata` (as the series built from a STAC search record theirs).
    fn vrt(path: &Path, source: &str, metadata: &[(&str, &str)]) {
        let mdi: String = metadata.iter().map(|(k, v)| format!("<MDI key=\"{k}\">{v}</MDI>")).collect();
        std::fs::write(
            path,
            format!(
                "<VRTDataset rasterXSize=\"1\" rasterYSize=\"1\"><Metadata>{mdi}</Metadata>\
                 <VRTRasterBand dataType=\"Float32\" band=\"1\"><SimpleSource>\
                 <SourceFilename relativeToVRT=\"1\">{source}</SourceFilename><SourceBand>1</SourceBand>\
                 </SimpleSource></VRTRasterBand></VRTDataset>"
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_series_sensors_come_from_the_files_or_the_stac_metadata() {
        let dir = std::env::temp_dir().join("janus-zeit-sensors-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // by file name: Landsat 9, 7 and 5, a name that says nothing
        for name in ["LC09_L2SP_20220101_SR.tif", "LE07_L2SP_20200101_SR.tif", "LT05_L2SP_20100101_SR.tif", "scene.tif"]
        {
            tiff(&dir.join(name));
        }
        // by the VRT's STAC platform (over a file whose name says nothing), by its first item, and the platform
        // before the item
        vrt(&dir.join("platform.vrt"), "scene.tif", &[("STAC_PLATFORM", "landsat-8")]);
        vrt(
            &dir.join("item.vrt"),
            "scene.tif",
            &[
                ("STAC_COLLECTION", "sentinel-2-l2a"),
                ("STAC_ITEMS", "S2B_23KPQ_20200701_0_L2A S2A_23KPQ_20200701_1_L2A"),
            ],
        );
        vrt(
            &dir.join("both.vrt"),
            "scene.tif",
            &[("STAC_PLATFORM", "Sentinel-2A"), ("STAC_ITEMS", "LC08_L2SP_20200101")],
        );
        let names = [
            "LC09_L2SP_20220101_SR.tif",
            "LE07_L2SP_20200101_SR.tif",
            "LT05_L2SP_20100101_SR.tif",
            "scene.tif",
            "platform.vrt",
            "item.vrt",
            "both.vrt",
            "LC08_missing.tif",
        ];
        let info = CubeInfo {
            layers: names
                .iter()
                .map(|n| janus_cube::Layer { path: dir.join(n).to_string_lossy().into_owned(), ..Default::default() })
                .collect(),
            ..Default::default()
        };
        // a file that cannot be opened is still read by its name
        assert_eq!(of_series(&info), ["LND09", "LND07", "LND05", "", "LND08", "SEN2", "SEN2", "LND08"]);
        assert_eq!(for_tool("tasseled_cap", &info), Some(of_series(&info)));
        assert_eq!(for_tool("spectral_adjust", &info).map(|s| s.len()), Some(8));
        assert_eq!(for_tool("ndfi", &info), None);
        let input = crate::pixel::PixelInput { days: Some(vec![18262, 18300]), ..Default::default() };
        assert_eq!(date_text(&input, 1), "2020-02-08");
        assert_eq!(date_text(&input, 5), "date 6");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
