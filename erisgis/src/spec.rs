//! Adapter specs: where a county publishes a dataset's layer and how its attributes map to the
//! dataset's normalized fields. A spec holds pointers, never features. `Unavailable` records why
//! a county has no adapter, and when to look again.

use crate::dataset::Dataset;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SPEC_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub version: u32,
    /// Lowercase letters, digits and dashes, such as `wi-statewide-v11` or `ga-fulton`.
    pub id: String,
    /// Who publishes the layer, such as `Wisconsin State Cartographer's Office`.
    pub publisher: String,
    /// Whether the layer is the jurisdiction's own: published by the county or city, its state,
    /// or a vendor serving it for them (`true`), or a copy someone else hosts, such as a personal
    /// ArcGIS Online item (`false`, with why in `notes`). Specs that do not say read as unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official: Option<bool>,
    pub source: Source,
    pub coverage: Coverage,
    /// Source attribute names by normalized field, the dataset's fields only. Every field is
    /// optional; geometry always comes from the source's geometry.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// A static filter, such as an ArcGIS `where` clause or a CQL filter; none by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Free text: licensing, terms of use, update frequency, quirks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Where the layer's features lie, in longitude and latitude, as its source says when it is
    /// saved; a box this misses is not read from it. Unknown for sources that do not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent: Option<crate::geom::Bbox>,
    /// Filled in by the validator: when the spec last passed, for which counties, and which
    /// rung of the model ladder wrote it.
    #[serde(default)]
    pub verification: Verification,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    /// Unix milliseconds of the last passing validation.
    #[serde(default)]
    pub verified_at: u64,
    /// The counties it passed for.
    #[serde(default)]
    pub counties: Vec<String>,
    /// The model that wrote the spec, and its rung on the ladder (0 is the cheapest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rung: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Source {
    /// An ArcGIS REST FeatureServer or MapServer layer: `.../FeatureServer/0`.
    ArcgisFeature {
        url: String,
        /// The layer's maxRecordCount; pages are this size at most.
        #[serde(default = "default_page")]
        page_size: u32,
        /// Whether the server answers `f=geojson`; Esri JSON is read otherwise.
        #[serde(default = "yes")]
        geojson: bool,
    },
    /// An OGC Web Feature Service.
    Wfs {
        /// The service endpoint, without request parameters.
        url: String,
        type_name: String,
        #[serde(default = "wfs_version")]
        version: String,
        #[serde(default = "default_page")]
        page_size: u32,
        /// Whether the service wants EPSG:4326 boxes as latitude,longitude, as WFS 2.0 servers
        /// usually do.
        #[serde(default)]
        lat_lon: bool,
    },
    /// An OGC API Features collection: `.../collections/<id>`.
    OgcFeatures {
        url: String,
        #[serde(default = "default_page")]
        page_size: u32,
    },
    /// A file to download whole: a GeoJSON file, or a zipped shapefile.
    BulkDownload {
        url: String,
        format: BulkFormat,
        /// The file's coordinate system. Only longitude/latitude (4326, 4269) and Web Mercator
        /// (3857) are read.
        #[serde(default = "wgs84")]
        epsg: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BulkFormat {
    Geojson,
    ShapefileZip,
}

fn default_page() -> u32 {
    1000
}
fn yes() -> bool {
    true
}
fn wfs_version() -> String {
    "2.0.0".into()
}
fn wgs84() -> u32 {
    4326
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Five-digit county FIPS codes.
    Counties(Vec<String>),
    /// Every county of these states, by postal abbreviation.
    States(Vec<String>),
}

impl Coverage {
    pub fn claims(&self, fips: &str, state_of: impl Fn(&str) -> Option<String>) -> bool {
        match self {
            Coverage::Counties(list) => list.iter().any(|f| f == fips),
            Coverage::States(states) => state_of(fips).is_some_and(|s| states.contains(&s)),
        }
    }
}

impl Spec {
    /// Checks what can be checked without the network, as a spec of `dataset`.
    pub fn check(&self, dataset: &Dataset) -> Result<()> {
        ensure!(self.version == SPEC_VERSION, "spec version must be {SPEC_VERSION}");
        ensure!(
            !self.id.is_empty()
                && self.id.len() <= 64
                && self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "id must be 1-64 lowercase letters, digits and dashes"
        );
        ensure!(!self.publisher.trim().is_empty(), "publisher is required");
        for (field, source) in &self.fields {
            if dataset.field(field).is_none() {
                let names: Vec<&str> = dataset.fields.iter().map(|f| f.name).collect();
                bail!("{field} is not a field of {}; they are {}", dataset.name, names.join(", "));
            }
            ensure!(!source.trim().is_empty(), "the source attribute for {field} is empty");
        }
        let url = match &self.source {
            Source::ArcgisFeature { url, .. }
            | Source::Wfs { url, .. }
            | Source::OgcFeatures { url, .. }
            | Source::BulkDownload { url, .. } => url,
        };
        let parsed = url::Url::parse(url)?;
        ensure!(matches!(parsed.scheme(), "http" | "https"), "the source URL must be http or https");
        if let Source::BulkDownload { epsg, .. } = &self.source {
            ensure!(matches!(epsg, 4326 | 4269 | 3857), "bulk downloads must be in EPSG 4326, 4269 or 3857");
        }
        match &self.coverage {
            Coverage::Counties(list) => {
                ensure!(!list.is_empty(), "coverage lists no counties");
                for fips in list {
                    ensure!(fips.len() == 5 && fips.chars().all(|c| c.is_ascii_digit()), "{fips} is not a county FIPS");
                }
            }
            Coverage::States(states) => {
                ensure!(!states.is_empty(), "coverage lists no states");
                for state in states {
                    if state.len() != 2 || !state.chars().all(|c| c.is_ascii_uppercase()) {
                        bail!("{state} is not a postal abbreviation");
                    }
                }
            }
        }
        Ok(())
    }

    pub fn host(&self) -> Option<String> {
        let url = match &self.source {
            Source::ArcgisFeature { url, .. }
            | Source::Wfs { url, .. }
            | Source::OgcFeatures { url, .. }
            | Source::BulkDownload { url, .. } => url,
        };
        url::Url::parse(url).ok()?.host_str().map(str::to_owned)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// The county publishes no GIS parcels at all.
    NoPublicGis,
    /// Parcels are only shown in a web viewer with no queryable service behind it.
    ViewerOnly,
    /// Parcel data is sold.
    Paywalled,
    /// The service requires an account or a key.
    LoginRequired,
    /// The terms of use forbid automated access.
    TosForbidsAutomation,
    /// There is nothing to publish: the county and the places in it have none of the dataset,
    /// as a county that does not zone has no zoning.
    NoneExists,
}

impl Reason {
    pub const ALL: [Reason; 6] = [
        Reason::NoPublicGis,
        Reason::ViewerOnly,
        Reason::Paywalled,
        Reason::LoginRequired,
        Reason::TosForbidsAutomation,
        Reason::NoneExists,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Reason::NoPublicGis => "no-public-gis",
            Reason::ViewerOnly => "viewer-only",
            Reason::Paywalled => "paywalled",
            Reason::LoginRequired => "login-required",
            Reason::TosForbidsAutomation => "tos-forbids-automation",
            Reason::NoneExists => "none-exists",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == text)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Unavailable {
    pub version: u32,
    pub fips: String,
    pub reason: Reason,
    /// Links that show it: the viewer, the price list, the terms, the login page.
    pub evidence: Vec<String>,
    /// What was found.
    pub note: String,
    /// Unix milliseconds.
    pub recorded_at: u64,
    /// Unix milliseconds after which the county is searched again.
    pub recheck_after: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parcels::PARCELS;
    use crate::zoning::ZONING;
    use serde_json::json;

    #[test]
    fn specs_read_from_json_with_defaults() {
        let spec: Spec = serde_json::from_value(json!({
            "version": 1, "id": "wi-statewide", "publisher": "Wisconsin SCO",
            "source": {"kind": "arcgis-feature", "url": "https://services3.arcgis.com/x/FeatureServer/0"},
            "coverage": {"states": ["WI"]},
            "fields": {"apn": "PARCELID", "owner": "OWNERNME1"},
        }))
        .unwrap();
        spec.check(&PARCELS).unwrap();
        assert_eq!(
            spec.source,
            Source::ArcgisFeature {
                url: "https://services3.arcgis.com/x/FeatureServer/0".into(),
                page_size: 1000,
                geojson: true
            }
        );
        assert_eq!(
            spec.fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>(),
            [("apn", "PARCELID"), ("owner", "OWNERNME1")]
        );
        assert_eq!(spec.host().as_deref(), Some("services3.arcgis.com"));
        assert!(spec.coverage.claims("55025", |_| Some("WI".into())));
        assert_eq!(spec.official, None, "a spec that does not say is neither official nor a copy");
        let said = |official: bool| {
            let mut value = serde_json::to_value(&spec).unwrap();
            value["official"] = json!(official);
            serde_json::from_value::<Spec>(value).unwrap().official
        };
        assert_eq!((said(true), said(false)), (Some(true), Some(false)));
    }

    #[test]
    fn bad_specs_are_refused() {
        let base = json!({
            "version": 1, "id": "ok", "publisher": "p",
            "source": {"kind": "ogc-features", "url": "https://example.org/collections/parcels"},
            "coverage": {"counties": ["13121"]}, "fields": {},
        });
        let with = |path: &str, value: serde_json::Value| {
            let mut v = base.clone();
            *v.pointer_mut(path).unwrap() = value;
            serde_json::from_value::<Spec>(v).unwrap().check(&PARCELS)
        };
        assert!(with("/id", json!("ok")).is_ok());
        assert!(with("/id", json!("Has Spaces")).is_err());
        assert!(with("/source/url", json!("file:///etc/passwd")).is_err());
        assert!(with("/coverage/counties", json!(["131"])).is_err());
        let bulk =
            json!({"kind": "bulk-download", "url": "https://x.org/p.zip", "format": "shapefile-zip", "epsg": 2285});
        assert!(with("/source", bulk).is_err());
    }

    #[test]
    fn a_spec_maps_its_own_datasets_fields_only() {
        let spec = |fields: serde_json::Value| -> Spec {
            serde_json::from_value(json!({
                "version": 1, "id": "ok", "publisher": "p",
                "source": {"kind": "ogc-features", "url": "https://example.org/collections/zoning"},
                "coverage": {"counties": ["13121"]}, "fields": fields,
            }))
            .unwrap()
        };
        assert!(spec(json!({"zone": "ZONE_CODE"})).check(&ZONING).is_ok());
        let error = spec(json!({"zone": "ZONE_CODE"})).check(&PARCELS).unwrap_err().to_string();
        assert!(error.contains("zone is not a field of parcels"), "{error}");
        assert!(spec(json!({"apn": "PIN"})).check(&ZONING).is_err());
        assert!(spec(json!({"zone": " "})).check(&ZONING).is_err());
    }
}
