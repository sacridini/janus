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
    /// "any", "annual" or "regular"
    pub time: String,
    pub min_dates: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_per_year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional_bands: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Param {
    pub id: String,
    pub label: String,
    /// int, float, bool, enum, patterns, layers
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
}

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
