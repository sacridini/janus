//! A map without a series: when only vector layers are open, the map stands on a "canvas" of their own, an empty
//! raster (one Byte band, no data everywhere) in the vectors' CRS over their extent, opened as a hidden layer.
//! Everything the map does on a series then works for the vectors alone: placing them, zooming out to the CRS's
//! extent, the basemap, exports. The canvas goes as soon as a series is opened (the map then stands on it) or the
//! last vector layer is removed.

use std::path::PathBuf;

use crate::app::State;

/// The canvas' longer side, in pixels.
const SIDE: usize = 2048;

/// The folder of the canvases.
fn dir() -> PathBuf {
    crate::settings::app_data_dir().join("canvas")
}

/// Whether a layer's files are a canvas.
pub fn is_canvas(inputs: &[String]) -> bool {
    let d = dir();
    inputs.len() == 1 && std::path::Path::new(&inputs[0]).parent().is_some_and(|p| p == d)
}

/// The empty raster under vectors: their bounding box (in their CRS, or longitudes and latitudes when they have
/// none) with a tenth of it around, square pixels.
fn write_canvas(bbox: [f64; 4], crs_wkt: &str, n: u64) -> Result<String, String> {
    let [x0, y0, x1, y1] = bbox;
    // a point or a line along an axis: a small area around it
    let geographic =
        crs_wkt.is_empty() || gdal::spatial_ref::SpatialRef::from_wkt(crs_wkt).is_ok_and(|s| s.is_geographic());
    let least = if geographic { 0.01 } else { 1000.0 };
    let (w, h) = ((x1 - x0).max(least), (y1 - y0).max(least));
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (w, h) = (w * 1.2, h * 1.2);
    let res = w.max(h) / SIDE as f64;
    let (pw, ph) = (((w / res).ceil() as usize).max(1), ((h / res).ceil() as usize).max(1));
    let _ = std::fs::create_dir_all(dir());
    let path = dir().join(format!("map_{n}.tif"));
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let opts = gdal::raster::RasterCreationOptions::from_iter(["COMPRESS=DEFLATE", "TILED=YES", "SPARSE_OK=TRUE"]);
    let mut ds = drv.create_with_band_type_with_options::<u8, _>(&path, pw, ph, 1, &opts).map_err(|e| e.to_string())?;
    ds.set_geo_transform(&[cx - pw as f64 * res / 2.0, res, 0.0, cy + ph as f64 * res / 2.0, 0.0, -res])
        .map_err(|e| e.to_string())?;
    let wkt = if crs_wkt.is_empty() {
        gdal::spatial_ref::SpatialRef::from_epsg(4326).and_then(|s| s.to_wkt()).unwrap_or_default()
    } else {
        crs_wkt.to_string()
    };
    ds.set_projection(&wkt).map_err(|e| e.to_string())?;
    let mut band = ds.rasterband(1).map_err(|e| e.to_string())?;
    band.set_no_data_value(Some(0.0)).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

impl State {
    /// The canvas layer's index, if one is open.
    pub fn canvas_index(&self) -> Option<usize> {
        self.layers.iter().position(|l| l.canvas)
    }

    /// Whether the active layer is the canvas (the map has vector layers only).
    pub fn vectors_only(&self) -> bool {
        self.layers.get(self.active).is_some_and(|l| l.canvas)
    }

    /// Vector layers without a series: a canvas made under them; their last one removed: the canvas closed.
    pub(crate) fn pump_canvas(&mut self, ctx: &egui::Context) {
        if self.vectors.is_empty() {
            if let Some(i) = self.canvas_index() {
                self.remove_layer(i, ctx);
            }
            return;
        }
        if !self.layers.is_empty() || self.opening.is_some() || !self.pending_opens.is_empty() {
            return;
        }
        // their extent: in the first one's CRS (the others are placed on it as on a series)
        let first = &self.vectors[0].data;
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for f in &first.features {
            for ring in &f.parts {
                for p in ring.chunks(2) {
                    if p.len() == 2 && p[0].is_finite() && p[1].is_finite() {
                        b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
                    }
                }
            }
        }
        if b[0] > b[2] {
            return; // no geometry
        }
        self.canvas_count += 1;
        match write_canvas(b, &first.crs_wkt, self.canvas_count) {
            Ok(path) => {
                self.basemap.ui.on = self.basemap.ui.source != "none";
                self.open_inputs(vec![path], true, ctx);
            }
            Err(e) => self.error_popup = Some(format!("could not make a map for the vector layers: {e}")),
        }
    }
}
