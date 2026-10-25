//! The series as a VRT of one band per date (src/cube.cpp, writeCubeVrt): what a Zeit run reads.

use crate::{BandMeta, CubeInfo, band_meta, open_raster};

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// C's %.17g: every digit of a double.
fn g17(v: f64) -> String {
    crate::multidim::format_g(v, 17)
}

/// Writes a VRT of one Float32 band per date at `path`: the shown band of each date, or band `source_band` of
/// each date (> 0); with the files' scale, offset and NoData applied (NoData as NaN), unless `raw` (the quality
/// band's codes as they are).
pub fn write_cube_vrt(info: &CubeInfo, path: &str, source_band: i32, raw: bool) -> Result<(), String> {
    let srs = dataset_srs(info);
    if source_band <= 0
        && let Some(e) = &info.expr
    {
        return write_expr_vrt(info, e, &srs, path);
    }
    let mut x = format!("<VRTDataset rasterXSize=\"{}\" rasterYSize=\"{}\">\n", info.width, info.height);
    if !srs.is_empty() {
        x += &format!("  <SRS>{}</SRS>\n", xml_escape(&srs));
    }
    if let Some(g) = info.geo_transform {
        x += &format!(
            "  <GeoTransform>{}, {}, {}, {}, {}, {}</GeoTransform>\n",
            g17(g[0]),
            g17(g[1]),
            g17(g[2]),
            g17(g[3]),
            g17(g[4]),
            g17(g[5])
        );
    }
    for (t, l) in info.layers.iter().enumerate() {
        let mut src_band = l.band;
        let mut m = l.meta;
        if source_band > 0 && source_band + l.band_base != l.band {
            // another band of the date
            src_band = source_band + l.band_base;
            m = if source_band == info.sel.nd_band {
                l.nd_meta
            } else {
                open_raster(&l.path)
                    .ok()
                    .and_then(|ds| ds.rasterband(src_band as usize).ok().map(|b| band_meta(&b)))
                    .unwrap_or_default()
            };
        }
        if raw {
            m = BandMeta::default();
        }
        let scaled = m.scale != 1.0 || m.offset != 0.0;
        let source = if scaled || m.no_data.is_some() { "ComplexSource" } else { "SimpleSource" };
        x += &format!("  <VRTRasterBand dataType=\"Float32\" band=\"{}\">\n", t + 1);
        x += &format!("    <Description>{}</Description>\n", xml_escape(&l.label));
        if !raw {
            x += "    <NoDataValue>nan</NoDataValue>\n";
        }
        x += &format!("    <{source}>\n");
        x += &format!("      <SourceFilename relativeToVRT=\"0\">{}</SourceFilename>\n", xml_escape(&l.path));
        x += &format!("      <SourceBand>{src_band}</SourceBand>\n");
        if scaled {
            x += &format!("      <ScaleOffset>{}</ScaleOffset>\n", g17(m.offset));
            x += &format!("      <ScaleRatio>{}</ScaleRatio>\n", g17(m.scale));
        }
        if let Some(nd) = m.no_data {
            x += &format!("      <NODATA>{}</NODATA>\n", g17(nd));
        }
        x += &format!("    </{source}>\n");
        x += "  </VRTRasterBand>\n";
    }
    x += "</VRTDataset>\n";
    std::fs::write(path, x).map_err(|e| format!("could not write {path}: {e}"))
}

/// The WKT of a cube's first file (its CRS as assumed when the file has none).
pub fn dataset_srs(info: &CubeInfo) -> String {
    let srs = open_raster(&info.first_path)
        .ok()
        .and_then(|ds| ds.spatial_ref().ok())
        .and_then(|s| s.to_wkt().ok())
        .unwrap_or_default();
    // assumed (a lon/lat NetCDF without a grid mapping)
    if srs.is_empty() { info.crs_wkt.clone() } else { srs }
}

/// The shown series of an expression: a derived band per date over the bands it reads (the calculator's pixel
/// function), what Zeit reads.
fn write_expr_vrt(info: &CubeInfo, e: &crate::calc::LayerExpr, srs: &str, path: &str) -> Result<(), String> {
    use crate::calc::vrt::{Band, Grid, Source, block_side, date_args, register, vrt_xml};
    register();
    let expr = crate::calc::canonical::canonical(&e.program);
    let mut metas: std::collections::HashMap<(String, i32), BandMeta> = std::collections::HashMap::new();
    let mut bands = Vec::with_capacity(info.t());
    for (t, l) in info.layers.iter().enumerate() {
        let mut sources = Vec::new();
        for &b in &e.bands {
            let fb = b + l.band_base;
            let meta = if fb == l.band {
                l.meta
            } else {
                *metas.entry((l.path.clone(), fb)).or_insert_with(|| {
                    open_raster(&l.path)
                        .ok()
                        .and_then(|ds| ds.rasterband(fb as usize).ok().map(|x| band_meta(&x)))
                        .unwrap_or_default()
                })
            };
            let path = l.path.clone();
            sources.push(Some(Source { path, band: fb, meta, raw: false, rects: None, resampling: None }));
        }
        let vars = crate::calc::plan::date_vars(info, t);
        bands.push(Band { description: l.label.clone(), sources, args: date_args(&expr, e.bands.len(), &vars) });
    }
    let grid = Grid { w: info.width, h: info.height, gt: info.geo_transform, srs: srs.to_string() };
    let x = vrt_xml(&grid, &bands, block_side(e.bands.len()));
    std::fs::write(path, x).map_err(|e| format!("could not write {path}: {e}"))
}
