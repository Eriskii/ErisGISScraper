//! A fake county GIS server on loopback: a 12×12 grid of square parcels around a point, served
//! as an ArcGIS FeatureServer (GeoJSON and Esri JSON), a WFS, and an OGC API Features
//! collection, plus broken variants that real servers are known for; and a zoning layer over
//! the same ground, nine districts of 4×4 parcels each.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub const SIDE: usize = 12;
pub const CELL: f64 = 0.0008;

#[derive(Clone)]
pub struct Parcel {
    pub id: usize,
    /// west, south, east, north
    pub bounds: [f64; 4],
}

pub struct Grid {
    pub parcels: Vec<Parcel>,
}

impl Grid {
    pub fn around(x: f64, y: f64) -> Self {
        Self::around_all(&[(x, y)])
    }

    /// A grid around each point.
    pub fn around_all(points: &[(f64, f64)]) -> Self {
        let mut parcels = Vec::new();
        for &(x, y) in points {
            let start = (x - CELL * SIDE as f64 / 2.0, y - CELL * SIDE as f64 / 2.0);
            for i in 0..SIDE * SIDE {
                let (col, row) = ((i % SIDE) as f64, (i / SIDE) as f64);
                let w = start.0 + col * CELL;
                let s = start.1 + row * CELL;
                let id = parcels.len();
                parcels.push(Parcel { id, bounds: [w + 0.00005, s + 0.00005, w + CELL - 0.00005, s + CELL - 0.00005] });
            }
        }
        Self { parcels }
    }

    fn within(&self, bbox: [f64; 4]) -> Vec<&Parcel> {
        self.parcels
            .iter()
            .filter(|p| {
                p.bounds[0] <= bbox[2] && bbox[0] <= p.bounds[2] && p.bounds[1] <= bbox[3] && bbox[1] <= p.bounds[3]
            })
            .collect()
    }
}

fn ring(b: [f64; 4], clockwise: bool) -> Value {
    let [w, s, e, n] = b;
    if clockwise {
        json!([[w, s], [w, n], [e, n], [e, s], [w, s]])
    } else {
        json!([[w, s], [e, s], [e, n], [w, n], [w, s]])
    }
}

fn properties(p: &Parcel) -> Value {
    json!({"OBJECTID": p.id + 1, "PARCELNO": format!("0810-{:04}", p.id), "OWNER": if p.id.is_multiple_of(4) { Value::Null } else { json!(format!("Owner {}", p.id)) }, "SITEADDR": format!("{} Main St", 100 + p.id), "GISACRES": 1.25})
}

fn mercator_inverse(lon: f64, lat: f64) -> (f64, f64) {
    const R: f64 = 6_378_137.0;
    (lon.to_radians() * R, ((std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0).tan()).ln() * R)
}

fn bbox_param(text: &str) -> [f64; 4] {
    let v: Vec<f64> = text.split(',').take(4).map(|s| s.parse().unwrap()).collect();
    [v[0], v[1], v[2], v[3]]
}

fn param<'a>(q: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
}

async fn arcgis(
    Path(variant): Path<String>,
    State(grid): State<Arc<Grid>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if variant == "refuses" {
        return axum::Json(json!({"error": {"code": 400, "message": "Invalid query parameters", "details": []}}))
            .into_response();
    }
    // Like ArcGIS Online timing out inside, or its gateway giving up: every other query fails,
    // and asking again works.
    static FLAKY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if variant.starts_with("flaky") && FLAKY.fetch_add(1, std::sync::atomic::Ordering::SeqCst).is_multiple_of(2) {
        if variant == "flaky-gateway" {
            return (axum::http::StatusCode::GATEWAY_TIMEOUT, "504 Gateway Time-out").into_response();
        }
        return axum::Json(
            json!({"error": {"code": 400, "message": "Unable to perform query. Please check your parameters.", "details": []}}),
        )
        .into_response();
    }
    if variant == "stalls" {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
    if q.get("returnCountOnly").is_some_and(|v| v == "true") {
        return axum::Json(json!({"count": grid.parcels.len()})).into_response();
    }
    if q.get("returnExtentOnly").is_some_and(|v| v == "true") {
        return axum::Json(extent(grid.parcels.iter().map(|p| p.bounds))).into_response();
    }
    let all = grid.within(bbox_param(&q["geometry"]));
    let offset: usize =
        if variant == "ignores-offset" { 0 } else { q.get("resultOffset").map_or(0, |s| s.parse().unwrap()) };
    let max_record = if variant == "whole-pages" { 2000 } else { 50usize };
    let count = q.get("resultRecordCount").map_or(max_record, |s| s.parse::<usize>().unwrap().min(max_record));
    // Like a county with condominiums: every tenth parcel is a building of three units, each
    // its own parcel on the building's outline; every tenth from the fifth, two units sharing
    // one parcel number. The second unit's ring starts at another corner.
    let units = |p: &Parcel| match (variant == "condos", p.id % 10) {
        (true, 0) => 3,
        (true, 5) => 2,
        _ => 1,
    };
    let expanded: Vec<(&Parcel, usize)> =
        all.iter().flat_map(|p| (1..=units(p)).map(move |unit| (*p, if units(p) > 1 { unit } else { 0 }))).collect();
    let page: Vec<(&Parcel, usize)> = expanded.iter().skip(offset).take(count).copied().collect();
    let exceeded = offset + page.len() < expanded.len();
    let unit_properties = |p: &Parcel, unit: usize| {
        let mut properties = properties(p);
        if unit > 0 {
            if p.id.is_multiple_of(10) {
                properties["PARCELNO"] = json!(format!("0810-{:04}-U{unit}", p.id));
            }
            properties["SITEADDR"] = json!(format!("{} Main St Unit {unit}", 100 + p.id));
        }
        properties
    };
    if q["f"] == "geojson" {
        let features: Vec<Value> = page
            .iter()
            .map(|&(p, unit)| {
                let mut coordinates = ring(p.bounds, false);
                if unit == 2 {
                    let points = coordinates.as_array_mut().unwrap();
                    points.pop();
                    points.rotate_left(2);
                    let first = points[0].clone();
                    points.push(first);
                }
                // Like layers digitized carelessly: every third parcel's ring crosses itself (two
                // corners swapped), or collapses onto its diagonal.
                if p.id.is_multiple_of(3) && (variant == "bowties" || variant == "collapsed") {
                    let [w, s, e, n] = p.bounds;
                    coordinates = if variant == "bowties" {
                        json!([[w, s], [e, n], [e, s], [w, n], [w, s]])
                    } else {
                        json!([[w, s], [e, n], [w, s], [e, n], [w, s]])
                    };
                }
                if variant == "mercator" {
                    for point in coordinates.as_array_mut().unwrap() {
                        let (x, y) = mercator_inverse(point[0].as_f64().unwrap(), point[1].as_f64().unwrap());
                        *point = json!([x, y]);
                    }
                }
                json!({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [coordinates]}, "properties": unit_properties(p, unit)})
            })
            .collect();
        let mut body = json!({"type": "FeatureCollection", "features": features});
        if exceeded {
            body["properties"] = json!({"exceededTransferLimit": true});
        }
        axum::Json(body).into_response()
    } else {
        let features: Vec<Value> = page
            .iter()
            .map(|&(p, unit)| json!({"geometry": {"rings": [ring(p.bounds, true)]}, "attributes": unit_properties(p, unit)}))
            .collect();
        axum::Json(json!({"features": features, "exceededTransferLimit": exceeded})).into_response()
    }
}

async fn wfs(State(grid): State<Arc<Grid>>, Query(q): Query<HashMap<String, String>>) -> Response {
    // WFS 2.0 with EPSG:4326: latitude first, in the box and in the answer.
    let b = bbox_param(param(&q, "bbox").unwrap());
    let all = grid.within([b[1], b[0], b[3], b[2]]);
    let offset: usize = param(&q, "startIndex").map_or(0, |s| s.parse().unwrap());
    let count: usize = param(&q, "count").map_or(1000, |s| s.parse().unwrap());
    let features: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(count)
        .map(|p| {
            let mut r = ring(p.bounds, false);
            for point in r.as_array_mut().unwrap() {
                *point = json!([point[1], point[0]]);
            }
            json!({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [r]}, "properties": properties(p)})
        })
        .collect();
    axum::Json(json!({"type": "FeatureCollection", "features": features})).into_response()
}

async fn ogc(State(grid): State<Arc<Grid>>, Query(q): Query<HashMap<String, String>>) -> Response {
    let all = grid.within(bbox_param(&q["bbox"]));
    let offset: usize = q.get("offset").map_or(0, |s| s.parse().unwrap());
    let limit: usize = q["limit"].parse().unwrap();
    let features: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(limit)
        .map(|p| json!({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [ring(p.bounds, false)]}, "properties": properties(p)}))
        .collect();
    let mut links = vec![];
    if offset + limit < all.len() {
        links.push(json!({"rel": "next", "href": format!("/ogc/collections/parcels/items?bbox={}&limit={limit}&offset={}", q["bbox"], offset + limit)}));
    }
    axum::Json(json!({"type": "FeatureCollection", "features": features, "links": links})).into_response()
}

async fn layer() -> Response {
    axum::Json(json!({
        "name": "Tax Parcels", "type": "Feature Layer", "geometryType": "esriGeometryPolygon", "maxRecordCount": 50,
        "advancedQueryCapabilities": {"supportsPagination": true}, "supportedQueryFormats": "JSON, geoJSON",
        "extent": {"spatialReference": {"wkid": 3857}},
        "fields": [{"name": "OBJECTID", "type": "esriFieldTypeOID"}, {"name": "PARCELNO", "type": "esriFieldTypeString", "alias": "Parcel number"},
                   {"name": "OWNER", "type": "esriFieldTypeString"}, {"name": "SITEADDR", "type": "esriFieldTypeString"}],
    }))
    .into_response()
}

/// The zoning districts over a grid: 4×4 parcels each, their codes and names in turn.
pub const DISTRICTS: [(&str, &str); 9] = [
    ("R-1", "Single-Family Residential"),
    ("R-4", "Multi-Family Residential"),
    ("C-2", "General Commercial"),
    ("M-1", "Light Industrial"),
    ("A-1", "Agricultural"),
    ("MU", "Mixed Use"),
    ("OS", "Open Space"),
    ("PUD", "Planned Unit Development"),
    ("R-1", "Single-Family Residential"),
];

impl Grid {
    /// Each district's bounds: the union of its parcels, which the grid holds row by row.
    pub fn districts(&self) -> Vec<[f64; 4]> {
        let mut out = Vec::new();
        for grid in self.parcels.chunks(SIDE * SIDE) {
            for district in 0..9 {
                let (col, row) = ((district % 3) * 4, (district / 3) * 4);
                let first = &grid[row * SIDE + col].bounds;
                let last = &grid[(row + 3) * SIDE + col + 3].bounds;
                out.push([first[0], first[1], last[2], last[3]]);
            }
        }
        out
    }
}

/// The zoning layer: an ArcGIS FeatureServer answering GeoJSON, paged by 4.
async fn zoning(State(grid): State<Arc<Grid>>, Query(q): Query<HashMap<String, String>>) -> Response {
    if q.get("returnExtentOnly").is_some_and(|v| v == "true") {
        return axum::Json(extent(grid.districts().into_iter())).into_response();
    }
    let b = bbox_param(&q["geometry"]);
    let all: Vec<(usize, [f64; 4])> = grid
        .districts()
        .into_iter()
        .enumerate()
        .filter(|(_, d)| d[0] <= b[2] && b[0] <= d[2] && d[1] <= b[3] && b[1] <= d[3])
        .collect();
    let offset: usize = q.get("resultOffset").map_or(0, |s| s.parse().unwrap());
    let count = q.get("resultRecordCount").map_or(4, |s| s.parse::<usize>().unwrap().min(4));
    let page: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(count)
        .map(|(i, d)| {
            let (code, name) = DISTRICTS[i % 9];
            json!({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [ring(*d, false)]},
                   "properties": {"OBJECTID": i + 1, "ZONING": code, "ZONE_DESC": name, "MUNI": "Town of Fake"}})
        })
        .collect();
    let mut body = json!({"type": "FeatureCollection", "features": page});
    if offset + count < all.len() {
        body["properties"] = json!({"exceededTransferLimit": true});
    }
    axum::Json(body).into_response()
}

async fn zoning_named(
    Path(_layer): Path<String>,
    state: State<Arc<Grid>>,
    query: Query<HashMap<String, String>>,
) -> Response {
    zoning(state, query).await
}

/// An ArcGIS answer to a query for the extent of `boxes`, in longitude and latitude.
fn extent(boxes: impl Iterator<Item = [f64; 4]>) -> Value {
    let [mut w, mut s, mut e, mut n] = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for b in boxes {
        (w, s, e, n) = (w.min(b[0]), s.min(b[1]), e.max(b[2]), n.max(b[3]));
    }
    json!({"extent": {"xmin": w, "ymin": s, "xmax": e, "ymax": n, "spatialReference": {"wkid": 4326}}})
}

/// Serves `grid`; returns the server's root URL.
pub async fn serve(grid: Grid) -> String {
    let app = Router::new()
        .route("/arcgis/{variant}/FeatureServer/0/query", get(arcgis))
        .route("/arcgis/{variant}/FeatureServer/0", get(layer))
        .route("/zoning/FeatureServer/0/query", get(zoning))
        // Other jurisdictions' layers over the same ground, each at a path of its own.
        .route("/zoning/{layer}/FeatureServer/0/query", get(zoning_named))
        .route("/wfs", get(wfs))
        .route("/ogc/collections/parcels/items", get(ogc))
        .route("/page", get(page))
        .with_state(Arc::new(grid));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

/// Words no answer may carry: they stand for the bulk of a page, which stays out of agents'
/// context.
pub const PAGE_FILLER: &str = "LONG-PAGE-BODY";

/// A county's GIS page: a link to its parcel service among a long body.
async fn page() -> Response {
    let filler = format!("<p>{PAGE_FILLER} about county offices and meeting minutes.</p>").repeat(2_000);
    axum::response::Html(format!(
        "<html><body><h1>County GIS</h1>{filler}<a href=\"/arcgis/good/FeatureServer/0\">Parcels service</a></body></html>"
    ))
    .into_response()
}
