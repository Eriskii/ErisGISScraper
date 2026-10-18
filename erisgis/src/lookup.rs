//! What a map view's box holds of a dataset: the counties it crosses, the adapters each
//! county's features are read through, and those features as GeoJSON with their normalized
//! fields. A county is read through the adapters verified for it (the first alone, unless the
//! dataset takes one per jurisdiction), or else one whose coverage claims it; a county with
//! neither says why it shows nothing. In a dataset that stacks, features drawn on one outline,
//! such as a building's condominium units, are one feature listing its units.

use crate::counties::{Counties, County};
use crate::dataset::Dataset;
use crate::geom::Bbox;
use crate::registry::{Registry, Standing, now};
use crate::runtime::Runtime;
use crate::spec::{Source, Spec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

/// The most features read from one source for one box.
pub const MAX_FEATURES: usize = 5000;
/// How long a kept answer serves the same box.
const KEEP_FOR_MS: u64 = 7 * 86_400_000;

/// A view's features and where each county it crosses stands.
#[derive(Debug, Default, Serialize)]
pub struct View {
    /// GeoJSON features: the normalized fields, any the dataset derives from them, `key` (the
    /// adapter's id and the feature's own, the same in every box), `adapter` and `publisher`. In
    /// a dataset that stacks, a feature standing for several on one outline also has
    /// `unit_count` and `units` (each unit's normalized fields); its own fields are those every
    /// unit shares, and its key is its smallest unit's.
    pub features: Vec<Value>,
    pub counties: Vec<CountyView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountyView {
    pub fips: String,
    /// Such as `Dane County, WI`.
    pub county: String,
    /// The adapters the county's features are read through; none when it has none.
    pub sources: Vec<SourceView>,
    /// Why the county has no adapter, when the registry records it.
    pub unavailable: Option<UnavailableView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub adapter: String,
    pub publisher: String,
    /// Whether the adapter passed validation for this county, rather than claiming it.
    pub verified: bool,
    /// Whether the layer is the jurisdiction's own (`false`: a copy someone else hosts); absent
    /// when the adapter does not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official: Option<bool>,
    /// Whether the layer's extent meets the box (or the layer does not say where it lies); one
    /// lying elsewhere in the county is not read for it.
    pub here: bool,
    /// Whether the box held more features than were read from it.
    pub more: bool,
    /// Why reading it failed.
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnavailableView {
    pub reason: String,
    pub note: String,
}

/// The counties whose shape meets `bbox`.
pub fn counties_in(counties: &Counties, bbox: Bbox) -> Vec<&County> {
    use geo::Intersects;
    let area = bbox.polygon();
    counties.list.iter().filter(|c| c.bbox.intersects(&bbox) && c.geometry.intersects(&area)).collect()
}

/// The adapters a county's features are read through, each with whether it was verified for
/// the county: those verified (the first alone, unless the dataset takes one per jurisdiction),
/// or else the first whose coverage claims it.
pub fn sources_of<'r>(registry: &'r Registry, dataset: &Dataset, county: &County) -> Vec<(&'r Spec, bool)> {
    let verified: Vec<(&Spec, bool)> = registry
        .index
        .get(&county.fips)
        .into_iter()
        .flatten()
        .filter_map(|id| registry.adapters.get(id))
        .map(|spec| (spec, true))
        .take(if dataset.per_jurisdiction { usize::MAX } else { 1 })
        .collect();
    if !verified.is_empty() {
        return verified;
    }
    registry
        .adapters
        .values()
        .find(|spec| spec.coverage.claims(&county.fips, |_| Some(county.state.clone())))
        .map(|spec| (spec, false))
        .into_iter()
        .collect()
}

/// What one source gave for the box.
#[derive(Default, Serialize, Deserialize)]
struct Read {
    at: u64,
    features: Vec<Value>,
    more: bool,
}

/// Reads the features of every county `bbox` crosses from `registry`, the registry of
/// `dataset`. Answers are kept in `keep` (by adapter and box) and serve the same box for a week.
pub async fn read(
    runtime: &Runtime,
    registry: &Registry,
    dataset: &Dataset,
    counties: &Counties,
    bbox: Bbox,
    keep: Option<&Path>,
) -> View {
    let crossed = counties_in(counties, bbox);
    let mut sources: BTreeMap<String, &Spec> = BTreeMap::new();
    let mut view = View::default();
    for county in &crossed {
        let found = sources_of(registry, dataset, county);
        let unavailable = match (found.is_empty(), registry.standing(&county.fips, now())) {
            (true, Standing::Unavailable(record)) => {
                Some(UnavailableView { reason: record.reason.name().to_owned(), note: record.note })
            }
            _ => None,
        };
        for (spec, _) in &found {
            sources.insert(spec.id.clone(), spec);
        }
        view.counties.push(CountyView {
            fips: county.fips.clone(),
            county: format!("{}, {}", county.name, county.state),
            sources: found
                .into_iter()
                .map(|(spec, verified)| SourceView {
                    adapter: spec.id.clone(),
                    publisher: spec.publisher.clone(),
                    verified,
                    official: spec.official,
                    here: meets(spec, bbox),
                    more: false,
                    error: None,
                })
                .collect(),
            unavailable,
        });
    }
    let reads =
        futures_util::future::join_all(sources.values().map(|spec| read_source(runtime, dataset, spec, bbox, keep)))
            .await;
    let mut seen = HashSet::new();
    for (spec, read) in sources.values().zip(reads) {
        for source in view.counties.iter_mut().flat_map(|c| c.sources.iter_mut()).filter(|s| s.adapter == spec.id) {
            match &read {
                Ok(read) => source.more = read.more,
                Err(error) => source.error = Some(error.clone()),
            }
        }
        let mut features = read.map(|r| r.features).unwrap_or_default();
        // Derived after the kept answer, so a kept answer gets the dataset's current rules.
        if let Some(derive) = dataset.derive {
            for feature in &mut features {
                if let Some(properties) = feature["properties"].as_object_mut() {
                    derive(properties);
                }
            }
        }
        let features = if dataset.stacks { stack(dataset, features) } else { features };
        for feature in features {
            if seen.insert(feature["properties"]["key"].as_str().unwrap_or_default().to_owned()) {
                view.features.push(feature);
            }
        }
    }
    view
}

/// Whether the spec's layer may hold features in `bbox`: its extent, with a little room for its
/// edges, meets the box, or it does not say where it lies.
fn meets(spec: &Spec, bbox: Bbox) -> bool {
    spec.extent.is_none_or(|e| {
        Bbox([e.west() - 0.001, e.south() - 0.001, e.east() + 0.001, e.north() + 0.001]).intersects(&bbox)
    })
}

async fn read_source(
    runtime: &Runtime,
    dataset: &Dataset,
    spec: &Spec,
    bbox: Bbox,
    keep: Option<&Path>,
) -> Result<Read, String> {
    if matches!(spec.source, Source::BulkDownload { .. }) {
        return Err("this source is a bulk download, which the map does not fetch".into());
    }
    // A layer lying elsewhere in the county holds nothing here.
    if !meets(spec, bbox) {
        return Ok(Read::default());
    }
    let kept = keep.map(|dir| dir.join(format!("{}-{}-{:016x}.json", dataset.name, spec.id, box_hash(bbox))));
    if let Some(path) = &kept
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(read) = serde_json::from_slice::<Read>(&bytes)
        && now().saturating_sub(read.at) < KEEP_FOR_MS
    {
        return Ok(read);
    }
    let fetch = runtime.fetch(spec, bbox, MAX_FEATURES, None).await.map_err(|e| format!("{e:#}"))?;
    let features = fetch.features.iter().map(|f| feature(dataset, spec, f)).collect();
    let read = Read { at: now(), features, more: fetch.more };
    if let Some(path) = kept
        && let Ok(text) = serde_json::to_vec(&read)
    {
        let _ = crate::registry::write_atomic(&path, &text);
    }
    Ok(read)
}

/// A feature as a map feature: its normalized fields, its source, and a key that is the same whichever box it was read in: the adapter's id and the
/// feature's own name in the dataset's key field, or a hash of its attributes and outline.
pub fn feature(dataset: &Dataset, spec: &Spec, read: &crate::runtime::Feature) -> Value {
    let mut properties = serde_json::Map::new();
    for (field, value) in read.normalized(spec) {
        properties.insert(field, value);
    }
    let geometry = crate::geom::to_geojson(&read.geometry, 7);
    let own = match dataset.key.and_then(|key| properties.get(key)) {
        Some(Value::String(name)) => name.clone(),
        Some(other) => other.to_string(),
        None => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            Value::Object(read.attributes.clone()).to_string().hash(&mut hasher);
            if dataset.key.is_none() {
                outline(&geometry).hash(&mut hasher);
            }
            format!("{:016x}", hasher.finish())
        }
    };
    properties.insert("key".into(), json!(format!("{}:{own}", spec.id)));
    properties.insert("adapter".into(), json!(spec.id));
    properties.insert("publisher".into(), json!(spec.publisher));
    if let Some(official) = spec.official {
        properties.insert("official".into(), json!(official));
    }
    json!({"type": "Feature", "geometry": geometry, "properties": properties})
}

/// An outline as a comparable string: coordinates to about 10 cm, each ring from its smallest
/// point and in one direction, the rings in order.
fn outline(geometry: &Value) -> String {
    let mut rings: Vec<Vec<(i64, i64)>> = Vec::new();
    for polygon in geometry["coordinates"].as_array().into_iter().flatten() {
        for ring in polygon.as_array().into_iter().flatten() {
            let mut points: Vec<(i64, i64)> = ring
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| Some(((p[0].as_f64()? * 1e6).round() as i64, (p[1].as_f64()? * 1e6).round() as i64)))
                .collect();
            if points.len() > 1 && points.first() == points.last() {
                points.pop();
            }
            if let Some(start) = points.iter().enumerate().min_by_key(|(_, p)| **p).map(|(i, _)| i) {
                points.rotate_left(start);
            }
            if points.len() > 2 && points[1] > points[points.len() - 1] {
                points[1..].reverse();
            }
            rings.push(points);
        }
    }
    rings.sort();
    format!("{rings:?}")
}

/// One source's features with those on each outline as one feature (see [`View`]).
fn stack(dataset: &Dataset, features: Vec<Value>) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for feature in features {
        let shape = outline(&feature["geometry"]);
        if !groups.contains_key(&shape) {
            order.push(shape.clone());
        }
        groups.entry(shape).or_default().push(feature);
    }
    order
        .into_iter()
        .filter_map(|shape| groups.remove(&shape))
        .map(|mut members| {
            members.sort_by(|a, b| a["properties"]["key"].as_str().cmp(&b["properties"]["key"].as_str()));
            let mut units: Vec<serde_json::Map<String, Value>> = Vec::new();
            for member in &members {
                let unit: serde_json::Map<String, Value> = dataset
                    .fields
                    .iter()
                    .filter_map(|f| member["properties"].get(f.name).map(|v| (f.name.to_string(), v.clone())))
                    .collect();
                if !units.contains(&unit) {
                    units.push(unit);
                }
            }
            let mut feature = members.swap_remove(0);
            if units.len() > 1 {
                let properties = feature["properties"].as_object_mut().expect("feature properties");
                for field in dataset.fields {
                    if units.iter().any(|u| u.get(field.name) != units[0].get(field.name)) {
                        properties.remove(field.name);
                    }
                }
                properties.insert("unit_count".into(), json!(units.len()));
                properties.insert("units".into(), json!(units));
            }
            feature
        })
        .collect()
}

fn box_hash(bbox: Bbox) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for edge in bbox.0 {
        ((edge * 1e6).round() as i64).hash(&mut hasher);
    }
    hasher.finish()
}
