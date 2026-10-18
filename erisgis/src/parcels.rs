//! Tax parcels: each county's property lines, with the assessor's number, owner, address and
//! values. One adapter covers a county; a building's condominium parcels, drawn on one outline,
//! are one map feature listing its units.

use crate::dataset::{Dataset, Field, Instructions};
use crate::spec::Reason;

pub const PARCELS: Dataset = Dataset {
    name: "parcels",
    one: "parcel",
    many: "parcels",
    fields: &[
        Field { name: "apn", about: "the assessor's parcel number" },
        Field { name: "owner", about: "the owner's name" },
        Field { name: "address", about: "the site address" },
        Field { name: "acres", about: "the area in acres" },
        Field { name: "land_use", about: "the land use or property class" },
        Field { name: "assessed_value", about: "the assessed value" },
    ],
    identifying: &["apn", "address", "owner"],
    key: Some("apn"),
    min_features: 5,
    sample_sizes: &[0.01, 0.04, 0.12],
    stacks: true,
    per_jurisdiction: false,
    reasons: &[
        Reason::NoPublicGis,
        Reason::ViewerOnly,
        Reason::Paywalled,
        Reason::LoginRequired,
        Reason::TosForbidsAutomation,
    ],
    derive: None,
    agents: Instructions {
        county_goal: "You find where one US county publishes its tax parcel polygons, then write and validate an \
            adapter spec that reads them. Your job ends when `spec submit` saves a passing spec, or when \
            `spec unavailable` records that the county has none.",
        state_goal: "You look for ONE public layer of tax parcel polygons that covers a whole US state (a statewide \
            parcel layer from a state GIS office, often aggregated from counties). When you find one, \
            validate a spec for it with coverage {\"states\": [\"XX\"]} using `statewide_spec validate`, and \
            save it with `statewide_spec submit`. A statewide layer with a county field covers every county \
            without a filter. If you find no statewide layer after a real search, stop and say so in one or \
            two sentences, naming any per-county portals you came across.",
        how: "- Counties and states mostly publish parcels as ArcGIS REST services (…/arcgis/rest/services/…/FeatureServer/N
  or MapServer/N), often behind an ArcGIS Hub open-data site. Start with `arcgis search parcels`, then
  `web search <county> county <state> parcels gis`.
- Pick the polygon layer of tax parcels: not lot lines, points or subdivisions.",
        example_fields: r#"{"apn": "PARCELID", "owner": "OWNERNME1", "address": "SITEADRESS",
             "acres": "GISACRES", "land_use": "PROPCLASS", "assessed_value": "CNTASSDVALUE"}"#,
        search: "parcels",
        saved: "Your job is done: reply with one line saying what you saved.",
        follow_up: "",
        jurisdictions_goal: "",
    },
};
