//! The validator against a fake county server: specs that work pass, and each way a spec is
//! commonly wrong fails the check that names it.

use erisgis::testing as support;

use erisgis::counties::{Counties, County};
use erisgis::http::{Http, Limits};
use erisgis::parcels::PARCELS;
use erisgis::runtime::{Runtime, VALIDATE_WITHIN};
use erisgis::spec::Spec;
use erisgis::validate::{Report, validate};
use serde_json::{Value, json};
use std::sync::OnceLock;

fn counties() -> &'static Counties {
    static COUNTIES: OnceLock<Counties> = OnceLock::new();
    COUNTIES.get_or_init(|| Counties::embedded().unwrap())
}

fn dane() -> &'static County {
    counties().get("55025").unwrap()
}

async fn server() -> String {
    support::serve(support::Grid::around(dane().interior[0], dane().interior[1])).await
}

fn runtime(cache: &std::path::Path) -> Runtime {
    Runtime {
        http: Http::new(Limits { allow_private: true, ..Limits::default() }).unwrap(),
        cache: cache.to_owned(),
        validate_within: VALIDATE_WITHIN,
    }
}

fn spec(source: Value, fields: Value) -> Spec {
    serde_json::from_value(json!({
        "version": 1, "id": "test", "publisher": "Fake County",
        "source": source, "coverage": {"counties": ["55025"]}, "fields": fields,
    }))
    .unwrap()
}

fn good_fields() -> Value {
    json!({"apn": "PARCELNO", "owner": "OWNER", "address": "SITEADDR", "acres": "GISACRES"})
}

async fn run(spec: &Spec) -> Report {
    let cache = tempfile::tempdir().unwrap();
    validate(&runtime(cache.path()), &PARCELS, spec, dane()).await
}

fn failed(report: &Report) -> Vec<&str> {
    report.checks.iter().filter(|c| !c.ok).map(|c| c.name).collect()
}

#[tokio::test]
async fn an_arcgis_geojson_layer_passes_and_pages() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/good/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert!(report.passed, "{}", report.summary());
    assert!(
        report.features >= 5 && report.pages > 1,
        "the 50-record limit forces pages: {} in {}",
        report.features,
        report.pages
    );
    assert_eq!(report.fill["owner"], 0.75, "a quarter of owners are empty");
    assert!(report.attribute_names.contains(&"PARCELNO".to_owned()));
}

#[tokio::test]
async fn an_arcgis_esri_json_layer_passes() {
    let root = server().await;
    let source =
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/good/FeatureServer/0"), "geojson": false});
    let report = run(&spec(source, good_fields())).await;
    assert!(report.passed, "{}", report.summary());
}

#[tokio::test]
async fn projected_coordinates_fail() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/mercator/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert!(
        failed(&report).contains(&"longitude-latitude") && failed(&report).contains(&"location"),
        "{}",
        report.summary()
    );
}

#[tokio::test]
async fn a_field_the_source_lacks_fails_and_the_report_lists_the_real_ones() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/good/FeatureServer/0")}),
        json!({"apn": "PIN"}),
    ))
    .await;
    assert_eq!(failed(&report), ["field-fill"], "{}", report.summary());
    assert!(report.summary().contains("no attribute PIN"));
    assert!(report.sample_attributes[0].contains_key("SITEADDR"));
}

#[tokio::test]
async fn a_server_that_ignores_offsets_fails_paging() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/ignores-offset/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert_eq!(failed(&report), ["query"]);
    assert!(report.summary().contains("ignores resultOffset"), "{}", report.summary());
}

#[tokio::test]
async fn an_arcgis_error_fails_the_query_with_its_message() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/refuses/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert_eq!(failed(&report), ["query"]);
    assert!(report.summary().contains("Invalid query parameters"));
}

#[tokio::test]
async fn a_latitude_first_wfs_passes_only_when_the_spec_says_so() {
    let root = server().await;
    let wfs = |lat_lon: bool| json!({"kind": "wfs", "url": format!("{root}/wfs"), "type_name": "county:parcels", "page_size": 40, "lat_lon": lat_lon});
    let report = run(&spec(wfs(true), good_fields())).await;
    assert!(report.passed, "{}", report.summary());
    let report = run(&spec(wfs(false), good_fields())).await;
    assert!(!report.passed);
}

#[tokio::test]
async fn an_ogc_features_collection_follows_next_links() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "ogc-features", "url": format!("{root}/ogc/collections/parcels"), "page_size": 30}),
        good_fields(),
    ))
    .await;
    assert!(report.passed, "{}", report.summary());
    assert!(report.pages > 1);
}

#[tokio::test]
async fn a_county_outside_the_coverage_fails() {
    let root = server().await;
    let mut spec =
        spec(json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/good/FeatureServer/0")}), good_fields());
    spec.coverage = erisgis::spec::Coverage::States(vec!["MN".into()]);
    assert_eq!(failed(&run(&spec).await), ["coverage"]);
}

#[tokio::test]
async fn a_sample_that_fits_one_page_is_paged_again_in_halves_of_the_same_box() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/whole-pages/FeatureServer/0"),
            "page_size": 2000}),
        good_fields(),
    ))
    .await;
    assert!(report.passed, "{}", report.summary());
    let paging = report.checks.iter().find(|c| c.name == "paging").unwrap();
    let half = report.features.div_ceil(2);
    assert!(paging.detail.starts_with(&format!("pages of {half}:")), "{}", paging.detail);
}

#[tokio::test]
async fn validation_that_runs_too_long_fails_on_time() {
    let root = server().await;
    let cache = tempfile::tempdir().unwrap();
    let mut runtime = runtime(cache.path());
    runtime.validate_within = std::time::Duration::from_secs(1);
    let spec =
        spec(json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/stalls/FeatureServer/0")}), good_fields());
    let started = std::time::Instant::now();
    let report = validate(&runtime, &PARCELS, &spec, dane()).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(failed(&report), ["time"]);
    assert!(report.summary().contains("1 s"), "{}", report.summary());
}

#[tokio::test]
async fn a_query_arcgis_online_times_out_on_is_asked_again() {
    let root = server().await;
    for variant in ["flaky", "flaky-gateway"] {
        let report = run(&spec(
            json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/{variant}/FeatureServer/0")}),
            good_fields(),
        ))
        .await;
        assert!(report.passed, "{variant}: {}", report.summary());
    }
}

#[tokio::test]
async fn a_county_whose_interior_point_has_no_parcels_is_sampled_where_its_parcels_are() {
    // Parcels only in a corner of Dane County, far from the point it is sampled around.
    let [x, y] = dane().interior;
    let corner = (x + 0.2, y - 0.1);
    assert!(dane().contains(corner.0, corner.1), "the corner is inside the county");
    let root = support::serve(support::Grid::around(corner.0, corner.1)).await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/good/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert!(report.passed, "{}", report.summary());
    let sample = report.checks.iter().find(|c| c.name == "sample").expect("the report says where it sampled");
    assert!(sample.detail.contains("no parcels near"), "{}", sample.detail);
}

#[tokio::test]
async fn a_layer_with_no_parcels_in_the_county_still_fails() {
    // Parcels only outside Dane County.
    let elsewhere = counties().get("55001").unwrap().interior;
    let root = support::serve(support::Grid::around(elsewhere[0], elsewhere[1])).await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/good/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert!(!report.passed);
    assert!(failed(&report).contains(&"features"), "{}", report.summary());
}

#[tokio::test]
async fn polygons_that_cross_themselves_are_repaired_and_pass() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/bowties/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert!(report.passed, "{}", report.summary());
    let valid = report.checks.iter().find(|c| c.name == "valid-polygons").unwrap();
    assert!(valid.detail.contains("repaired"), "{}", valid.detail);
    assert!(report.repaired > 0 && report.repaired < report.features, "{} of {}", report.repaired, report.features);
}

#[tokio::test]
async fn polygons_with_no_area_cannot_be_repaired_and_fail() {
    let root = server().await;
    let report = run(&spec(
        json!({"kind": "arcgis-feature", "url": format!("{root}/arcgis/collapsed/FeatureServer/0")}),
        good_fields(),
    ))
    .await;
    assert_eq!(failed(&report), ["valid-polygons"], "{}", report.summary());
}
