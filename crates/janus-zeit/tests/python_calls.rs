//! "Copy Python" of the FORCE time-series tools: each has an entry in `calls`, naming the Zeit function and
//! passing the tool's parameters (as tools/tsa_fixtures.py calls them).

use std::collections::BTreeMap;

use janus_zeit::python::calls;
use janus_zeit::{Params, PixelInput};
use serde_json::{Value, json};

fn code(tool: &str, given: &[(&str, Value)]) -> String {
    let given: BTreeMap<String, Value> = given.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let p = Params::with_defaults(tool, &given).unwrap();
    let input = PixelInput {
        years: vec![2020.0, 2020.5],
        days: Some(vec![18300, 18400]),
        values: vec![0.1, 0.2],
        ..Default::default()
    };
    let (code, inputs) = calls(tool, &p, &input).unwrap_or_else(|| panic!("{tool}: no Python calls"));
    assert_eq!(inputs, vec!["series"], "{tool}");
    code
}

#[test]
fn the_time_series_tools_have_python_calls() {
    let c = code("interpolate", &[("method", json!("moving")), ("step", json!(9)), ("window", json!(7.0))]);
    assert!(c.contains("zeit.interpolate(series, 'moving', step=9, window=7.0"), "{c}");
    let c = code("interpolate", &[("method", json!("rbf")), ("sigma", json!("4,8,16")), ("cutoff", json!(0.9))]);
    assert!(c.contains("sigma=[4.0, 8.0, 16.0], cutoff=0.9"), "{c}");
    let c = code("interpolate", &[("method", json!("harmonic")), ("modes", json!(2)), ("trend", json!(false))]);
    assert!(c.contains("modes=2, trend=False"), "{c}");

    let c = code("temporal_metrics", &[("metrics", json!(["median", "p10"]))]);
    assert!(c.contains("zeit.temporal_metrics(series, ['median', 'p10']"), "{c}");
    let c = code("fold", &[("metrics", json!(["mean"])), ("by", json!("month"))]);
    assert!(c.contains("zeit.temporal_metrics(series, ['mean'], by='month'"), "{c}");
    let c = code("standardize", &[("method", json!("center"))]);
    assert!(c.contains("zeit.standardize(series, 'center'"), "{c}");

    let c = code("linear_trend", &[("by", json!("quarter")), ("conf", json!(0.9)), ("tail", json!("left"))]);
    assert!(
        c.contains("by='quarter'") && c.contains("zeit.linear_trend(result1, dim='quarter', conf=0.9, tail='left'"),
        "{c}"
    );
    let c = code("cat", &[("by", json!("year")), ("penalty", json!(true))]);
    assert!(c.contains("zeit.cat(result1, penalty=True, dim='year'"), "{c}");

    let c = code("clear_observations", &[("months", json!(3)), ("statistics", json!(["count", "max"]))]);
    assert!(c.contains("zeit.clear_observations(series, months=3, metrics=['count', 'max']"), "{c}");
    let c =
        code("polar_metrics", &[("metrics", json!(["VSS", "IGS"])), ("adaptive", json!(false)), ("mid", json!(0.4))]);
    assert!(
        c.contains("zeit.polar_metrics(series, ['VSS', 'IGS'], start=0.2, mid=0.4, end=0.8, adaptive=False"),
        "{c}"
    );
}

#[test]
fn eof_and_lagged_correlation_have_python_calls() {
    let c = code(
        "eof",
        &[("n_modes", json!(5)), ("rotate", json!(3)), ("north_n", json!("dates")), ("weights", json!("none"))],
    );
    assert!(c.contains("zeit.eof(series, n_modes=5, anomaly='month', harmonics=2, weights='none'"), "{c}");
    assert!(c.contains("rotate=3, kaiser=True, north_n='dates', correlation_maps=False, seed=42"), "{c}");
    // the compact call leaves out what Zeit's default is: of the defaults, only North's n differs
    let input = PixelInput { years: vec![2020.0], days: Some(vec![18300]), values: vec![0.1], ..Default::default() };
    let p = Params::with_defaults("eof", &BTreeMap::new()).unwrap();
    let call = janus_zeit::python::tool_call("eof", &p, &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.eof(series, north_n='autocorrelation')");

    // the driver: another series (its own cube), its mean over the window, or a CSV
    let given = |v: &[(&str, Value)]| -> Params {
        Params::with_defaults("lagged_correlation", &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect())
            .unwrap()
    };
    let (c, inputs) =
        calls("lagged_correlation", &given(&[("lags", json!(6)), ("method", json!("spearman"))]), &input).unwrap();
    assert_eq!(inputs, vec!["series", "other_cube"]);
    assert!(
        c.contains("zeit.lagged_correlation(series, other_cube, lags=6, step='month', match='nearest', tolerance='8D'"),
        "{c}"
    );
    assert!(
        c.contains("method='spearman', best='absolute', min_pairs=10, effective_n=True, sidak=True, alpha_fdr=0.1"),
        "{c}"
    );
    let (c, inputs) = calls("lagged_correlation", &given(&[("y_mode", json!("window_mean"))]), &input).unwrap();
    assert_eq!(inputs, vec!["series", "other_cube"]);
    assert!(c.contains("zeit.lagged_correlation(series, other_cube.mean(dim=('y', 'x')), lags=3"), "{c}");
    let (c, inputs) = calls(
        "lagged_correlation",
        &given(&[("y_mode", json!("file")), ("y_file", json!("oni.csv")), ("tolerance", json!(12.5))]),
        &input,
    )
    .unwrap();
    assert_eq!(inputs, vec!["series"]);
    assert!(c.starts_with("driver = pd.read_csv('oni.csv', index_col=0, parse_dates=True).iloc[:, 0]"), "{c}");
    assert!(
        c.contains("zeit.lagged_correlation(series, driver, lags=3, step='month', match='nearest', tolerance='12.5D'"),
        "{c}"
    );
    // the defaults leave out of the compact call everything but the driver
    let call = janus_zeit::python::tool_call("lagged_correlation", &given(&[]), &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.lagged_correlation(series, other_cube)");
}

#[test]
fn mnf_cva_imad_and_radcal_have_python_calls() {
    let given = |tool: &str, v: &[(&str, Value)]| -> Params {
        Params::with_defaults(tool, &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect()).unwrap()
    };
    let input = PixelInput {
        years: vec![2019.0, 2020.0, 2021.0],
        days: Some(vec![17900, 18300, 18700]),
        values: vec![0.1, 0.2, 0.3],
        band_numbers: [("red", 3), ("nir", 4), ("swir1", 5)].iter().map(|(r, b)| (r.to_string(), *b)).collect(),
        ..Default::default()
    };
    // MNF of a date: the components, then both
    let (c, inputs) =
        calls("mnf", &given("mnf", &[("date", json!(2020.2)), ("n_components", json!(4))]), &input).unwrap();
    assert_eq!(inputs, vec!["bands_all"]);
    assert!(c.starts_with("result1 = zeit.mnf(bands_all.isel(time=1), 4, noise='diff_hv', scale='noise'"), "{c}");
    let (c, _) = calls(
        "mnf",
        &given(
            "mnf",
            &[("scope", json!("all_dates")), ("output", json!("both")), ("k", json!(3)), ("bands", json!("1, 2,nir"))],
        ),
        &input,
    )
    .unwrap();
    assert!(
        c.starts_with("result1, transform = zeit.mnf(bands_all, None, noise='diff_hv', bands=['b001', 'b002', 'nir']"),
        "{c}"
    );
    assert!(
        c.contains("return_transform=True")
            && c.contains("result2 = zeit.mnf_denoise(bands_all, k=3, transform=transform"),
        "{c}"
    );
    let (c, _) = calls("mnf", &given("mnf", &[("output", json!("denoised"))]), &input).unwrap();
    assert!(c.contains("zeit.mnf_denoise(bands_all.isel(time=2), snr_min=1.0, noise='diff_hv'"), "{c}");
    // CVA: the two dates by index, the sign sectors with the bands' names in bands_all
    let (c, _) = calls("cva", &given("cva", &[("threshold", json!("em"))]), &input).unwrap();
    assert!(
        c.contains(
            "zeit.cva(bands_all, dates=(0, 2), roles={'red': 'b003', 'nir': 'b004', 'swir1': 'b005'}, direction='sign'"
        ),
        "{c}"
    );
    assert!(c.contains("threshold='em', em_init=0.2, sample=1000000"), "{c}");
    let (c, _) = calls(
        "cva",
        &given(
            "cva",
            &[
                ("direction", json!("c2va")),
                ("threshold", json!("fixed")),
                ("value", json!(0.25)),
                ("sample", json!(0)),
            ],
        ),
        &input,
    )
    .unwrap();
    assert!(c.contains("direction='c2va', scale='none', threshold=0.25") && c.contains("sample=None"), "{c}");
    // iMAD and the normalization
    let (c, _) = calls(
        "imad",
        &given("imad", &[("before", json!(2019.9)), ("after", json!(2021.0)), ("lam", json!(0.5))]),
        &input,
    )
    .unwrap();
    assert!(c.contains("zeit.imad(bands_all, dates=(1, 2), max_iter=50, tol=0.001, lam=0.5, penalty='ridge'"), "{c}");
    let (c, _) = calls("radcal", &given("radcal", &[("target", json!(2021.0))]), &input).unwrap();
    assert!(
        c.contains("zeit.radcal(bands_all, reference=0, targets=[2], threshold=0.95, holdout=0.3333333333333333"),
        "{c}"
    );
    // the compact call leaves out Zeit's defaults
    let call = janus_zeit::python::tool_call("imad", &given("imad", &[]), &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.imad(bands_all, dates=(0, 2))");
    let call = janus_zeit::python::tool_call("radcal", &given("radcal", &[]), &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.radcal(bands_all, reference=0)");
}

#[test]
fn omnibus_has_a_python_call_on_the_mapped_bands_with_the_scale_converted() {
    let given = |v: &[(&str, Value)]| -> Params {
        Params::with_defaults("omnibus", &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect()).unwrap()
    };
    let input = PixelInput {
        years: vec![2020.0, 2020.5],
        days: Some(vec![18300, 18400]),
        values: vec![0.1, 0.2],
        band_numbers: [("vv", 2), ("vh", 1)].iter().map(|(r, b)| (r.to_string(), *b)).collect(),
        ..Default::default()
    };
    // the automatic scale: the median decides in Python as it does in Janus
    let (c, inputs) = calls("omnibus", &given(&[]), &input).unwrap();
    assert_eq!(inputs, vec!["bands_all"]);
    assert!(c.starts_with("if float(bands_all.sel(band=['b002', 'b001']).median(skipna=True)) < 0:"), "{c}");
    assert!(c.contains("    bands_all = 10.0 ** (bands_all / 10.0)"), "{c}");
    assert!(
        c.contains(
            "result1 = zeit.omnibus(bands_all, bands=['b002', 'b001'], enl=4.4, alpha=0.01, approximation='improved', per_date=False"
        ),
        "{c}"
    );
    // dB: converted unconditionally; linear: not at all; the series of directions too
    let (c, _) = calls("omnibus", &given(&[("scale", json!("db")), ("per_date", json!(true))]), &input).unwrap();
    assert!(c.starts_with("bands_all = 10.0 ** (bands_all / 10.0)  # dB to linear power"), "{c}");
    assert!(c.contains("result1, result2 = zeit.omnibus(") && c.contains("per_date=True"), "{c}");
    let (c, _) = calls(
        "omnibus",
        &given(&[("scale", json!("linear")), ("enl", json!(9.5)), ("approximation", json!("chi2"))]),
        &input,
    )
    .unwrap();
    assert!(!c.contains("10.0 **") && c.contains("enl=9.5") && c.contains("approximation='chi2'"), "{c}");
    // one polarization: VV alone; none mapped: no call
    let vv_only = PixelInput { band_numbers: [("vv".to_string(), 1)].into_iter().collect(), ..input.clone() };
    let (c, _) = calls("omnibus", &given(&[("scale", json!("linear"))]), &vv_only).unwrap();
    assert!(c.contains("bands=['b001']"), "{c}");
    assert!(calls("omnibus", &given(&[]), &PixelInput::default()).is_none());
    // the compact call leaves out Zeit's defaults
    let call = janus_zeit::python::tool_call("omnibus", &given(&[("scale", json!("linear"))]), &input).unwrap();
    assert_eq!(call.compact("").0, "result = zeit.omnibus(bands_all, bands=['b002', 'b001'])");
}

#[test]
fn the_topographic_correction_has_a_python_call_with_the_dem_and_the_sun_of_each_date() {
    use janus_zeit::sun::SunHint;
    let given = |v: &[(&str, Value)]| -> Params {
        Params::with_defaults("topocorr", &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect()).unwrap()
    };
    let hint = |z: f64, a: f64| SunHint { stac: Some([z, a]), ..Default::default() };
    let input = PixelInput {
        years: vec![2020.0, 2020.5],
        days: Some(vec![18300, 18400]),
        values: vec![0.1, 0.2],
        band_numbers: [("red", 3), ("nir", 4)].iter().map(|(r, b)| (r.to_string(), *b)).collect(),
        // six bands and a quality band the 7th
        bands_per_date: 7,
        qa_band: 7,
        suns: vec![hint(38.5, 141.0), hint(41.25, 139.5)],
        ..Default::default()
    };
    // a DEM file: reprojected onto the cube's grid; the sun as numbers; every band but the quality band
    let file = given(&[("dem_source", json!("file")), ("dem", json!("/data/dem.tif"))]);
    let (c, inputs) = calls("topocorr", &file, &input).unwrap();
    assert_eq!(inputs, vec!["bands_all"]);
    assert!(c.contains("dem = zeit.load_raster('/data/dem.tif').rio.reproject_match(bands_all.isel(time=0, band=0), resampling=Resampling.bilinear)"), "{c}");
    assert!(c.contains("# the sun of each date as Janus found it (STAC item), one number a date"), "{c}");
    assert!(
        c.contains(
            "result1 = zeit.topographic_correction(bands_all, dem, sun_zenith=[38.5, 41.25], sun_azimuth=[141.0, 139.5], method='scs+c', sample=None, min_slope=5.0, min_sample=100, bands=['b001', 'b002', 'b003', 'b004', 'b005', 'b006'], nodata=None, n_jobs=1)"
        ),
        "{c}"
    );
    // Zeit's own default is 'c': the script says why it passes the method
    assert!(c.contains("Zeit's own default method is 'c'"), "{c}");
    // the Copernicus DEM: the download, with its notice; the vegetation sample with the NDVI of the mapped bands
    let cop = given(&[
        ("sample", json!("ndvi")),
        ("ndvi_min", json!(0.4)),
        ("method", json!("c")),
        ("bands", json!("3, nir")),
    ]);
    let (c, _) = calls("topocorr", &cop, &input).unwrap();
    assert!(c.contains("produced using Copernicus WorldDEM-30") && c.contains("collections=['cop-dem-glo-30']"), "{c}");
    assert!(c.contains("ndvi = (bands_all.sel(band='b004') - bands_all.sel(band='b003')) / (bands_all.sel(band='b004') + bands_all.sel(band='b003'))"), "{c}");
    assert!(c.contains("sample = ((ndvi > 0.4) & (slope >= 5.0)).values"), "{c}");
    assert!(c.contains("sample=sample") && c.contains("bands=['b003', 'nir']") && c.contains("method='c'"), "{c}");
    // the angle bands as the sun, in hundredths of a degree
    let mut with_angles = input.clone();
    with_angles.band_numbers.extend([("sza".to_string(), 5), ("saa".to_string(), 6)]);
    let (c, _) =
        calls("topocorr", &given(&[("dem_source", json!("file")), ("angle_scale", json!("hundredths"))]), &with_angles)
            .unwrap();
    assert!(
        c.contains("sun_zenith=bands_all.sel(band='b005') * 0.01, sun_azimuth=bands_all.sel(band='b006') * 0.01"),
        "{c}"
    );
    assert!(c.contains("bands=['b001', 'b002', 'b003', 'b004']"), "the angle bands are not corrected: {c}");
    // a date with no sun: no call; the compact call leaves out Zeit's defaults
    let mut none = input.clone();
    none.suns = vec![hint(38.5, 141.0), SunHint::default()];
    assert!(calls("topocorr", &file, &none).is_none());
    let compact = janus_zeit::python::tool_call("topocorr", &given(&[("dem_source", json!("file"))]), &input)
        .unwrap()
        .compact("")
        .0;
    assert!(compact.contains("method='scs+c'") && compact.contains("sun_zenith=[38.5, 41.25]"), "{compact}");
    assert!(
        !compact.contains("min_slope") && !compact.contains("min_sample") && !compact.contains("n_jobs"),
        "{compact}"
    );
}

#[test]
fn the_co_registration_has_a_python_call_on_the_bands_with_the_quality_band_left_out() {
    let given = |v: &[(&str, Value)]| -> Params {
        Params::with_defaults("coregister", &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect()).unwrap()
    };
    let input = PixelInput {
        years: vec![2020.0, 2020.5, 2021.0, 2021.5],
        days: Some(vec![18262, 18444, 18628, 18809]),
        values: vec![0.1, 0.2, 0.3, 0.4],
        band_numbers: [("red", 3), ("nir", 4)].iter().map(|(r, b)| (r.to_string(), *b)).collect(),
        // six bands and a quality band the 7th
        bands_per_date: 7,
        qa_band: 7,
        sensors: vec!["LND08".into(), "SEN2".into(), "SEN2".into(), "SEN2".into()],
        ..Default::default()
    };
    // global, date 2: the NIR, the quality band not in the cube, the margin grown, Zeit's defaults left out of the compact call
    let g = given(&[("ref_date", json!(2)), ("mask_buffer", json!(0))]);
    let (c, inputs) = calls("coregister", &g, &input).unwrap();
    assert_eq!(inputs, vec!["bands_all"]);
    assert!(c.contains("cube = bands_all.sel(band=['b001', 'b002', 'b003', 'b004', 'b005', 'b006'])"), "{c}");
    assert!(c.contains("ref = cube.sel(band='b004').isel(time=1)  # the reference: date 2"), "{c}");
    assert!(
        c.contains("result1, shifts = zeit.coregister(cube, ref, window_size=256, max_shift=5, max_iter=5, band='b004', resampling='cubic', min_reliability=None, nodata=None, n_jobs=1)"),
        "{c}"
    );
    assert!(!c.contains("binary_dilation"), "no margin, no dilation: {c}");
    let compact = janus_zeit::python::tool_call("coregister", &g, &input).unwrap().compact("").0;
    assert!(compact.contains("zeit.coregister(cube, ref, band='b004')"), "{compact}");
    assert!(!compact.contains("window_size") && !compact.contains("n_jobs"), "{compact}");
    // the date with the most clear pixels, with the margin Janus puts around the clouds
    let (c, _) = calls("coregister", &given(&[]), &input).unwrap();
    assert!(c.contains("clear = bands_all.sel(band='b004').notnull().sum(['y', 'x'])"), "{c}");
    assert!(c.contains("ref = cube.sel(band='b004').isel(time=int(clear.argmax('time')))"), "{c}");
    assert!(c.contains("binary_dilation") && c.contains("cube = grow(cube, 2)"), "{c}");
    // the composite of the dates of the most frequent sensor (three of Sentinel-2), a pixel needing three
    let (c, _) = calls("coregister", &given(&[("reference", json!("composite"))]), &input).unwrap();
    assert!(c.contains("stack = bands_all.sel(band='b004').isel(time=[1, 2, 3])"), "{c}");
    assert!(c.contains("ref = stack.median('time').where(stack.notnull().sum('time') >= 3)"), "{c}");
    // local: a polynomial's order, AROSICS' 60 for the least reliability, the tie points' table
    let l = given(&[
        ("mode", json!("local")),
        ("ref_date", json!(1)),
        ("warp", json!("poly2")),
        ("grid_res", json!(100)),
        ("window_size", json!(128)),
        ("band", json!("red")),
    ]);
    let (c, _) = calls("coregister", &l, &input).unwrap();
    assert!(
        c.contains("result1, points = zeit.coregister_local(cube, ref, grid_res=100, window_size=128, max_shift=5, max_iter=5, min_reliability=60, tie_point_filter_level=3, rs_max_outlier=10, rs_tolerance=2.5, rs_random_state=0, warp='polynomial', order=2, min_points=5, band='b003', resampling='cubic', arosics_compat=False, nodata=None, n_jobs=1)"),
        "{c}"
    );
    assert!(c.contains("points.to_csv('tie_points.csv')"), "{c}");
    // the reference layer (another series) has no call; neither has a band that is not mapped
    assert!(calls("coregister", &given(&[("reference", json!("layer"))]), &input).is_none());
    assert!(calls("coregister", &given(&[("band", json!("swir1"))]), &input).is_none());
}
