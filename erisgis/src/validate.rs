//! Proves a spec works for a county by running it: enough features near a point inside the
//! county, polygons in longitude/latitude that fall where the county is, mapped fields that
//! exist and are filled in, and paging that returns every feature. How many features a sample
//! needs, and how far it looks, are the dataset's.

use crate::counties::County;
use crate::dataset::Dataset;
use crate::geom::Bbox;
use crate::geom::Shape;
use crate::runtime::{Fetch, Runtime};
use crate::spec::{Source, Spec};
use geo::Centroid;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Most features read for one sample.
const MAX_FEATURES: usize = 2000;
/// Features asked for across the whole county when its interior point has none nearby, to find
/// one to sample around.
const FIND_FEATURES: usize = 100;
/// Share of polygons that must be valid.
const VALID_SHARE: f64 = 0.9;
/// Share of features that must lie near the sample box, and of centroids inside the county.
const NEAR_SHARE: f64 = 0.9;
const INSIDE_SHARE: f64 = 0.5;
/// Share of features each mapped field must be filled in for.
const FILL_SHARE: f64 = 0.5;

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub spec: String,
    pub fips: String,
    pub county: String,
    pub passed: bool,
    pub checks: Vec<Check>,
    /// What the features are, such as `parcels`, for the summary.
    #[serde(skip)]
    many: &'static str,
    pub sample_bbox: Option<Bbox>,
    pub features: usize,
    /// Features whose polygons were invalid as the source has them and were repaired.
    pub repaired: usize,
    pub pages: usize,
    /// Share of features with each mapped field filled in.
    pub fill: BTreeMap<String, f64>,
    /// Attribute names the source has, to map fields from.
    pub attribute_names: Vec<String>,
    /// The attributes of the first features, shortened.
    pub sample_attributes: Vec<Map<String, Value>>,
    pub first_url: String,
}

impl Report {
    fn new(dataset: &Dataset, spec: &Spec, county: &County) -> Self {
        Self {
            many: dataset.many,
            spec: spec.id.clone(),
            fips: county.fips.clone(),
            county: county.label(),
            passed: false,
            checks: Vec::new(),
            sample_bbox: None,
            features: 0,
            repaired: 0,
            pages: 0,
            fill: BTreeMap::new(),
            attribute_names: Vec::new(),
            sample_attributes: Vec::new(),
            first_url: String::new(),
        }
    }

    fn check(&mut self, name: &'static str, ok: bool, detail: impl Into<String>) -> bool {
        self.checks.push(Check { name, ok, detail: detail.into() });
        ok
    }

    /// One line per check, for logs.
    pub fn summary(&self) -> String {
        let failed: Vec<String> =
            self.checks.iter().filter(|c| !c.ok).map(|c| format!("{}: {}", c.name, c.detail)).collect();
        if self.passed {
            format!("{} passes for {} ({} {})", self.spec, self.county, self.features, self.many)
        } else {
            format!("{} fails for {}: {}", self.spec, self.county, failed.join("; "))
        }
    }
}

fn share(count: usize, of: usize) -> f64 {
    if of == 0 { 0.0 } else { count as f64 / of as f64 }
}

/// Validates `spec`, a spec of `dataset`, for `county`, reporting every check; the `time`
/// check fails when that takes longer than the runtime allows.
pub async fn validate(runtime: &Runtime, dataset: &Dataset, spec: &Spec, county: &County) -> Report {
    match tokio::time::timeout(runtime.validate_within, checks(runtime, dataset, spec, county)).await {
        Ok(report) => report,
        Err(_) => {
            let mut report = Report::new(dataset, spec, county);
            report.check(
                "time",
                false,
                format!(
                    "validation took longer than {} s: the source answers too slowly to read {} from",
                    runtime.validate_within.as_secs(),
                    dataset.many
                ),
            );
            report.passed = false;
            report
        }
    }
}

/// The smallest of the dataset's sample boxes around `x`,`y` that holds enough features, or the
/// largest when none does.
async fn sample_around(
    runtime: &Runtime,
    dataset: &Dataset,
    spec: &Spec,
    x: f64,
    y: f64,
) -> anyhow::Result<(Bbox, Fetch)> {
    let mut sample = None;
    for &size in dataset.sample_sizes {
        let bbox = Bbox::around(x, y, size);
        let fetch = runtime.fetch(spec, bbox, MAX_FEATURES, None).await?;
        let enough = fetch.features.len() >= dataset.min_features;
        sample = Some((bbox, fetch));
        if enough {
            break;
        }
    }
    Ok(sample.expect("at least one sample size"))
}

async fn checks(runtime: &Runtime, dataset: &Dataset, spec: &Spec, county: &County) -> Report {
    let mut report = Report::new(dataset, spec, county);
    let (many, min) = (dataset.many, dataset.min_features);
    if let Err(error) = spec.check(dataset) {
        report.check("spec", false, format!("{error:#}"));
        return report;
    }
    if !spec.coverage.claims(&county.fips, |_| Some(county.state.clone())) {
        report.check("coverage", false, format!("the spec's coverage does not include {}", county.fips));
        return report;
    }

    let [mut x, mut y] = county.interior;
    let (mut bbox, mut fetch) = match sample_around(runtime, dataset, spec, x, y).await {
        Ok(sample) => sample,
        Err(error) => {
            report.check("query", false, format!("{error:#}"));
            return report;
        }
    };
    report.check("query", true, format!("{} pages", fetch.pages));
    // A large, sparse county's interior point can lie far from any feature: sample around one
    // of the layer's own features inside the county instead.
    if fetch.features.len() < min {
        let found = match runtime.fetch(spec, county.bbox, FIND_FEATURES, Some(FIND_FEATURES as u32)).await {
            Ok(found) => found,
            Err(error) => {
                report.check("query", false, format!("looking for {many} across the county: {error:#}"));
                return report;
            }
        };
        let inside =
            found.features.iter().filter_map(|f| f.geometry.centroid()).find(|c| county.contains(c.x(), c.y()));
        if let Some(found) = inside {
            match sample_around(runtime, dataset, spec, found.x(), found.y()).await {
                Ok(sample) => {
                    report.check(
                        "sample",
                        true,
                        format!(
                            "no {many} near the county's interior point {x},{y}; sampled around one at {},{}",
                            found.x(),
                            found.y()
                        ),
                    );
                    (x, y) = (found.x(), found.y());
                    (bbox, fetch) = sample;
                }
                Err(error) => {
                    report.check("query", false, format!("{error:#}"));
                    return report;
                }
            }
        }
    }
    report.sample_bbox = Some(bbox);
    report.features = fetch.features.len();
    report.pages = fetch.pages;
    report.first_url = fetch.first_url.clone();
    report.attribute_names = fetch.attribute_names.clone();
    report.sample_attributes = fetch
        .features
        .iter()
        .take(2)
        .map(|p| {
            p.attributes
                .iter()
                .take(40)
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) if s.len() > 80 => {
                            Value::String(format!("{}…", &s[..s.floor_char_boundary(80)]))
                        }
                        other => other.clone(),
                    };
                    (k.clone(), v)
                })
                .collect()
        })
        .collect();
    let n = fetch.features.len();
    if !report.check(
        "features",
        n >= min,
        format!("{n} {many} within {:.2}° of {x},{y} (at least {min} needed)", bbox.east() - bbox.west()),
    ) {
        return report;
    }

    let lon_lat = fetch.features.iter().filter(|f| Bbox::of(&f.geometry).is_some_and(|b| b.is_lon_lat())).count();
    report.check(
        "longitude-latitude",
        lon_lat == n,
        format!("{lon_lat} of {n} {many} have longitude/latitude coordinates; the others are projected"),
    );
    let invalid = fetch.features.iter().filter(|f| f.geometry.0.is_empty() || f.shape == Shape::Invalid).count();
    let valid = n - invalid;
    report.repaired = fetch.features.iter().filter(|f| f.shape == Shape::Repaired).count();
    report.check(
        "valid-polygons",
        share(valid, n) >= VALID_SHARE,
        format!(
            "{valid} of {n} polygons are valid, {} of them repaired; the other {invalid} are past repair ({:.0}% needed)",
            report.repaired,
            VALID_SHARE * 100.0
        ),
    );
    let centroids: Vec<_> = fetch.features.iter().filter_map(|f| f.geometry.centroid()).collect();
    // Near the box: a feature's extent meets the box grown a little. A large district's centroid
    // can lie far off while the district itself covers the box.
    let near_box = bbox.grown(0.02);
    let near = fetch.features.iter().filter(|f| Bbox::of(&f.geometry).is_some_and(|b| b.intersects(&near_box))).count();
    let inside = centroids.iter().filter(|c| county.contains(c.x(), c.y())).count();
    let first = centroids.first().map(|c| format!(" (the first is at {:.5},{:.5})", c.x(), c.y())).unwrap_or_default();
    report.check(
        "location",
        share(near, n) >= NEAR_SHARE,
        format!(
            "{near} of {n} {many} lie near the box asked for{first}; swapped or projected coordinates land elsewhere"
        ),
    );
    report.check(
        "in-county",
        share(inside, n) >= INSIDE_SHARE,
        format!("{inside} of {n} {many}' centroids lie inside {}", county.name),
    );

    let mapped = &spec.fields;
    report.check(
        "fields-mapped",
        dataset.identifying.iter().any(|f| mapped.contains_key(*f)),
        format!(
            "mapped: {:?}; at least one of {} is needed",
            mapped.keys().collect::<Vec<_>>(),
            dataset.identifying.join(", ")
        ),
    );
    for (field, source) in mapped {
        let exists = fetch.features.iter().any(|f| f.attribute(source).is_some());
        let filled = fetch.features.iter().filter(|f| f.normalized(spec).contains_key(field)).count();
        report.fill.insert(field.to_string(), (share(filled, n) * 1000.0).round() / 1000.0);
        let detail = if exists {
            format!("{field} ← {source}: filled for {filled} of {n} {many}")
        } else {
            format!("{field} ← {source}: the source has no attribute {source}")
        };
        report.check("field-fill", exists && share(filled, n) >= FILL_SHARE, detail);
    }

    if !matches!(spec.source, Source::BulkDownload { .. }) {
        if fetch.pages > 1 {
            report.check("paging", true, format!("{} pages read", fetch.pages));
        } else {
            // The sample came in one page: read the same box again in two halves. A larger box
            // costs some servers far more than smaller pages do.
            let page = n.div_ceil(2);
            match runtime.fetch(spec, bbox, n + page, Some(page as u32)).await {
                Ok(paged) => {
                    let distinct: std::collections::HashSet<String> =
                        paged.features.iter().map(|f| Value::Object(f.attributes.clone()).to_string()).collect();
                    let read = paged.features.len();
                    report.check(
                        "paging",
                        paged.pages > 1 && distinct.len() + n / 50 >= n && read <= n + n / 50,
                        format!(
                            "pages of {page}: {read} {many} ({} distinct) in {} pages, against {n} in one",
                            distinct.len(),
                            paged.pages
                        ),
                    );
                }
                Err(error) => {
                    report.check("paging", false, format!("pages of {page} failed: {error:#}"));
                }
            }
        }
    }
    report.passed = report.checks.iter().all(|c| c.ok);
    report
}
