//! Validation of maps after the good practices of Olofsson et al. (2014): the strata of a map (or of several
//! maps), the sample size and its allocation, a stratified random sample of pixels, and Stehman's (2014)
//! estimators of the accuracies and of the error-adjusted areas from the reference labels of the sample; the
//! project that keeps all of it, a GeoPackage. The interface is in janus-app (validation.rs). Ported from
//! src/validation.cpp: the same strata, the same sample for the same seed, the same project files.
//!
//! The estimators are those of zeit.accuracy, so the numbers are the same: with the strata being the classes of the
//! map assessed they reduce to Olofsson et al. (2014), eqs. 4-11; a sample stratified by one map can assess
//! another one. No finite population correction, like Zeit and R's sits_accuracy (negligible on maps of thousands
//! of pixels and more).

// NaN tests written as the C++ (!(v >= 1): NaN too)
#![allow(clippy::needless_range_loop, clippy::neg_cmp_op_on_partial_ord)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub mod assess_map;
pub mod design;
pub mod estimate;
pub mod labels;
pub mod project;
mod reader;
pub mod rules;
pub mod selftest;
pub mod strata;

pub use assess_map::{
    CHANGE, NO_CHANGE, Prepared, change_by_year, default_mapping, map_classes, prepare, python_snippet,
};
pub use design::{Alloc, Expected, Point, allocate, draw_sample, expected_errors, sample_size};
pub use estimate::{
    Agreement, AssessInput, Assessment, ClassArea, DateAgreement, Est, Estimator, assess, cohen, date_agreement, z_of,
};
pub use labels::{ADJUDICATED, Event, Label, now_iso, resolve, year_of};
pub use project::{
    AssessedMap, FinalRef, Project, append_points, create_project, is_project, load_project, new_project_id,
    read_labels, save_labels, save_project_meta,
};
pub use rules::{Grid, MapKind, MapRule, bins_text, fmt_g, interval_name, parse_bins, read_grid, row_areas};
pub use strata::{Strata, StrataSpec, Stratum, compute_strata, count_strata, mapped_areas, values_at};

/// A value outside every class.
pub const NONE: i32 = i32::MIN;

/// Background work: progress in [0, 1], cancelled when asked; `threads` for GDAL to decode and compress blocks
/// with (the Settings' processing threads).
#[derive(Debug)]
pub struct Progress {
    done: AtomicU64,
    pub cancel: AtomicBool,
    pub threads: usize,
}

impl Default for Progress {
    fn default() -> Self {
        Progress { done: AtomicU64::new(0), cancel: AtomicBool::new(false), threads: 1 }
    }
}

impl Progress {
    pub fn with_threads(threads: usize) -> Progress {
        Progress { threads: threads.max(1), ..Default::default() }
    }

    pub fn done(&self) -> f64 {
        f64::from_bits(self.done.load(Ordering::Relaxed))
    }

    pub fn set_done(&self, v: f64) {
        self.done.store(v.to_bits(), Ordering::Relaxed);
    }
}
