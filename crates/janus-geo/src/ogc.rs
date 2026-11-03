//! OGC web services (IDEIAS: camadas OGC): maps and features from servers, straight on the map. A server's
//! capabilities (WMS, WMTS, WFS, OGC API - Features) read through GDAL's libcurl (janus_gdal::http_fetch) and
//! parsed with GDAL's own XML reader (CPLParseXMLString) or serde_json: its layers, with their titles, abstracts,
//! extents, time dimension and styles.
//!
//! A map layer (WMS, WMTS) becomes a BasemapSource drawn by the basemap's machinery (basemap.rs: tiles in Web
//! Mercator, GDAL's WMS driver, its disk cache, a pool of its own): a WMS through GDAL_WMS's WMS service (each
//! tile a GetMap of its box), a WMTS through its tile template when it has a Web Mercator tile matrix set.
//!
//! Features (WFS, OGC API - Features) are fetched by GDAL's WFS and OAPIF drivers (paging, GML or GeoJSON, axis
//! order) for a box in longitude and latitude, an optional date range and attribute filter, up to a cap, into a
//! GeoPackage (fetch_features): a vector file like any other, cached on disk (fetch_cached).

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};

use crate::basemap::{BasemapSource, curl_ca_bundle};
use crate::reproject::Quiet;

/// Features fetched at most by default.
pub const DEFAULT_MAX_FEATURES: usize = 50_000;
/// Features asked for per request (WFS paging, OGC API's limit).
const PAGE_SIZE: usize = 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Service {
    #[default]
    Wms,
    Wmts,
    Wfs,
    Oapif,
}

impl Service {
    pub const ALL: [Service; 4] = [Service::Wms, Service::Wmts, Service::Wfs, Service::Oapif];

    pub fn name(self) -> &'static str {
        match self {
            Service::Wms => "WMS",
            Service::Wmts => "WMTS",
            Service::Wfs => "WFS",
            Service::Oapif => "OGC API - Features",
        }
    }

    /// Kept in projects.
    pub fn id(self) -> &'static str {
        match self {
            Service::Wms => "wms",
            Service::Wmts => "wmts",
            Service::Wfs => "wfs",
            Service::Oapif => "oapif",
        }
    }

    pub fn from_id(id: &str) -> Option<Service> {
        Service::ALL.into_iter().find(|s| s.id().eq_ignore_ascii_case(id))
    }

    /// A map (images) rather than features.
    pub fn is_map(self) -> bool {
        matches!(self, Service::Wms | Service::Wmts)
    }
}

/// A server offered in the dialog.
#[derive(Clone, Debug)]
pub struct Preset {
    pub name: &'static str,
    pub url: &'static str,
    pub service: Option<Service>,
    /// the credit drawn with its layers
    pub attribution: &'static str,
    pub terms: &'static str,
}

/// INPE's TerraBrasilis (PRODES, DETER), IBGE's geoservices, an OGC API - Features demo.
pub fn presets() -> Vec<Preset> {
    let inpe = "INPE / TerraBrasilis (PRODES, DETER)";
    let inpe_terms = "INPE's open data: cite INPE (PRODES, DETER) when you use it";
    vec![
        Preset {
            name: "TerraBrasilis: DETER Amazon alerts (WFS)",
            url: "https://terrabrasilis.dpi.inpe.br/geoserver/deter-amz/ows",
            service: Some(Service::Wfs),
            attribution: inpe,
            terms: inpe_terms,
        },
        Preset {
            name: "TerraBrasilis: DETER Amazon alerts (WMS)",
            url: "https://terrabrasilis.dpi.inpe.br/geoserver/deter-amz/ows",
            service: Some(Service::Wms),
            attribution: inpe,
            terms: inpe_terms,
        },
        Preset {
            name: "TerraBrasilis: PRODES Legal Amazon (WFS)",
            url: "https://terrabrasilis.dpi.inpe.br/geoserver/prodes-legal-amz/ows",
            service: Some(Service::Wfs),
            attribution: inpe,
            terms: inpe_terms,
        },
        Preset {
            name: "TerraBrasilis: every map (WMS)",
            url: "https://terrabrasilis.dpi.inpe.br/geoserver/ows",
            service: Some(Service::Wms),
            attribution: inpe,
            terms: inpe_terms,
        },
        Preset {
            name: "IBGE: municipalities, states... (WFS)",
            url: "https://geoservicos.ibge.gov.br/geoserver/ows",
            service: Some(Service::Wfs),
            attribution: "IBGE",
            terms: "IBGE's open data: cite IBGE as the source",
        },
        Preset {
            name: "IBGE: maps (WMS)",
            url: "https://geoservicos.ibge.gov.br/geoserver/ows",
            service: Some(Service::Wms),
            attribution: "IBGE",
            terms: "IBGE's open data: cite IBGE as the source",
        },
        Preset {
            name: "pygeoapi demo (OGC API - Features)",
            url: "https://demo.pygeoapi.io/master",
            service: Some(Service::Oapif),
            attribution: "pygeoapi demo server",
            terms: "A demonstration server: light use",
        },
    ]
}

/// The credit of a URL's preset server, if it is one.
pub fn preset_attribution(url: &str) -> Option<&'static str> {
    let host = host_of(url);
    presets().into_iter().find(|p| host_of(p.url) == host).map(|p| p.attribution)
}

/// The host of a URL ("" for none).
pub fn host_of(url: &str) -> &str {
    let rest = url.find("://").map_or(url, |a| &url[a + 3..]);
    &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())]
}

/// A local server (the tests'): its tiles are not cached on disk.
fn is_local(url: &str) -> bool {
    let h = host_of(url);
    let h = h.rsplit_once(':').map_or(h, |(a, p)| if p.chars().all(|c| c.is_ascii_digit()) { a } else { h });
    h == "127.0.0.1" || h == "localhost" || h == "[::1]"
}

/// The service a URL names (its SERVICE parameter or its path), None if it does not say.
pub fn guess(url: &str) -> Option<Service> {
    let l = url.to_ascii_lowercase();
    for (k, s) in [("service=wmts", Service::Wmts), ("service=wms", Service::Wms), ("service=wfs", Service::Wfs)] {
        if l.contains(k) {
            return Some(s);
        }
    }
    let path = &l[..l.find('?').unwrap_or(l.len())];
    if path.contains("/collections") || path.contains("/ogcapi") || path.ends_with("/features") {
        return Some(Service::Oapif);
    }
    if path.contains("wmts") {
        return Some(Service::Wmts);
    }
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    match last {
        "wms" => Some(Service::Wms),
        "wfs" => Some(Service::Wfs),
        _ => None,
    }
}

/// The URL without the parameters of a request (SERVICE, REQUEST, VERSION...): the server's base.
pub fn base_url(url: &str) -> String {
    let url = url.trim();
    let Some(q) = url.find('?') else { return url.to_string() };
    const DROP: [&str; 9] =
        ["service", "request", "version", "acceptversions", "typename", "typenames", "layers", "outputformat", "f"];
    let kept: Vec<&str> = url[q + 1..]
        .split('&')
        .filter(|kv| {
            let k = kv.split('=').next().unwrap_or("").to_ascii_lowercase();
            !kv.is_empty() && !DROP.contains(&k.as_str())
        })
        .collect();
    if kept.is_empty() { url[..q].to_string() } else { format!("{}?{}", &url[..q], kept.join("&")) }
}

/// `url` with these query parameters added.
pub fn with_query(url: &str, params: &[(&str, &str)]) -> String {
    let mut u = url.to_string();
    for (k, v) in params {
        if !u.contains('?') {
            u.push('?');
        } else if !u.ends_with('?') && !u.ends_with('&') {
            u.push('&');
        }
        u += &format!("{k}={}", encode(v));
    }
    u
}

/// A query value percent-encoded (what a URL may not hold as is).
pub fn encode(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~:/,".contains(&b) {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

/// The landing page of an OGC API: the URL up to "/collections", without a query or a final "/".
pub fn oapif_base(url: &str) -> String {
    let u = url.trim();
    let u = &u[..u.find('?').unwrap_or(u.len())];
    let u = match u.find("/collections") {
        Some(p) => &u[..p],
        None => u,
    };
    u.trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------------------------------------------
// HTTP

fn user_agent() -> String {
    format!("Janus/{} (+https://github.com/sacridini/janus)", env!("CARGO_PKG_VERSION"))
}

/// The thread's HTTP settings for GDAL's drivers: timeouts, retries, the User-Agent, certificates.
fn http_config() {
    let set = |k: &str, v: &str| {
        let _ = gdal::config::set_thread_local_config_option(k, v);
    };
    set("GDAL_HTTP_TIMEOUT", "120");
    set("GDAL_HTTP_CONNECTTIMEOUT", "15");
    set("GDAL_HTTP_MAX_RETRY", "2");
    set("GDAL_HTTP_RETRY_DELAY", "2");
    set("GDAL_HTTP_USERAGENT", &user_agent());
    let ca = curl_ca_bundle();
    if !ca.is_empty() {
        set("CURL_CA_BUNDLE", &ca);
    }
}

/// A network failure (the server not reached) rather than a server's answer.
pub fn is_offline_error(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.starts_with("no connection")
        || l.contains("could not resolve")
        || l.contains("couldn't resolve")
        || l.contains("failed to connect")
        || l.contains("couldn't connect")
        || l.contains("timed out")
        || l.contains("connection refused")
}

/// GET `url` (JSON asked for when `json`): the body, or a plain reason.
pub fn get(url: &str, json: bool) -> Result<Vec<u8>, String> {
    http_config();
    let ua = format!("USERAGENT={}", user_agent());
    let mut opts = vec!["TIMEOUT=60", "CONNECTTIMEOUT=10", "MAX_RETRY=1", "RETRY_DELAY=1", ua.as_str()];
    if json {
        opts.push("HEADERS=Accept: application/json, application/geo+json");
    }
    let _q = Quiet::new(); // the reason is returned, not printed
    janus_gdal::http_fetch(url, None, &opts).map_err(|e| {
        let host = host_of(url);
        if is_offline_error(&e) {
            format!("no connection to {host} ({})", e.lines().next().unwrap_or(&e))
        } else {
            format!("{host}: {}", e.trim())
        }
    })
}

// ---------------------------------------------------------------------------------------------------------------
// XML (GDAL's reader, namespaces stripped)

/// An XML element: its name without its namespace, attributes, text and children.
#[derive(Clone, Debug, Default)]
pub struct Xml {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub kids: Vec<Xml>,
}

impl Xml {
    /// The document's root element.
    pub fn parse(text: &str) -> Result<Xml, String> {
        use gdal_sys::CPLXMLNodeType::*;
        let c = CString::new(text.replace('\0', "")).map_err(|e| e.to_string())?;
        let _q = Quiet::new();
        // SAFETY: GDAL parses a C string alive for the call and returns a tree we own, freed once below; the
        // conversion only reads it.
        unsafe {
            gdal_sys::CPLErrorReset();
            let root = gdal_sys::CPLParseXMLString(c.as_ptr());
            if root.is_null() {
                return Err(format!("not XML ({})", last_error()));
            }
            gdal_sys::CPLStripXMLNamespace(root, std::ptr::null(), 1);
            let mut n = root;
            let mut out = None;
            while !n.is_null() {
                if (*n).eType == CXT_Element && !cstr((*n).pszValue).starts_with('?') {
                    out = Some(convert(n));
                    break;
                }
                n = (*n).psNext;
            }
            gdal_sys::CPLDestroyXMLNode(root);
            out.ok_or_else(|| "no XML element".into())
        }
    }

    pub fn child(&self, name: &str) -> Option<&Xml> {
        self.kids.iter().find(|k| k.name == name)
    }

    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Xml> + 'a {
        self.kids.iter().filter(move |k| k.name == name)
    }

    /// The element at a path of names ("a/b/c").
    pub fn at(&self, path: &str) -> Option<&Xml> {
        path.split('/').try_fold(self, |n, p| n.child(p))
    }

    /// The trimmed text at a path ("" if none).
    pub fn text_at(&self, path: &str) -> String {
        self.at(path).map_or(String::new(), |n| n.text.trim().to_string())
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.0 == name).map(|a| a.1.as_str())
    }

    /// The first element of this name anywhere under it (depth first).
    pub fn find(&self, name: &str) -> Option<&Xml> {
        for k in &self.kids {
            if k.name == name {
                return Some(k);
            }
            if let Some(f) = k.find(name) {
                return Some(f);
            }
        }
        None
    }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// An element of GDAL's tree as an Xml.
unsafe fn convert(n: *const gdal_sys::CPLXMLNode) -> Xml {
    use gdal_sys::CPLXMLNodeType::*;
    let mut x = Xml { name: unsafe { cstr((*n).pszValue) }, ..Default::default() };
    let mut c = unsafe { (*n).psChild };
    while !c.is_null() {
        let t = unsafe { (*c).eType };
        if t == CXT_Element {
            x.kids.push(unsafe { convert(c) });
        } else if t == CXT_Text {
            x.text += &unsafe { cstr((*c).pszValue) };
        } else if t == CXT_Attribute {
            let v = unsafe { (*c).psChild };
            let value = if v.is_null() { String::new() } else { unsafe { cstr((*v).pszValue) } };
            x.attrs.push((unsafe { cstr((*c).pszValue) }, value));
        }
        c = unsafe { (*c).psNext };
    }
    x
}

fn last_error() -> String {
    // SAFETY: GDAL's string, copied at once.
    unsafe { cstr(gdal_sys::CPLGetLastErrorMsg()) }
}

/// The text of an OGC exception report (WMS ServiceExceptionReport, OWS ExceptionReport), if it is one.
fn exception_text(x: &Xml) -> Option<String> {
    if !x.name.contains("Exception") {
        return None;
    }
    let mut texts = Vec::new();
    fn walk(x: &Xml, out: &mut Vec<String>) {
        if (x.name == "ServiceException" || x.name == "ExceptionText") && !x.text.trim().is_empty() {
            out.push(x.text.trim().to_string());
        }
        for k in &x.kids {
            walk(k, out);
        }
    }
    walk(x, &mut texts);
    Some(if texts.is_empty() { "the server answered with an exception".into() } else { texts.join("; ") })
}

// ---------------------------------------------------------------------------------------------------------------
// Capabilities

/// A layer a server offers.
#[derive(Clone, Debug, Default)]
pub struct OgcLayer {
    /// what requests name it (WMS Name, WFS typename, WMTS Identifier, OGC API collection id)
    pub name: String,
    pub title: String,
    pub abstract_text: String,
    /// longitude and latitude (west, south, east, north)
    pub bbox: Option<[f64; 4]>,
    pub crs: Vec<String>,
    /// WMS (and OGC API) time dimension: its values as listed (a date, or start/end/period) and its default
    pub times: Vec<String>,
    pub time_default: String,
    /// WMS style names, WMTS style identifiers: the default first
    pub styles: Vec<String>,
    /// WMTS image formats
    pub formats: Vec<String>,
    /// WMTS: an XYZ template in Web Mercator ({z}, {x}, {y}; {Style} and {Format} to fill) and its finest zoom
    pub tiles: Option<(String, i32)>,
    /// the layer's credit, if it gives one
    pub attribution: String,
}

impl OgcLayer {
    /// Its title, or its name.
    pub fn label(&self) -> &str {
        if self.title.trim().is_empty() { &self.name } else { &self.title }
    }

    /// Its name, title or abstract holds every word of `filter` (any case).
    pub fn matches(&self, filter: &str) -> bool {
        let hay = format!("{} {} {}", self.name, self.title, self.abstract_text).to_lowercase();
        filter.to_lowercase().split_whitespace().all(|w| hay.contains(w))
    }

    /// The projection a WMS is asked in: Web Mercator by one of its codes, the one the layer lists.
    pub fn web_mercator_code(&self) -> &'static str {
        for c in ["EPSG:3857", "EPSG:900913", "EPSG:102100", "EPSG:102113"] {
            if self.crs.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                return c;
            }
        }
        "EPSG:3857"
    }
}

/// What a server offers.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    pub service: Service,
    /// the base URL requests go to
    pub url: String,
    pub version: String,
    pub title: String,
    pub provider: String,
    pub layers: Vec<OgcLayer>,
    /// WMS GetMap image formats
    pub formats: Vec<String>,
    /// WFS: paging supported (ImplementsResultPaging)
    pub paging: bool,
    /// the time the request and the parsing took
    pub ms: f64,
    pub bytes: usize,
}

/// A server's capabilities: of the service asked for, or found (the URL's, else OGC API, WFS and WMS tried in
/// turn).
pub fn capabilities(url: &str, service: Option<Service>) -> Result<Capabilities, String> {
    let url = url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("the URL must start with https:// or http://".into());
    }
    let tries: Vec<Service> = match service.or_else(|| guess(url)) {
        Some(s) => vec![s],
        None => vec![Service::Oapif, Service::Wfs, Service::Wms],
    };
    let mut errs = Vec::new();
    for s in tries {
        match capabilities_of(url, s) {
            Ok(c) => return Ok(c),
            Err(e) if is_offline_error(&e) => return Err(e),
            Err(e) => errs.push(format!("{}: {e}", s.name())),
        }
    }
    Err(if errs.len() == 1 { errs.remove(0) } else { format!("no OGC service found there ({})", errs.join("; ")) })
}

fn capabilities_of(url: &str, s: Service) -> Result<Capabilities, String> {
    let t0 = Instant::now();
    let (body, mut c) = match s {
        Service::Oapif => {
            let base = oapif_base(url);
            let body = get(&with_query(&format!("{base}/collections"), &[("f", "json")]), true)?;
            let c = parse_oapif(&body, &base)?;
            (body, c)
        }
        _ => {
            let base = base_url(url);
            let q = with_query(&base, &[("SERVICE", s.name()), ("REQUEST", "GetCapabilities")]);
            let body = get(&q, false)?;
            let text = String::from_utf8_lossy(&body);
            let x = Xml::parse(&text).map_err(|_| not_capabilities(s, &text))?;
            if let Some(e) = exception_text(&x) {
                return Err(e);
            }
            let mut c = match s {
                Service::Wms => parse_wms(&x),
                Service::Wmts => parse_wmts(&x),
                _ => parse_wfs(&x),
            }
            .ok_or_else(|| not_capabilities(s, &text))?;
            if c.url.is_empty() || s != Service::Wms {
                c.url = base;
            }
            (body, c)
        }
    };
    c.ms = t0.elapsed().as_secs_f64() * 1000.0;
    c.bytes = body.len();
    Ok(c)
}

fn not_capabilities(s: Service, text: &str) -> String {
    let start: String = text.trim().chars().take(120).collect::<String>().replace(['\n', '\r'], " ");
    format!("not {} capabilities (the server answered: {start})", s.name())
}

fn numbers(s: &str) -> Vec<f64> {
    s.split([' ', ',']).filter_map(|v| v.trim().parse().ok()).collect()
}

fn valid_bbox(b: [f64; 4]) -> Option<[f64; 4]> {
    (b.iter().all(|v| v.is_finite()) && b[2] >= b[0] && b[3] >= b[1]).then_some(b)
}

/// OWS 1.1 / 2.0 WGS84BoundingBox (LowerCorner "lon lat", UpperCorner).
fn ows_bbox(x: &Xml) -> Option<[f64; 4]> {
    let b = x.child("WGS84BoundingBox")?;
    let (lo, hi) = (numbers(&b.text_at("LowerCorner")), numbers(&b.text_at("UpperCorner")));
    (lo.len() >= 2 && hi.len() >= 2).then(|| valid_bbox([lo[0], lo[1], hi[0], hi[1]])).flatten()
}

/// WMS 1.1.1 and 1.3.0.
pub fn parse_wms(x: &Xml) -> Option<Capabilities> {
    if x.name != "WMS_Capabilities" && x.name != "WMT_MS_Capabilities" {
        return None;
    }
    let mut c = Capabilities {
        service: Service::Wms,
        version: x.attr("version").unwrap_or("1.3.0").to_string(),
        title: x.text_at("Service/Title"),
        provider: x.text_at("Service/ContactInformation/ContactPersonPrimary/ContactOrganization"),
        ..Default::default()
    };
    let cap = x.child("Capability")?;
    if let Some(gm) = cap.at("Request/GetMap") {
        c.formats = gm.all("Format").map(|f| f.text.trim().to_string()).collect();
        if let Some(h) = gm.at("DCPType/HTTP/Get/OnlineResource").and_then(|o| o.attr("href")) {
            c.url = base_url(h);
        }
    }
    #[derive(Clone, Default)]
    struct Inherit {
        crs: Vec<String>,
        bbox: Option<[f64; 4]>,
        times: Vec<String>,
        time_default: String,
        styles: Vec<String>,
        attribution: String,
    }
    fn walk(l: &Xml, inh: &Inherit, out: &mut Vec<OgcLayer>) {
        let mut h = inh.clone();
        for k in l.all("CRS").chain(l.all("SRS")) {
            for code in k.text.split_whitespace() {
                if !h.crs.iter().any(|c| c == code) {
                    h.crs.push(code.to_string());
                }
            }
        }
        if let Some(b) = l.child("EX_GeographicBoundingBox") {
            let v = |n: &str| b.text_at(n).parse::<f64>().unwrap_or(f64::NAN);
            h.bbox = valid_bbox([
                v("westBoundLongitude"),
                v("southBoundLatitude"),
                v("eastBoundLongitude"),
                v("northBoundLatitude"),
            ])
            .or(h.bbox);
        } else if let Some(b) = l.child("LatLonBoundingBox") {
            let v = |n: &str| b.attr(n).and_then(|s| s.parse::<f64>().ok()).unwrap_or(f64::NAN);
            h.bbox = valid_bbox([v("minx"), v("miny"), v("maxx"), v("maxy")]).or(h.bbox);
        }
        // the time dimension: its values in Dimension (1.3.0) or in Extent (1.1.1)
        let is_time = |n: &&Xml| n.attr("name").is_some_and(|v| v.eq_ignore_ascii_case("time"));
        let dim = l.all("Dimension").find(is_time);
        let ext = l.all("Extent").find(is_time);
        let values = ext.filter(|e| !e.text.trim().is_empty()).or(dim.filter(|d| !d.text.trim().is_empty()));
        if let Some(v) = values {
            h.times = v.text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            h.time_default =
                v.attr("default").or_else(|| dim.and_then(|d| d.attr("default"))).unwrap_or("").to_string();
        }
        let styles: Vec<String> = l.all("Style").map(|s| s.text_at("Name")).filter(|s| !s.is_empty()).collect();
        if !styles.is_empty() {
            h.styles = styles;
        }
        let a = l.text_at("Attribution/Title");
        if !a.is_empty() {
            h.attribution = a;
        }
        let name = l.text_at("Name");
        if !name.is_empty() {
            out.push(OgcLayer {
                name,
                title: l.text_at("Title"),
                abstract_text: l.text_at("Abstract"),
                bbox: h.bbox,
                crs: h.crs.clone(),
                times: h.times.clone(),
                time_default: h.time_default.clone(),
                styles: h.styles.clone(),
                attribution: h.attribution.clone(),
                ..Default::default()
            });
        }
        for k in l.all("Layer") {
            walk(k, &h, out);
        }
    }
    for l in cap.all("Layer") {
        walk(l, &Inherit::default(), &mut c.layers);
    }
    Some(c)
}

/// WFS 1.0, 1.1 and 2.0.
pub fn parse_wfs(x: &Xml) -> Option<Capabilities> {
    if x.name != "WFS_Capabilities" {
        return None;
    }
    let mut c = Capabilities {
        service: Service::Wfs,
        version: x.attr("version").unwrap_or("2.0.0").to_string(),
        title: x.text_at("ServiceIdentification/Title"),
        provider: x.text_at("ServiceProvider/ProviderName"),
        ..Default::default()
    };
    if c.title.is_empty() {
        c.title = x.text_at("Service/Title");
    }
    c.paging = x
        .child("OperationsMetadata")
        .map(|o| {
            o.all("Constraint").any(|k| {
                k.attr("name") == Some("ImplementsResultPaging")
                    && k.text_at("DefaultValue").eq_ignore_ascii_case("true")
            })
        })
        .unwrap_or(false);
    for f in x.child("FeatureTypeList").into_iter().flat_map(|l| l.all("FeatureType")) {
        let mut crs: Vec<String> = Vec::new();
        for n in ["DefaultCRS", "DefaultSRS", "SRS", "OtherCRS", "OtherSRS"] {
            crs.extend(f.all(n).map(|k| k.text.trim().to_string()));
        }
        let bbox = ows_bbox(f).or_else(|| {
            let b = f.child("LatLongBoundingBox")?;
            let v = |n: &str| b.attr(n).and_then(|s| s.parse::<f64>().ok()).unwrap_or(f64::NAN);
            valid_bbox([v("minx"), v("miny"), v("maxx"), v("maxy")])
        });
        c.layers.push(OgcLayer {
            name: f.text_at("Name"),
            title: f.text_at("Title"),
            abstract_text: f.text_at("Abstract"),
            bbox,
            crs,
            ..Default::default()
        });
    }
    c.layers.retain(|l| !l.name.is_empty());
    Some(c)
}

/// Web Mercator's scale denominator at zoom 0 for 256-pixel tiles (OGC's 0.28 mm pixel).
const MERCATOR_SD0: f64 = 559_082_264.028_717_8;

/// A WMTS tile matrix set usable as XYZ tiles: Web Mercator, 256-pixel tiles, levels 0..=n named prefix + level.
/// Its prefix and finest level.
fn xyz_matrix_set(t: &Xml) -> Option<(String, i32)> {
    let crs = t.text_at("SupportedCRS");
    if !["3857", "900913", "102100", "102113"].iter().any(|c| crs.contains(c)) {
        return None;
    }
    let mut levels = Vec::new();
    for m in t.all("TileMatrix") {
        let id = m.text_at("Identifier");
        let sd: f64 = m.text_at("ScaleDenominator").parse().ok()?;
        let tl = numbers(&m.text_at("TopLeftCorner"));
        if m.text_at("TileWidth") != "256" || tl.len() < 2 || (tl[0] + 20037508.34).abs() > 10.0 {
            return None;
        }
        let z = (MERCATOR_SD0 / sd).log2().round() as i32;
        if ((MERCATOR_SD0 / f64::from(1u32 << z.clamp(0, 30))) - sd).abs() / sd > 0.01 {
            return None;
        }
        levels.push((z, id));
    }
    levels.sort_by_key(|l| l.0);
    if levels.is_empty() || levels[0].0 != 0 || levels.iter().enumerate().any(|(k, l)| l.0 != k as i32) {
        return None;
    }
    // every identifier a common prefix and the level
    let prefix = levels[0].1.strip_suffix('0')?.to_string();
    levels.iter().all(|(z, id)| *id == format!("{prefix}{z}")).then(|| (prefix, levels.last().unwrap().0))
}

/// WMTS 1.0.0: the layers with a Web Mercator tile matrix set get a tile template.
pub fn parse_wmts(x: &Xml) -> Option<Capabilities> {
    let contents = x.child("Contents")?;
    let mut c = Capabilities {
        service: Service::Wmts,
        version: x.attr("version").unwrap_or("1.0.0").to_string(),
        title: x.text_at("ServiceIdentification/Title"),
        provider: x.text_at("ServiceProvider/ProviderName"),
        ..Default::default()
    };
    let sets: Vec<(String, Option<(String, i32)>)> =
        contents.all("TileMatrixSet").map(|t| (t.text_at("Identifier"), xyz_matrix_set(t))).collect();
    let get_tile = x
        .child("OperationsMetadata")
        .into_iter()
        .flat_map(|o| o.all("Operation"))
        .find(|o| o.attr("name") == Some("GetTile"))
        .and_then(|o| o.at("DCP/HTTP/Get").and_then(|g| g.attr("href")))
        .map(base_url);
    for l in contents.all("Layer") {
        let name = l.text_at("Identifier");
        let mut styles: Vec<(bool, String)> = l
            .all("Style")
            .map(|s| (s.attr("isDefault").is_some_and(|d| d == "true"), s.text_at("Identifier")))
            .collect();
        styles.sort_by_key(|s| !s.0);
        let formats: Vec<String> = l.all("Format").map(|f| f.text.trim().to_string()).collect();
        let dims: Vec<(String, String)> =
            l.all("Dimension").map(|d| (d.text_at("Identifier"), d.text_at("Default"))).collect();
        let linked: Vec<String> = l.all("TileMatrixSetLink").map(|k| k.text_at("TileMatrixSet")).collect();
        let tiles = linked.iter().find_map(|id| {
            let (prefix, zmax) = sets.iter().find(|s| &s.0 == id)?.1.clone()?;
            let rest = l
                .all("ResourceURL")
                .find(|r| r.attr("resourceType") == Some("tile"))
                .and_then(|r| r.attr("template"))
                .map(|t| {
                    let mut t = t
                        .replace("{TileMatrixSet}", id)
                        .replace("{TileMatrix}", &format!("{prefix}{{z}}"))
                        .replace("{TileRow}", "{y}")
                        .replace("{TileCol}", "{x}");
                    for (d, v) in &dims {
                        t = t.replace(&format!("{{{d}}}"), v);
                    }
                    t
                })
                .or_else(|| {
                    let mut t = with_query(
                        get_tile.as_deref()?,
                        &[("SERVICE", "WMTS"), ("REQUEST", "GetTile"), ("VERSION", "1.0.0"), ("LAYER", &name)],
                    );
                    t += &format!(
                        "&STYLE={{Style}}&TILEMATRIXSET={}&TILEMATRIX={}{{z}}&TILEROW={{y}}&TILECOL={{x}}&FORMAT={{Format}}",
                        encode(id),
                        encode(&prefix)
                    );
                    Some(t)
                })?;
            Some((rest, zmax))
        });
        c.layers.push(OgcLayer {
            name,
            title: l.text_at("Title"),
            abstract_text: l.text_at("Abstract"),
            bbox: ows_bbox(l),
            styles: styles.into_iter().map(|s| s.1).collect(),
            formats,
            tiles,
            ..Default::default()
        });
    }
    Some(c)
}

/// OGC API - Features: the collections of /collections.
pub fn parse_oapif(body: &[u8], base: &str) -> Result<Capabilities, String> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| not_capabilities(Service::Oapif, &String::from_utf8_lossy(&body[..body.len().min(400)])))?;
    let cols = v.get("collections").and_then(|c| c.as_array()).ok_or("no collections listed")?;
    let mut c = Capabilities {
        service: Service::Oapif,
        url: base.to_string(),
        version: "1.0".into(),
        title: v.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        ..Default::default()
    };
    let s = |x: &serde_json::Value, k: &str| x.get(k).and_then(|t| t.as_str()).unwrap_or("").to_string();
    for col in cols {
        let bbox = col.pointer("/extent/spatial/bbox/0").and_then(|b| b.as_array()).and_then(|b| {
            let n: Vec<f64> = b.iter().filter_map(|v| v.as_f64()).collect();
            match n.len() {
                4 => valid_bbox([n[0], n[1], n[2], n[3]]),
                6 => valid_bbox([n[0], n[1], n[3], n[4]]),
                _ => None,
            }
        });
        let interval = col
            .pointer("/extent/temporal/interval/0")
            .and_then(|i| i.as_array())
            .map(|i| i.iter().map(|v| v.as_str().unwrap_or("..").to_string()).collect::<Vec<_>>().join("/"));
        c.layers.push(OgcLayer {
            name: s(col, "id"),
            title: s(col, "title"),
            abstract_text: s(col, "description"),
            bbox,
            crs: col
                .get("crs")
                .and_then(|c| c.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            time_default: interval.unwrap_or_default(),
            attribution: s(col, "attribution"),
            ..Default::default()
        });
    }
    c.layers.retain(|l| !l.name.is_empty());
    Ok(c)
}

// ---------------------------------------------------------------------------------------------------------------
// Maps (WMS, WMTS) as tiles

/// A map layer as asked for: the server, the layer and how it is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapLayerSpec {
    pub service: Service,
    /// WMS: GetMap's base URL; WMTS: the server's
    pub url: String,
    pub version: String,
    pub layer: String,
    pub style: String,
    pub format: String,
    pub transparent: bool,
    /// WMS TIME ("" the server's default)
    pub time: String,
    /// the projection asked for (a Web Mercator code)
    pub srs: String,
    /// WMTS: the tile template ({Style}, {Format} filled here) and its finest zoom
    pub tiles: String,
    pub max_zoom: i32,
    pub title: String,
    pub attribution: String,
}

/// The tiles of a map layer for the basemap's machinery.
pub fn map_source(m: &MapLayerSpec) -> BasemapSource {
    let local = is_local(&m.url);
    let attribution = if m.attribution.is_empty() {
        preset_attribution(&m.url).map_or_else(|| format!("{}: {}", m.service.name(), host_of(&m.url)), String::from)
    } else {
        m.attribution.clone()
    };
    let mut s = BasemapSource {
        id: m.service.id().into(),
        name: m.title.clone(),
        max_zoom: if m.max_zoom > 0 { m.max_zoom } else { 19 },
        tile_size: 256,
        attribution,
        terms: "The server's terms of use apply (its credit is shown)".into(),
        connections: 3,
        cache_days: if local { 0 } else { 1 },
        ..Default::default()
    };
    let x = |v: &str| v.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    match m.service {
        Service::Wmts => {
            s.url = m.tiles.replace("{Style}", &encode(&m.style)).replace("{Format}", &encode(&m.format));
        }
        _ => {
            let mut server = m.url.clone();
            if !m.time.is_empty() {
                server = with_query(&server, &[("TIME", &m.time)]);
            }
            let srs = if m.srs.is_empty() { "EPSG:3857" } else { &m.srs };
            s.url = server.clone();
            s.service = format!(
                "<Service name=\"WMS\"><Version>{}</Version><ServerUrl>{}</ServerUrl><SRS>{srs}</SRS><CRS>{srs}</CRS>\
                 <ImageFormat>{}</ImageFormat><Transparent>{}</Transparent><Layers>{}</Layers>\
                 <Styles>{}</Styles></Service>",
                x(if m.version.is_empty() { "1.1.1" } else { &m.version }),
                x(&server),
                x(if m.format.is_empty() { "image/png" } else { &m.format }),
                if m.transparent { "TRUE" } else { "FALSE" },
                x(&m.layer),
                x(&m.style)
            );
        }
    }
    s
}

/// The value of a time dimension to ask for at `date` (YYYY-MM-DD): the last listed value not after it (a listed
/// range start/end[/period] holds it: the date itself), None if every value is after it.
pub fn time_at(values: &[String], date: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    for v in values {
        let parts: Vec<&str> = v.split('/').collect();
        if parts.len() >= 2 {
            let (a, b) = (parts[0], parts[1]);
            if a.get(..10).is_some_and(|a| a <= date) && b.get(..10).is_none_or(|b| date <= b) {
                return Some(date.to_string());
            }
            continue;
        }
        if v.get(..10).is_some_and(|d| d <= date) && best.is_none_or(|b| b < v.as_str()) {
            best = Some(v);
        }
    }
    best.map(String::from)
}

// ---------------------------------------------------------------------------------------------------------------
// Features (WFS, OGC API - Features) into a GeoPackage

/// Features asked for: the server and layer, where, when, which, how many at most.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeatureRequest {
    pub service: Service,
    pub url: String,
    pub layer: String,
    /// longitude and latitude (west, south, east, north); None: the whole layer
    pub bbox: Option<[f64; 4]>,
    /// a date range on this field (inclusive, YYYY-MM-DD; either end may be empty)
    pub date_field: String,
    pub from: String,
    pub to: String,
    /// an attribute filter in OGR SQL (sent to the server as an OGC filter or CQL when it can be)
    pub filter: String,
    /// 0: DEFAULT_MAX_FEATURES
    pub max_features: usize,
}

/// FNV-1a: a name that stays the same from run to run.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3))
}

/// A name for a file: letters, digits, '-' and '_'.
fn slug(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let t = t.trim_matches('_');
    t.chars().take(40).collect::<String>().to_lowercase()
}

impl FeatureRequest {
    pub fn cap(&self) -> usize {
        if self.max_features == 0 { DEFAULT_MAX_FEATURES } else { self.max_features }
    }

    /// The OGR SQL filter: the date range and the attribute filter.
    pub fn where_clause(&self) -> String {
        let mut parts = Vec::new();
        let f = self.date_field.trim().replace('"', "");
        if !f.is_empty() {
            if !self.from.trim().is_empty() {
                parts.push(format!("\"{f}\" >= '{}'", self.from.trim().replace('\'', "")));
            }
            if !self.to.trim().is_empty() {
                // the whole last day
                let to = self.to.trim().replace('\'', "");
                parts.push(if to.len() == 10 {
                    format!("\"{f}\" < '{}'", next_day(&to))
                } else {
                    format!("\"{f}\" <= '{to}'")
                });
            }
        }
        if !self.filter.trim().is_empty() {
            parts.push(format!("({})", self.filter.trim()));
        }
        parts.join(" AND ")
    }

    /// What tells two requests apart (the cache's key).
    pub fn key(&self) -> String {
        let b = self.bbox.map(|b| b.map(|v| format!("{v:.6}")).join(",")).unwrap_or_default();
        let text = format!(
            "{}|{}|{}|{b}|{}|{}",
            self.service.id(),
            self.url.trim(),
            self.layer,
            self.where_clause(),
            self.cap()
        );
        format!("{:016x}", fnv(&text))
    }

    /// Its GeoPackage in a cache folder.
    pub fn cache_path(&self, dir: &Path) -> PathBuf {
        let name = self.layer.rsplit(':').next().unwrap_or(&self.layer);
        dir.join(format!("{}_{}.gpkg", slug(name), self.key()))
    }

    /// The layer's name in the GeoPackage.
    pub fn out_layer(&self) -> String {
        let s = slug(self.layer.rsplit(':').next().unwrap_or(&self.layer));
        if s.is_empty() { "features".into() } else { s }
    }

    /// As text (kept in the GeoPackage, the log and the provenance).
    pub fn describe(&self) -> String {
        let mut s = format!("{} {} layer {}", self.service.name(), self.url, self.layer);
        if let Some(b) = self.bbox {
            s += &format!(", box {:.5},{:.5},{:.5},{:.5}", b[0], b[1], b[2], b[3]);
        }
        let w = self.where_clause();
        if !w.is_empty() {
            s += &format!(", where {w}");
        }
        s + &format!(", at most {} features", self.cap())
    }
}

/// The day after YYYY-MM-DD.
fn next_day(d: &str) -> String {
    let p: Vec<i32> = d.split('-').filter_map(|v| v.parse().ok()).collect();
    let [y, m, day] = p[..] else { return d.to_string() };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let (mut y, mut m, mut day) = (y, m, day + 1);
    if day > days[(m.clamp(1, 12) - 1) as usize] {
        day = 1;
        m += 1;
        if m > 12 {
            m = 1;
            y += 1;
        }
    }
    format!("{y:04}-{m:02}-{day:02}")
}

/// Features fetched.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub path: PathBuf,
    /// the layer's name in the GeoPackage
    pub layer: String,
    pub features: usize,
    /// more were there than the cap
    pub capped: bool,
    /// as the server counted them (None: not said)
    pub matched: Option<u64>,
    pub seconds: f64,
    /// read from the cache (nothing fetched)
    pub cached: bool,
}

/// A GDAL dataset handle closed when dropped.
struct Ds(gdal_sys::GDALDatasetH);

impl Drop for Ds {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a dataset this module opened, closed once.
            unsafe { gdal_sys::GDALClose(self.0) };
        }
    }
}

fn c(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// The server's layer opened through GDAL's WFS or OAPIF driver.
fn open_layer(req: &FeatureRequest) -> Result<(Ds, gdal_sys::OGRLayerH), String> {
    http_config();
    let _ = gdal::config::set_thread_local_config_option("OGR_WFS_LOAD_MULTIPLE_LAYER_DEFN", "NO");
    let (conn, driver, opts) = match req.service {
        Service::Wfs => (
            format!("WFS:{}", with_query(&base_url(&req.url), &[("TYPENAME", &req.layer)])),
            "WFS",
            vec!["PAGING_ALLOWED=ON".to_string(), format!("PAGE_SIZE={PAGE_SIZE}")],
        ),
        Service::Oapif => (format!("OAPIF:{}", oapif_base(&req.url)), "OAPIF", vec![format!("PAGE_SIZE={PAGE_SIZE}")]),
        s => return Err(format!("{} serves maps, not features", s.name())),
    };
    let (c_conn, c_drv) = (c(&conn), c(driver));
    let c_opts: Vec<CString> = opts.iter().map(|o| c(o)).collect();
    let mut p_opts: Vec<*const c_char> = c_opts.iter().map(|o| o.as_ptr()).collect();
    p_opts.push(std::ptr::null());
    let drivers = [c_drv.as_ptr(), std::ptr::null()];
    let _q = Quiet::new();
    // SAFETY: C strings and null-terminated lists alive for the call; the handle is closed by Ds.
    unsafe {
        gdal_sys::CPLErrorReset();
        let h = gdal_sys::GDALOpenEx(
            c_conn.as_ptr(),
            0x04, // GDAL_OF_VECTOR, read only
            drivers.as_ptr(),
            p_opts.as_ptr(),
            std::ptr::null(),
        );
        if h.is_null() {
            let e = last_error();
            let host = host_of(&req.url);
            return Err(if is_offline_error(&e) {
                format!("no connection to {host} ({e})")
            } else if e.is_empty() {
                format!("{host}: the {} could not be opened", req.service.name())
            } else {
                format!("{host}: {e}")
            });
        }
        let ds = Ds(h);
        let name = c(&req.layer);
        let mut l = gdal_sys::GDALDatasetGetLayerByName(h, name.as_ptr());
        if l.is_null() {
            // a typename without its prefix, or with another one
            let want = req.layer.rsplit(':').next().unwrap_or(&req.layer);
            for i in 0..gdal_sys::GDALDatasetGetLayerCount(h) {
                let li = gdal_sys::GDALDatasetGetLayer(h, i);
                let n = cstr(gdal_sys::OGR_L_GetName(li));
                if n.rsplit(':').next() == Some(want) {
                    l = li;
                    break;
                }
            }
        }
        if l.is_null() {
            return Err(format!("the layer {} is not on the server", req.layer));
        }
        Ok((ds, l))
    }
}

/// A layer's fields (name, type) as the server describes them (WFS DescribeFeatureType, OGC API's schema or
/// first page).
pub fn layer_fields(service: Service, url: &str, layer: &str) -> Result<Vec<(String, String)>, String> {
    let req = FeatureRequest { service, url: url.into(), layer: layer.into(), ..Default::default() };
    let (_ds, l) = open_layer(&req)?;
    let mut out = Vec::new();
    // SAFETY: a layer of the open dataset; its definition read only.
    unsafe {
        let d = gdal_sys::OGR_L_GetLayerDefn(l);
        for i in 0..gdal_sys::OGR_FD_GetFieldCount(d) {
            let f = gdal_sys::OGR_FD_GetFieldDefn(d, i);
            out.push((
                cstr(gdal_sys::OGR_Fld_GetNameRef(f)),
                cstr(gdal_sys::OGR_GetFieldTypeName(gdal_sys::OGR_Fld_GetType(f))),
            ));
        }
    }
    Ok(out)
}

/// The box (longitude, latitude) in the layer's CRS, for its spatial filter.
fn bbox_in(layer_srs: gdal_sys::OGRSpatialReferenceH, b: [f64; 4]) -> Result<[f64; 4], String> {
    if layer_srs.is_null() {
        return Ok(b);
    }
    // SAFETY: the layer's SRS, cloned by from_c_obj.
    let mut dst = unsafe { SpatialRef::from_c_obj(layer_srs) }.map_err(|e| e.to_string())?;
    let mut src = SpatialRef::from_epsg(4326).map_err(|e| format!("EPSG:4326 is not known to PROJ ({e})"))?;
    src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    dst.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = CoordTransform::new(&src, &dst).map_err(|_| "no transformation to the layer's CRS".to_string())?;
    ct.transform_bounds(&b, 21).map_err(|e| format!("the box could not be placed in the layer's CRS ({e})"))
}

/// Fetches the features of `req` into the GeoPackage `out` (written beside it, then moved in place): `progress`
/// gets the fraction done and a line; `cancel` stops it between features ("cancelled"). Its layer carries the
/// request (metadata JANUS_SOURCE).
pub fn fetch_features(
    req: &FeatureRequest,
    out: &Path,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Fetched, String> {
    let t0 = Instant::now();
    progress(0.0, &format!("connecting to {}", host_of(&req.url)));
    let (_src_ds, src) = open_layer(req)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let cap = req.cap();
    let _q = Quiet::new();
    // SAFETY: handles of the datasets opened here (closed by Ds), C strings alive for each call, every feature
    // created here destroyed once.
    unsafe {
        use gdal_sys::*;
        let srs = OGR_L_GetSpatialRef(src);
        if let Some(b) = req.bbox {
            let r = bbox_in(srs, b)?;
            OGR_L_SetSpatialFilterRect(src, r[0], r[1], r[2], r[3]);
        }
        let w = req.where_clause();
        if !w.is_empty() {
            let cw = c(&w);
            if OGR_L_SetAttributeFilter(src, cw.as_ptr()) != OGRErr::OGRERR_NONE {
                return Err(format!("the filter is not understood: {w} ({})", last_error()));
            }
        }
        // how many the server has (WFS: a "hits" request; OGC API: numberMatched, when it says)
        progress(0.0, "counting the features");
        CPLErrorReset();
        let n = OGR_L_GetFeatureCount(src, i32::from(req.service == Service::Wfs && !req.url.contains("1.0.0")));
        let matched = (n >= 0).then_some(n as u64);
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let expected = matched.map_or(cap, |m| (m as usize).min(cap)).max(1);
        // the GeoPackage, beside its final place
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let part = out.with_extension("part.gpkg");
        let _ = std::fs::remove_file(&part);
        let gpkg = GDALGetDriverByName(c("GPKG").as_ptr());
        if gpkg.is_null() {
            return Err("GDAL has no GeoPackage driver".into());
        }
        let cp = c(&part.to_string_lossy());
        let dst_ds = Ds(GDALCreate(gpkg, cp.as_ptr(), 0, 0, 0, GDALDataType::GDT_Unknown, std::ptr::null_mut()));
        if dst_ds.0.is_null() {
            return Err(format!("{}: {}", part.display(), last_error()));
        }
        let gt = OGR_GT_GetLinear(OGR_L_GetGeomType(src));
        let lname = req.out_layer();
        let lopts = [c("FID=ogc_fid"), c("GEOMETRY_NAME=geom"), c("SPATIAL_INDEX=YES")];
        let mut lp: Vec<*mut c_char> = lopts.iter().map(|o| o.as_ptr() as *mut c_char).collect();
        lp.push(std::ptr::null_mut());
        let dst = GDALDatasetCreateLayer(dst_ds.0, c(&lname).as_ptr(), srs, gt, lp.as_mut_ptr());
        if dst.is_null() {
            return Err(format!("the GeoPackage's layer: {}", last_error()));
        }
        let sd = OGR_L_GetLayerDefn(src);
        let nf = OGR_FD_GetFieldCount(sd);
        let mut map = vec![-1i32; nf.max(0) as usize];
        let mut k = 0;
        for (i, m) in map.iter_mut().enumerate() {
            let fd = OGR_FD_GetFieldDefn(sd, i as i32);
            if cstr(OGR_Fld_GetNameRef(fd)).eq_ignore_ascii_case("ogc_fid") {
                continue;
            }
            if OGR_L_CreateField(dst, fd, 1) == OGRErr::OGRERR_NONE {
                *m = k;
                k += 1;
            }
        }
        let meta = c(&req.describe());
        GDALSetMetadataItem(dst as GDALMajorObjectH, c("JANUS_SOURCE").as_ptr(), meta.as_ptr(), std::ptr::null());
        let dd = OGR_L_GetLayerDefn(dst);
        GDALDatasetStartTransaction(dst_ds.0, 0);
        OGR_L_ResetReading(src);
        CPLErrorReset();
        let (mut count, mut capped) = (0usize, false);
        let mut last = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(dst_ds);
                let _ = std::fs::remove_file(&part);
                return Err("cancelled".into());
            }
            let f = OGR_L_GetNextFeature(src);
            if f.is_null() {
                break;
            }
            if count == cap {
                OGR_F_Destroy(f);
                capped = true;
                break;
            }
            let o = OGR_F_Create(dd);
            OGR_F_SetFromWithMap(o, f, 1, map.as_ptr());
            let g = OGR_F_GetGeometryRef(f);
            if !g.is_null() {
                if OGR_G_HasCurveGeometry(g, 1) != 0 {
                    OGR_F_SetGeometryDirectly(o, OGR_G_GetLinearGeometry(g, 0.0, std::ptr::null_mut()));
                } else {
                    OGR_F_SetGeometry(o, g);
                }
            }
            let ok = OGR_L_CreateFeature(dst, o) == OGRErr::OGRERR_NONE;
            OGR_F_Destroy(o);
            OGR_F_Destroy(f);
            if !ok {
                return Err(format!("writing feature {}: {}", count + 1, last_error()));
            }
            count += 1;
            if last.elapsed().as_millis() > 200 {
                last = Instant::now();
                progress(
                    (count as f64 / expected as f64).min(0.99),
                    &format!("{count} of {} features", matched.map_or(format!("at most {cap}"), |m| m.to_string())),
                );
            }
        }
        if CPLGetLastErrorType() >= CPLErr::CE_Failure {
            let e = last_error();
            if !e.is_empty() {
                return Err(if is_offline_error(&e) {
                    format!("no connection to {} ({e})", host_of(&req.url))
                } else {
                    format!("{}: {e}", host_of(&req.url))
                });
            }
        }
        if GDALDatasetCommitTransaction(dst_ds.0) != OGRErr::OGRERR_NONE {
            return Err(format!("the GeoPackage could not be written: {}", last_error()));
        }
        drop(dst_ds);
        let _ = std::fs::remove_file(out);
        std::fs::rename(&part, out).map_err(|e| format!("{}: {e}", out.display()))?;
        let capped = capped || matched.is_some_and(|m| m as usize > count);
        progress(1.0, &format!("{count} features{}", if capped { " (capped)" } else { "" }));
        Ok(Fetched {
            path: out.to_path_buf(),
            layer: lname,
            features: count,
            capped,
            matched,
            seconds: t0.elapsed().as_secs_f64(),
            cached: false,
        })
    }
}

/// As fetch_features into the request's GeoPackage in `dir` (cache_path), unless one younger than `max_age_s`
/// is there: then at once, nothing fetched. GeoPackages older than a week are deleted from `dir` first.
pub fn fetch_cached(
    req: &FeatureRequest,
    dir: &Path,
    max_age_s: u64,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Fetched, String> {
    let path = req.cache_path(dir);
    let age = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map(|d| d.as_secs())
    };
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "gpkg") && age(&p).is_some_and(|a| a > 7 * 86400) && p != path {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    if age(&path).is_some_and(|a| a <= max_age_s)
        && let Some(n) = gpkg_count(&path, &req.out_layer())
    {
        progress(1.0, &format!("{n} features (cached)"));
        return Ok(Fetched {
            path,
            layer: req.out_layer(),
            features: n,
            capped: n >= req.cap(),
            matched: None,
            seconds: 0.0,
            cached: true,
        });
    }
    fetch_features(req, &path, progress, cancel)
}

/// The features in a GeoPackage's layer (None: not readable).
pub fn gpkg_count(path: &Path, layer: &str) -> Option<usize> {
    use gdal::vector::LayerAccess;
    let ds = gdal::Dataset::open(path).ok()?;
    let l = ds.layer_by_name(layer).ok()?;
    Some(l.feature_count() as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GDAL's drivers registered once (two threads registering at once corrupt the heap).
    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(gdal::DriverManager::register_all);
    }

    #[test]
    fn urls() {
        assert_eq!(guess("https://x/geoserver/ows?service=WFS&request=GetCapabilities"), Some(Service::Wfs));
        assert_eq!(guess("https://x/geoserver/wms"), Some(Service::Wms));
        assert_eq!(guess("https://x/api/collections/lakes/items"), Some(Service::Oapif));
        assert_eq!(guess("https://x/geoserver/ows"), None);
        assert_eq!(base_url("https://x/ows?SERVICE=WMS&request=GetCapabilities&map=a.map"), "https://x/ows?map=a.map");
        assert_eq!(base_url("https://x/ows?service=WFS"), "https://x/ows");
        assert_eq!(with_query("https://x/ows", &[("A", "1 2"), ("B", "x")]), "https://x/ows?A=1%202&B=x");
        assert_eq!(with_query("https://x/ows?", &[("A", "1")]), "https://x/ows?A=1");
        assert_eq!(oapif_base("https://x/api/collections/lakes/items?f=json"), "https://x/api");
        assert_eq!(host_of("https://a.b:8080/x"), "a.b:8080");
        assert!(is_local("http://127.0.0.1:4321/ows"));
        assert!(!is_local("https://terrabrasilis.dpi.inpe.br/geoserver/ows"));
    }

    #[test]
    fn requests() {
        let mut r = FeatureRequest {
            service: Service::Wfs,
            url: "https://x/ows".into(),
            layer: "deter-amz:deter_amz".into(),
            bbox: Some([-55.0, -11.0, -54.0, -10.0]),
            date_field: "view_date".into(),
            from: "2020-01-01".into(),
            to: "2020-12-31".into(),
            ..Default::default()
        };
        assert_eq!(r.where_clause(), "\"view_date\" >= '2020-01-01' AND \"view_date\" < '2021-01-01'");
        let k = r.key();
        assert_eq!(k.len(), 16);
        assert_eq!(r.out_layer(), "deter_amz");
        r.filter = "classname = 'DESMATAMENTO_CR'".into();
        assert_ne!(r.key(), k);
        assert!(r.cache_path(Path::new("c")).to_string_lossy().contains("deter_amz_"));
        assert_eq!(next_day("2024-02-28"), "2024-02-29");
        assert_eq!(next_day("2023-12-31"), "2024-01-01");
    }

    #[test]
    fn times() {
        let v: Vec<String> = ["2020-01-01T03:00:00.000Z", "2020-02-01T03:00:00.000Z", "2020-03-01T03:00:00.000Z"]
            .map(String::from)
            .into();
        assert_eq!(time_at(&v, "2020-02-15").as_deref(), Some("2020-02-01T03:00:00.000Z"));
        assert_eq!(time_at(&v, "2019-12-31"), None);
        let r = vec!["2016-08-01/2030-12-01/P1M".to_string()];
        assert_eq!(time_at(&r, "2020-02-15").as_deref(), Some("2020-02-15"));
    }

    #[test]
    fn wms_and_wfs_capabilities() {
        setup();
        let wms = r#"<?xml version="1.0"?><WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms"
            xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Test</Title></Service><Capability><Request>
            <GetMap><Format>image/png</Format><Format>image/jpeg</Format><DCPType><HTTP><Get><OnlineResource
            xlink:href="http://h/ows?SERVICE=WMS&amp;"/></Get></HTTP></DCPType></GetMap></Request>
            <Layer><Title>root</Title><CRS>EPSG:4326</CRS><CRS>EPSG:3857</CRS>
            <Layer queryable="1"><Name>ws:alerts</Name><Title>Alerts</Title><Abstract>Daily</Abstract>
            <EX_GeographicBoundingBox><westBoundLongitude>-60</westBoundLongitude><eastBoundLongitude>-50</eastBoundLongitude>
            <southBoundLatitude>-12</southBoundLatitude><northBoundLatitude>-2</northBoundLatitude></EX_GeographicBoundingBox>
            <Dimension name="time" default="2020-02-01" units="ISO8601">2020-01-01,2020-02-01</Dimension>
            <Style><Name>red</Name></Style></Layer></Layer></Capability></WMS_Capabilities>"#;
        let c = parse_wms(&Xml::parse(wms).unwrap()).unwrap();
        assert_eq!(c.version, "1.3.0");
        assert_eq!(c.url, "http://h/ows");
        assert_eq!(c.formats, ["image/png", "image/jpeg"]);
        assert_eq!(c.layers.len(), 1);
        let l = &c.layers[0];
        assert_eq!((l.name.as_str(), l.title.as_str(), l.abstract_text.as_str()), ("ws:alerts", "Alerts", "Daily"));
        assert_eq!(l.bbox, Some([-60.0, -12.0, -50.0, -2.0]));
        assert_eq!(l.times, ["2020-01-01", "2020-02-01"]);
        assert_eq!(l.time_default, "2020-02-01");
        assert_eq!(l.styles, ["red"]);
        assert_eq!(l.web_mercator_code(), "EPSG:3857");
        assert!(l.matches("ALERTS daily"));
        let wfs = r#"<wfs:WFS_Capabilities version="2.0.0" xmlns:wfs="http://www.opengis.net/wfs/2.0"
            xmlns:ows="http://www.opengis.net/ows/1.1"><ows:OperationsMetadata><ows:Constraint name="ImplementsResultPaging">
            <ows:NoValues/><ows:DefaultValue>TRUE</ows:DefaultValue></ows:Constraint></ows:OperationsMetadata>
            <wfs:FeatureTypeList><wfs:FeatureType><wfs:Name>ws:mun</wfs:Name><wfs:Title>Municipalities</wfs:Title>
            <wfs:DefaultCRS>urn:ogc:def:crs:EPSG::4674</wfs:DefaultCRS><ows:WGS84BoundingBox><ows:LowerCorner>-74 -34</ows:LowerCorner>
            <ows:UpperCorner>-28 5</ows:UpperCorner></ows:WGS84BoundingBox></wfs:FeatureType></wfs:FeatureTypeList></wfs:WFS_Capabilities>"#;
        let c = parse_wfs(&Xml::parse(wfs).unwrap()).unwrap();
        assert!(c.paging);
        assert_eq!(c.layers[0].name, "ws:mun");
        assert_eq!(c.layers[0].bbox, Some([-74.0, -34.0, -28.0, 5.0]));
        let e = Xml::parse("<ServiceExceptionReport><ServiceException code=\"x\">Bad layer</ServiceException></ServiceExceptionReport>").unwrap();
        assert_eq!(exception_text(&e).as_deref(), Some("Bad layer"));
    }

    #[test]
    fn wmts_capabilities() {
        setup();
        let mut matrices = String::new();
        for z in 0..=3 {
            matrices += &format!(
                "<TileMatrix><ows:Identifier>EPSG:900913:{z}</ows:Identifier><ScaleDenominator>{}</ScaleDenominator>\
                 <TopLeftCorner>-20037508.34 20037508.34</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight>\
                 <MatrixWidth>{}</MatrixWidth><MatrixHeight>{}</MatrixHeight></TileMatrix>",
                MERCATOR_SD0 / f64::from(1 << z),
                1 << z,
                1 << z
            );
        }
        let x = format!(
            r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0">
            <Contents><Layer><ows:Title>Roads</ows:Title><ows:Identifier>roads</ows:Identifier>
            <Style isDefault="true"><ows:Identifier>default</ows:Identifier></Style><Format>image/png</Format>
            <TileMatrixSetLink><TileMatrixSet>EPSG:900913</TileMatrixSet></TileMatrixSetLink>
            <ResourceURL format="image/png" resourceType="tile" template="http://h/wmts/roads/{{Style}}/{{TileMatrixSet}}/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.png"/>
            </Layer><TileMatrixSet><ows:Identifier>EPSG:900913</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::900913</ows:SupportedCRS>{matrices}</TileMatrixSet></Contents></Capabilities>"#
        );
        let c = parse_wmts(&Xml::parse(&x).unwrap()).unwrap();
        let (t, z) = c.layers[0].tiles.clone().unwrap();
        assert_eq!(z, 3);
        assert_eq!(t, "http://h/wmts/roads/{Style}/EPSG:900913/EPSG:900913:{z}/{y}/{x}.png");
        let s = map_source(&MapLayerSpec {
            service: Service::Wmts,
            tiles: t,
            style: "default".into(),
            format: "image/png".into(),
            url: "http://h/wmts".into(),
            max_zoom: z,
            ..Default::default()
        });
        assert_eq!(s.url, "http://h/wmts/roads/default/EPSG:900913/EPSG:900913:{z}/{y}/{x}.png");
        assert!(crate::basemap::url_problem(&s.url).is_none());
    }

    #[test]
    fn oapif_collections() {
        let j = br#"{"collections":[{"id":"lakes","title":"Large Lakes","description":"lakes of the world",
            "extent":{"spatial":{"bbox":[[-180,-90,180,90]]},"temporal":{"interval":[["2011-11-11T11:11:11Z",null]]}}}]}"#;
        let c = parse_oapif(j, "https://x/api").unwrap();
        assert_eq!(c.layers[0].name, "lakes");
        assert_eq!(c.layers[0].bbox, Some([-180.0, -90.0, 180.0, 90.0]));
        assert_eq!(c.layers[0].time_default, "2011-11-11T11:11:11Z/..");
    }
}
