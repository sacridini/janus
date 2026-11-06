//! The tools' descriptors: what the bridge's `hello` sent (each tool_*.py's MANIFEST), kept in tools.json. The
//! forms of the Zeit window are made from them; tests/bridge.rs checks they are still the bridge's.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Manifest {
    pub protocol: i32,
    /// the Zeit the descriptors were taken from
    pub zeit_version: String,
    pub tools: Vec<Tool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    /// "series" (the default) or "layers": maps Janus shows instead of the series
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    pub requires: Requires,
    /// "pixel", "raster"
    pub modes: Vec<String>,
    pub params: Vec<Param>,
    pub outputs: Vec<Output>,
    /// pixels per chunk of a raster run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_cells: Option<i64>,
    /// fitted again on the chart when the map's date changes (CCDC's synthetic image)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_date: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Requires {
    /// "any", "annual", "regular", or "spectrum" (a scene of one band per wavelength, not a series)
    pub time: String,
    pub min_dates: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_per_year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional_bands: Option<Vec<String>>,
    /// every band of each date, whatever its role (spectral unmixing's endmembers)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_bands: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Param {
    pub id: String,
    pub label: String,
    /// int, float, bool, enum, multi (an enum of which several can be chosen: the value is an array of options),
    /// patterns, layers
    #[serde(rename = "type")]
    pub kind: String,
    pub default: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    pub help: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Output {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colormap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// a series of the run's dates, opened as a new layer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<bool>,
    /// a series opened hidden, under the maps (to hover for its pixels' series): the run's maps stay in view
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// "int16" with `scale`: stored in 2 bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// polygons in a GeoPackage (SNIC's segments), opened as a vector layer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<bool>,
    /// a bool parameter that must be on for it to be written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classes: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classes_param: Option<String>,
    /// one output per entry of this list parameter (an endmember's fraction): the id `{id}_{k}` (from 1), the
    /// name with `{}` as the entry's name; for a "multi" parameter, the id `{id}_{option}` and the name with `{}`
    /// as the option's label
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<String>,
    /// the tool decides the output's bands once per job, with their labels (the band descriptions): not the
    /// run's dates (an interpolation's new grid, years, months); the chunk's [band][pixels] has that many bands
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<bool>,
}

/// The Tools menu's categories, by the question asked and in the order of a workflow: the data prepared, its
/// spectra, its dynamics through time, its changes, its patterns in space, its classes (each tool's `category`).
pub const CATEGORIES: [&str; 6] = [
    DATA_PREPARATION,
    "Spectral analysis",
    "Temporal dynamics",
    "Change detection",
    "Spatial pattern analysis",
    CLASSIFICATION,
];
/// The category Janus' own preparations join (regularize, speckle filter, reproject).
pub const DATA_PREPARATION: &str = "Data preparation";
/// The category the Classification window and the field delineation join.
pub const CLASSIFICATION: &str = "Classification & pattern recognition";

/// Every tool's descriptor (read once).
pub fn manifest() -> &'static Manifest {
    static M: OnceLock<Manifest> = OnceLock::new();
    M.get_or_init(|| serde_json::from_str(include_str!("tools.json")).expect("tools.json"))
}

impl Manifest {
    pub fn tool(&self, id: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multi_parameter_and_an_output_with_bands_round_trip() {
        let tool: Tool = serde_json::from_value(serde_json::json!({
            "id": "t", "name": "T", "category": "c", "description": "", "requires": { "time": "any", "min_dates": 1 },
            "modes": ["raster"],
            "params": [{ "id": "m", "label": "M", "type": "multi", "default": ["a", "b"], "options": ["a", "b", "c"],
                         "labels": ["A", "B", "C"], "help": "" }],
            "outputs": [{ "id": "s", "name": "S", "series": true, "bands": true }],
        }))
        .unwrap();
        assert_eq!(tool.params[0].kind, "multi");
        assert_eq!(tool.params[0].default, serde_json::json!(["a", "b"]));
        assert_eq!(tool.outputs[0].bands, Some(true));
        let again: Tool = serde_json::from_str(&serde_json::to_string(&tool).unwrap()).unwrap();
        assert_eq!(again, tool);
    }

    #[test]
    fn the_interpolation_descriptor_has_its_own_bands() {
        let t = manifest().tool("interpolate").unwrap();
        assert_eq!(t.outputs.len(), 1);
        assert_eq!((t.outputs[0].series, t.outputs[0].bands), (Some(true), Some(true)));
    }
}
