//! A small OGC server on 127.0.0.1 for the tests and the UI self-test (no network): WMS 1.3.0 (GetCapabilities,
//! GetMap of PNGs whose colour says the box and the time asked for), WFS 2.0 (GetCapabilities,
//! DescribeFeatureType, GetFeature in GML 3.2 with a BBOX filter, hits and paging) and OGC API - Features (landing
//! page, conformance, collections, items in GeoJSON with bbox, limit and next links). Its features are squares
//! on a grid in a UTM zone, the same in both. Every request is logged.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};

/// The grid of squares served: `nx` × `ny` cells of `cell` metres from (x0, y0) (top left) in EPSG `epsg`, each
/// holding a square of `side` metres.
#[derive(Clone, Debug)]
pub struct MockData {
    pub epsg: u32,
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub side: f64,
    pub nx: usize,
    pub ny: usize,
    /// OGC API's largest page (the client pages with its next links)
    pub max_page: usize,
}

impl Default for MockData {
    /// Over the self-test's series A (UTM 23S, 500000-509000 E, 7494000-7500000 N): 10 × 6 squares.
    fn default() -> Self {
        MockData { epsg: 32723, x0: 500000.0, y0: 7500000.0, cell: 900.0, side: 600.0, nx: 10, ny: 6, max_page: 400 }
    }
}

/// A served feature: its number (from 1), name, day, area, square (x0, y0, x1, y1 in the grid's CRS) and its
/// corners in longitude and latitude.
#[derive(Clone, Debug)]
pub struct MockFeature {
    pub id: usize,
    pub name: String,
    pub day: String,
    pub area_ha: f64,
    pub square: [f64; 4],
    pub lonlat: Vec<[f64; 2]>,
}

impl MockData {
    pub fn features(&self) -> Vec<MockFeature> {
        let ct = to_lonlat(self.epsg);
        let mut out = Vec::new();
        for j in 0..self.ny {
            for i in 0..self.nx {
                let id = out.len() + 1;
                let m = (self.cell - self.side) / 2.0;
                let x0 = self.x0 + i as f64 * self.cell + m;
                let y1 = self.y0 - j as f64 * self.cell - m;
                let sq = [x0, y1 - self.side, x0 + self.side, y1];
                let corners = [[sq[0], sq[1]], [sq[2], sq[1]], [sq[2], sq[3]], [sq[0], sq[3]], [sq[0], sq[1]]];
                let lonlat = corners
                    .iter()
                    .map(|p| {
                        let (mut x, mut y) = ([p[0]], [p[1]]);
                        if let Some(ct) = &ct {
                            let _ = ct.transform_coords(&mut x, &mut y, &mut []);
                        }
                        [x[0], y[0]]
                    })
                    .collect();
                out.push(MockFeature {
                    id,
                    name: format!("plot {id}"),
                    day: day_of(id),
                    area_ha: self.side * self.side / 10000.0,
                    square: sq,
                    lonlat,
                });
            }
        }
        out
    }
}

/// 2020-01-01 plus (id - 1) mod 366 days.
fn day_of(id: usize) -> String {
    let mut d = ((id - 1) % 366) as i32;
    let days = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0;
    while d >= days[m] {
        d -= days[m];
        m += 1;
    }
    format!("2020-{:02}-{:02}", m + 1, d + 1)
}

fn to_lonlat(epsg: u32) -> Option<CoordTransform> {
    let mut a = SpatialRef::from_epsg(epsg).ok()?;
    let mut b = SpatialRef::from_epsg(4326).ok()?;
    a.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    b.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    CoordTransform::new(&a, &b).ok()
}

/// The colour of a GetMap image: by its box (Web Mercator metres) and its TIME.
pub fn map_colour(bbox: [f64; 4], time: &str) -> [u8; 4] {
    let r = 30 + ((bbox[0] / 97.0).floor() as i64).rem_euclid(200) as u8;
    let g = 30 + ((bbox[1] / 89.0).floor() as i64).rem_euclid(200) as u8;
    let b = if time.starts_with("2020-01") {
        220
    } else if time.is_empty() {
        120
    } else {
        40
    };
    [r, g, b, 255]
}

pub struct MockServer {
    pub port: u16,
    pub data: MockData,
    log: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wakes accept()
    }
}

struct Ctx {
    data: MockData,
    features: Vec<MockFeature>,
    port: u16,
    log: Arc<Mutex<Vec<String>>>,
}

impl MockServer {
    /// Listens on a free port of 127.0.0.1; a thread per connection (each answered and closed).
    pub fn start(data: MockData) -> std::io::Result<MockServer> {
        let l = TcpListener::bind("127.0.0.1:0")?;
        let port = l.local_addr()?.port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(Ctx { features: data.features(), data: data.clone(), port, log: log.clone() });
        let st = stop.clone();
        std::thread::Builder::new().name("ogc-mock".into()).spawn(move || {
            for s in l.incoming() {
                if st.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(s) = s else { continue };
                let c = ctx.clone();
                let _ = std::thread::Builder::new().name("ogc-mock-conn".into()).spawn(move || serve(s, &c));
            }
        })?;
        Ok(MockServer { port, data, log, stop })
    }

    /// http://127.0.0.1:port + path ("/ows", "/oapi").
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The requests so far ("GET /path?query").
    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match h {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The query's parameters, keys in capitals.
fn params(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (decode(k).to_ascii_uppercase(), decode(v))
        })
        .collect()
}

fn param<'a>(p: &'a [(String, String)], k: &str) -> &'a str {
    p.iter().find(|x| x.0 == k).map_or("", |x| x.1.as_str())
}

fn serve(mut s: TcpStream, c: &Ctx) {
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 1 << 20 {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let line = head.lines().next().unwrap_or("").to_string();
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    if method.is_empty() {
        return;
    }
    c.log.lock().unwrap().push(format!("{method} {target}"));
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let p = params(q);
    let (status, ctype, body) = route(c, path, &p);
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    if method != "HEAD" {
        out.extend_from_slice(&body);
    }
    let _ = s.write_all(&out);
    let _ = s.flush();
}

type Answer = (&'static str, &'static str, Vec<u8>);

fn xml(s: String) -> Answer {
    ("200 OK", "text/xml; charset=UTF-8", s.into_bytes())
}

fn json(v: serde_json::Value) -> Answer {
    ("200 OK", "application/json", v.to_string().into_bytes())
}

fn not_found() -> Answer {
    ("404 Not Found", "text/plain", b"not found".to_vec())
}

fn route(c: &Ctx, path: &str, p: &[(String, String)]) -> Answer {
    if path == "/ows" || path == "/wms" || path == "/wfs" {
        let service = param(p, "SERVICE").to_ascii_uppercase();
        let request = param(p, "REQUEST").to_ascii_lowercase();
        return match (service.as_str(), request.as_str()) {
            ("WMS", "getcapabilities") => xml(wms_capabilities(c)),
            ("WMS", "getmap") | ("", "getmap") => get_map(p),
            ("WFS", "getcapabilities") => xml(wfs_capabilities(c)),
            (_, "describefeaturetype") => xml(DESCRIBE.to_string()),
            (_, "getfeature") => xml(get_feature(c, p)),
            _ => xml("<ows:ExceptionReport xmlns:ows=\"http://www.opengis.net/ows/1.1\"><ows:Exception>\
                 <ows:ExceptionText>unknown request</ows:ExceptionText></ows:Exception></ows:ExceptionReport>"
                .into()),
        };
    }
    if path == "/error" {
        return ("500 Internal Server Error", "text/plain", b"the server failed".to_vec());
    }
    if let Some(rest) = path.strip_prefix("/oapi") {
        return oapi(c, rest.trim_end_matches('/'), p);
    }
    not_found()
}

fn lonlat_box(c: &Ctx) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for f in &c.features {
        for q in &f.lonlat {
            b = [b[0].min(q[0]), b[1].min(q[1]), b[2].max(q[0]), b[3].max(q[1])];
        }
    }
    b
}

fn wms_capabilities(c: &Ctx) -> String {
    let b = lonlat_box(c);
    let url = format!("http://127.0.0.1:{}/ows?", c.port);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Name>WMS</Name><Title>Janus mock WMS</Title></Service>
<Capability><Request>
<GetCapabilities><Format>text/xml</Format><DCPType><HTTP><Get><OnlineResource xlink:href="{url}"/></Get></HTTP></DCPType></GetCapabilities>
<GetMap><Format>image/png</Format><Format>image/jpeg</Format><DCPType><HTTP><Get><OnlineResource xlink:href="{url}"/></Get></HTTP></DCPType></GetMap>
</Request><Exception><Format>XML</Format></Exception>
<Layer><Title>Janus mock</Title><CRS>EPSG:4326</CRS><CRS>EPSG:3857</CRS>
<Layer queryable="1" opaque="0"><Name>test:alerts</Name><Title>Test alerts</Title><Abstract>Colours by box and time</Abstract>
<EX_GeographicBoundingBox><westBoundLongitude>{}</westBoundLongitude><eastBoundLongitude>{}</eastBoundLongitude><southBoundLatitude>{}</southBoundLatitude><northBoundLatitude>{}</northBoundLatitude></EX_GeographicBoundingBox>
<Dimension name="time" default="2020-03-01" units="ISO8601">2020-01-01,2020-02-01,2020-03-01</Dimension>
<Style><Name>default</Name><Title>Default</Title></Style>
<Attribution><Title>Janus mock data</Title></Attribution>
</Layer>
<Layer queryable="1"><Name>test:plain</Name><Title>Plain layer</Title></Layer>
</Layer></Capability></WMS_Capabilities>"#,
        b[0], b[2], b[1], b[3]
    )
}

fn numbers(s: &str) -> Vec<f64> {
    s.split([',', ' ']).filter_map(|v| v.trim().parse().ok()).collect()
}

static PNGS: AtomicU64 = AtomicU64::new(0);

/// A WIDTH × HEIGHT PNG of map_colour's colour (GDAL's PNG driver through /vsimem).
pub fn png_of(w: usize, h: usize, rgba: [u8; 4]) -> Option<Vec<u8>> {
    use gdal::raster::Buffer;
    let mem = gdal::DriverManager::get_driver_by_name("MEM").ok()?;
    let ds = mem.create_with_band_type::<u8, _>("", w, h, 4).ok()?;
    for (k, v) in rgba.iter().enumerate() {
        let mut b = Buffer::new((w, h), vec![*v; w * h]);
        ds.rasterband(k + 1).ok()?.write((0, 0), (w, h), &mut b).ok()?;
    }
    let png = gdal::DriverManager::get_driver_by_name("PNG").ok()?;
    let name = format!("/vsimem/janus_mock_{}.png", PNGS.fetch_add(1, Ordering::Relaxed));
    drop(ds.create_copy(&png, &name, &Default::default()).ok()?);
    gdal::vsi::get_vsi_mem_file_bytes_owned(&name).ok()
}

fn get_map(p: &[(String, String)]) -> Answer {
    let b = numbers(param(p, "BBOX"));
    let (w, h) = (param(p, "WIDTH").parse().unwrap_or(256), param(p, "HEIGHT").parse().unwrap_or(256));
    if b.len() != 4 || param(p, "LAYERS") != "test:alerts" {
        return xml("<ServiceExceptionReport version=\"1.3.0\"><ServiceException code=\"LayerNotDefined\">\
             unknown layer or box</ServiceException></ServiceExceptionReport>"
            .into());
    }
    match png_of(w, h, map_colour([b[0], b[1], b[2], b[3]], param(p, "TIME"))) {
        Some(png) => ("200 OK", "image/png", png),
        None => ("500 Internal Server Error", "text/plain", b"no PNG".to_vec()),
    }
}

fn wfs_capabilities(c: &Ctx) -> String {
    let b = lonlat_box(c);
    let url = format!("http://127.0.0.1:{}/ows", c.port);
    let op = |n: &str| {
        format!(
            r#"<ows:Operation name="{n}"><ows:DCP><ows:HTTP><ows:Get xlink:href="{url}"/></ows:HTTP></ows:DCP></ows:Operation>"#
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<wfs:WFS_Capabilities version="2.0.0" xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:ows="http://www.opengis.net/ows/1.1"
 xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:fes="http://www.opengis.net/fes/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:test="http://janus.test">
<ows:ServiceIdentification><ows:Title>Janus mock WFS</ows:Title><ows:ServiceType>WFS</ows:ServiceType><ows:ServiceTypeVersion>2.0.0</ows:ServiceTypeVersion></ows:ServiceIdentification>
<ows:ServiceProvider><ows:ProviderName>Janus</ows:ProviderName></ows:ServiceProvider>
<ows:OperationsMetadata>{}{}{}
<ows:Constraint name="ImplementsBasicWFS"><ows:NoValues/><ows:DefaultValue>TRUE</ows:DefaultValue></ows:Constraint>
<ows:Constraint name="ImplementsResultPaging"><ows:NoValues/><ows:DefaultValue>TRUE</ows:DefaultValue></ows:Constraint>
</ows:OperationsMetadata>
<wfs:FeatureTypeList><wfs:FeatureType><wfs:Name>test:plots</wfs:Name><wfs:Title>Test plots</wfs:Title>
<wfs:Abstract>Squares over the self-test's series</wfs:Abstract><wfs:DefaultCRS>urn:ogc:def:crs:EPSG::{}</wfs:DefaultCRS>
<ows:WGS84BoundingBox><ows:LowerCorner>{} {}</ows:LowerCorner><ows:UpperCorner>{} {}</ows:UpperCorner></ows:WGS84BoundingBox>
</wfs:FeatureType></wfs:FeatureTypeList>
<fes:Filter_Capabilities><fes:Conformance>
<fes:Constraint name="ImplementsQuery"><ows:NoValues/><ows:DefaultValue>TRUE</ows:DefaultValue></fes:Constraint>
<fes:Constraint name="ImplementsSpatialFilter"><ows:NoValues/><ows:DefaultValue>TRUE</ows:DefaultValue></fes:Constraint>
</fes:Conformance><fes:Spatial_Capabilities><fes:GeometryOperands><fes:GeometryOperand name="gml:Envelope"/></fes:GeometryOperands>
<fes:SpatialOperators><fes:SpatialOperator name="BBOX"/></fes:SpatialOperators></fes:Spatial_Capabilities></fes:Filter_Capabilities>
</wfs:WFS_Capabilities>"#,
        op("GetCapabilities"),
        op("DescribeFeatureType"),
        op("GetFeature"),
        c.data.epsg,
        b[0],
        b[1],
        b[2],
        b[3]
    )
}

const DESCRIBE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:test="http://janus.test"
 targetNamespace="http://janus.test" elementFormDefault="qualified">
<xsd:import namespace="http://www.opengis.net/gml/3.2" schemaLocation="http://schemas.opengis.net/gml/3.2.1/gml.xsd"/>
<xsd:complexType name="plotsType"><xsd:complexContent><xsd:extension base="gml:AbstractFeatureType"><xsd:sequence>
<xsd:element name="name" type="xsd:string" minOccurs="0"/>
<xsd:element name="day" type="xsd:date" minOccurs="0"/>
<xsd:element name="area_ha" type="xsd:double" minOccurs="0"/>
<xsd:element name="geom" type="gml:SurfacePropertyType" minOccurs="0"/>
</xsd:sequence></xsd:extension></xsd:complexContent></xsd:complexType>
<xsd:element name="plots" type="test:plotsType" substitutionGroup="gml:AbstractFeature"/>
</xsd:schema>"#;

/// The features in a box of the grid's CRS (the FILTER's BBOX, or the BBOX parameter), None: all.
fn wfs_box(p: &[(String, String)]) -> Option<[f64; 4]> {
    let f = param(p, "FILTER");
    if !f.is_empty() {
        let corner = |tag: &str| {
            let a = f.find(&format!("{tag}>"))? + tag.len() + 1;
            let e = f[a..].find('<')? + a;
            let v = numbers(&f[a..e]);
            (v.len() >= 2).then(|| [v[0], v[1]])
        };
        if let (Some(lo), Some(hi)) = (corner("lowerCorner"), corner("upperCorner")) {
            return Some([lo[0], lo[1], hi[0], hi[1]]);
        }
    }
    let b = numbers(param(p, "BBOX"));
    (b.len() >= 4).then(|| [b[0], b[1], b[2], b[3]])
}

fn overlaps(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

fn get_feature(c: &Ctx, p: &[(String, String)]) -> String {
    let bx = wfs_box(p);
    let sel: Vec<&MockFeature> = c.features.iter().filter(|f| bx.is_none_or(|b| overlaps(&f.square, &b))).collect();
    let ns = r#"xmlns:wfs="http://www.opengis.net/wfs/2.0" xmlns:gml="http://www.opengis.net/gml/3.2" xmlns:test="http://janus.test""#;
    if param(p, "RESULTTYPE").eq_ignore_ascii_case("hits") {
        return format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><wfs:FeatureCollection {ns} numberMatched="{}" numberReturned="0" timeStamp="2020-01-01T00:00:00Z"/>"#,
            sel.len()
        );
    }
    let start: usize = param(p, "STARTINDEX").parse().unwrap_or(0);
    let count: usize = param(p, "COUNT").parse().or_else(|_| param(p, "MAXFEATURES").parse()).unwrap_or(usize::MAX);
    let page: Vec<&&MockFeature> = sel.iter().skip(start).take(count).collect();
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><wfs:FeatureCollection {ns} numberMatched="{}" numberReturned="{}" timeStamp="2020-01-01T00:00:00Z">"#,
        sel.len(),
        page.len()
    );
    let srs = format!("urn:ogc:def:crs:EPSG::{}", c.data.epsg);
    for f in page {
        let q = f.square;
        s += &format!(
            "<wfs:member><test:plots gml:id=\"plots.{id}\"><test:name>{}</test:name><test:day>{}</test:day>\
             <test:area_ha>{}</test:area_ha><test:geom><gml:Polygon gml:id=\"g{id}\" srsName=\"{srs}\"><gml:exterior>\
             <gml:LinearRing><gml:posList>{x0} {y0} {x1} {y0} {x1} {y1} {x0} {y1} {x0} {y0}</gml:posList></gml:LinearRing>\
             </gml:exterior></gml:Polygon></test:geom></test:plots></wfs:member>",
            f.name,
            f.day,
            f.area_ha,
            id = f.id,
            x0 = q[0],
            y0 = q[1],
            x1 = q[2],
            y1 = q[3]
        );
    }
    s + "</wfs:FeatureCollection>"
}

fn oapi(c: &Ctx, rest: &str, p: &[(String, String)]) -> Answer {
    let base = format!("http://127.0.0.1:{}/oapi", c.port);
    let link = |rel: &str, href: String, t: &str| serde_json::json!({"rel": rel, "href": href, "type": t});
    let b = lonlat_box(c);
    let collection = serde_json::json!({
        "id": "parcels",
        "title": "Test parcels",
        "description": "Squares over the self-test's series",
        "extent": {"spatial": {"bbox": [b]}, "temporal": {"interval": [["2020-01-01T00:00:00Z", null]]}},
        "itemType": "feature",
        "crs": ["http://www.opengis.net/def/crs/OGC/1.3/CRS84"],
        "links": [link("items", format!("{base}/collections/parcels/items"), "application/geo+json")]
    });
    match rest {
        "" => json(serde_json::json!({
            "title": "Janus mock OGC API",
            "links": [
                link("self", base.clone(), "application/json"),
                link("conformance", format!("{base}/conformance"), "application/json"),
                link("data", format!("{base}/collections"), "application/json")
            ]
        })),
        "/conformance" => json(serde_json::json!({"conformsTo": [
            "http://www.opengis.net/spec/ogcapi-features-1/1.0/conf/core",
            "http://www.opengis.net/spec/ogcapi-features-1/1.0/conf/geojson"
        ]})),
        "/collections" => json(serde_json::json!({"collections": [collection], "links": []})),
        "/collections/parcels" => json(collection),
        "/collections/parcels/items" => {
            let bb = numbers(param(p, "BBOX"));
            let sel: Vec<&MockFeature> =
                c.features
                    .iter()
                    .filter(|f| {
                        bb.len() < 4 || {
                            let fb =
                                f.lonlat.iter().fold(
                                    [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY],
                                    |a, q| [a[0].min(q[0]), a[1].min(q[1]), a[2].max(q[0]), a[3].max(q[1])],
                                );
                            overlaps(&fb, &[bb[0], bb[1], bb[2], bb[3]])
                        }
                    })
                    .filter(|f| {
                        // datetime: an instant or start/end (".." or "" open), on the day; the end excluded when it
                        // has a time (as GDAL asks for a range of whole days)
                        let dt = param(p, "DATETIME");
                        if dt.is_empty() {
                            return true;
                        }
                        let (a, b) = dt.split_once('/').unwrap_or((dt, dt));
                        let day = |s: &str| s.get(..10).unwrap_or("").to_string();
                        let lo = (a != ".." && !a.is_empty()).then(|| day(a));
                        let hi = (b != ".." && !b.is_empty()).then(|| (day(b), b.len() > 10));
                        lo.is_none_or(|lo| f.day >= lo)
                            && hi.is_none_or(|(hi, open)| if open { f.day < hi } else { f.day <= hi })
                    })
                    .collect();
            let limit = param(p, "LIMIT").parse::<usize>().unwrap_or(10).clamp(1, c.data.max_page);
            let offset: usize = param(p, "OFFSET").parse().unwrap_or(0);
            let page: Vec<serde_json::Value> = sel
                .iter()
                .skip(offset)
                .take(limit)
                .map(|f| {
                    serde_json::json!({
                        "type": "Feature",
                        "id": f.id,
                        "properties": {"name": f.name, "day": f.day, "area_ha": f.area_ha},
                        "geometry": {"type": "Polygon", "coordinates": [f.lonlat]}
                    })
                })
                .collect();
            let mut links = vec![];
            if offset + page.len() < sel.len() {
                // the request's other parameters kept
                let mut q = format!("limit={limit}&offset={}", offset + page.len());
                for (k, v) in p.iter().filter(|(k, _)| k != "LIMIT" && k != "OFFSET") {
                    q += &format!("&{}={}", k.to_ascii_lowercase(), crate::ogc::encode(v));
                }
                links.push(link("next", format!("{base}/collections/parcels/items?{q}"), "application/geo+json"));
            }
            let n = page.len();
            let body = serde_json::json!({
                "type": "FeatureCollection",
                "numberMatched": sel.len(),
                "numberReturned": n,
                "features": page,
                "links": links
            });
            ("200 OK", "application/geo+json", body.to_string().into_bytes())
        }
        _ => not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_params() {
        assert_eq!(day_of(1), "2020-01-01");
        assert_eq!(day_of(32), "2020-02-01");
        assert_eq!(day_of(366), "2020-12-31");
        let p = params("service=WMS&BBOX=1%2C2,3,4&a+b=c");
        assert_eq!(param(&p, "SERVICE"), "WMS");
        assert_eq!(param(&p, "BBOX"), "1,2,3,4");
        assert_eq!(decode("100%"), "100%");
    }
}
