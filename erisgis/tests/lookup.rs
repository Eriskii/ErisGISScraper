//! Reading a map view's features through a dataset's registry: the counties a box crosses, the
//! adapters each is read through, and the features, against a fake county server.

use erisgis::testing as support;

use erisgis::counties::Counties;
use erisgis::geom::Bbox;
use erisgis::http::{Http, Limits};
use erisgis::lookup::{self, View};
use erisgis::parcels::PARCELS;
use erisgis::registry::Registry;
use erisgis::runtime::{Runtime, VALIDATE_WITHIN};
use serde_json::{Value, json};
use std::path::Path;

fn runtime(dir: &Path) -> Runtime {
    Runtime {
        http: Http::new(Limits { allow_private: true, ..Limits::default() }).unwrap(),
        cache: dir.join("downloads"),
        validate_within: VALIDATE_WITHIN,
    }
}

fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value.to_string()).unwrap();
}

fn adapter(dir: &Path, id: &str, url: &str, coverage: Value) {
    write(
        &dir.join(format!("adapters/{id}.json")),
        &json!({"version": 1, "id": id, "publisher": "Dane County Land Information", "coverage": coverage,
            "source": {"kind": "arcgis-feature", "url": url},
            "fields": {"apn": "PARCELNO", "owner": "OWNER", "address": "SITEADDR", "acres": "GISACRES"}}),
    );
}

/// A box of about 300 m around Dane County's interior point, inside Dane County alone.
fn dane_box(counties: &Counties) -> Bbox {
    let [x, y] = counties.get("55025").unwrap().interior;
    Bbox::around(x, y, 0.003)
}

async fn read(registry: &Path, cache: &Path, bbox: Bbox) -> View {
    let counties = Counties::embedded().unwrap();
    let registry = Registry::open(registry).unwrap();
    lookup::read(&runtime(cache), &registry, &PARCELS, &counties, bbox, Some(cache)).await
}

#[tokio::test]
async fn a_box_in_a_covered_county_holds_its_parcels_with_their_fields_and_source() {
    let counties = Counties::embedded().unwrap();
    let [x, y] = counties.get("55025").unwrap().interior;
    let root = support::serve(support::Grid::around(x, y)).await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    adapter(&registry, "wi-dane", &format!("{root}/arcgis/good/FeatureServer/0"), json!({"counties": ["55025"]}));
    write(&registry.join("index.json"), &json!({"version": 1, "counties": {"55025": ["wi-dane"]}}));

    let view = read(&registry, &dir.path().join("cache"), dane_box(&counties)).await;
    assert_eq!(view.counties.len(), 1);
    let dane = &view.counties[0];
    assert_eq!((dane.fips.as_str(), dane.county.as_str()), ("55025", "Dane County, WI"));
    let [source] = dane.sources.as_slice() else { panic!("one source: {:?}", dane.sources) };
    assert_eq!((source.adapter.as_str(), source.verified), ("wi-dane", true));
    assert_eq!(source.error, None);
    assert!(!view.features.is_empty());
    let first = &view.features[0];
    assert_eq!(first["geometry"]["type"], "MultiPolygon");
    let properties = &first["properties"];
    assert_eq!(properties["adapter"], "wi-dane");
    assert_eq!(properties["publisher"], "Dane County Land Information");
    assert!(properties["apn"].as_str().unwrap().starts_with("0810-"));
    assert_eq!(properties["acres"], 1.25);
    assert_eq!(properties["key"], format!("wi-dane:{}", properties["apn"].as_str().unwrap()));
    let keys: std::collections::HashSet<&str> =
        view.features.iter().map(|f| f["properties"]["key"].as_str().unwrap()).collect();
    assert_eq!(keys.len(), view.features.len(), "each parcel once");
}

#[tokio::test]
async fn a_statewide_claim_reads_a_county_it_was_not_verified_for() {
    let counties = Counties::embedded().unwrap();
    let [x, y] = counties.get("55025").unwrap().interior;
    let root = support::serve(support::Grid::around(x, y)).await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    adapter(&registry, "wi-statewide", &format!("{root}/arcgis/good/FeatureServer/0"), json!({"states": ["WI"]}));

    let view = read(&registry, &dir.path().join("cache"), dane_box(&counties)).await;
    let source = view.counties[0].sources.first().unwrap();
    assert_eq!((source.adapter.as_str(), source.verified), ("wi-statewide", false));
    assert!(!view.features.is_empty());
}

#[tokio::test]
async fn a_county_without_an_adapter_says_why_and_shows_nothing() {
    let counties = Counties::embedded().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    write(
        &registry.join("unavailable/55025.json"),
        &json!({"version": 1, "fips": "55025", "reason": "paywalled", "evidence": ["https://dane.example/store"],
            "note": "Parcels are sold.", "recorded_at": 1, "recheck_after": u64::MAX}),
    );
    let view = read(&registry, &dir.path().join("cache"), dane_box(&counties)).await;
    let dane = &view.counties[0];
    assert!(dane.sources.is_empty());
    let unavailable = dane.unavailable.as_ref().unwrap();
    assert_eq!((unavailable.reason.as_str(), unavailable.note.as_str()), ("paywalled", "Parcels are sold."));
    assert!(view.features.is_empty());

    let empty = read(&dir.path().join("nothing"), &dir.path().join("cache"), dane_box(&counties)).await;
    assert!(empty.counties[0].sources.is_empty() && empty.counties[0].unavailable.is_none());
}

#[tokio::test]
async fn a_box_across_a_county_line_names_both_counties() {
    let counties = Counties::embedded().unwrap();
    let dane = counties.get("55025").unwrap();
    // Dane County's western edge, straddled.
    let west = dane.bbox.west();
    let y = dane.interior[1];
    let crossing = lookup::counties_in(&counties, Bbox([west - 0.01, y - 0.005, west + 0.01, y + 0.005]));
    let fips: Vec<&str> = crossing.iter().map(|c| c.fips.as_str()).collect();
    assert!(fips.contains(&"55025") && fips.len() >= 2, "{fips:?}");
}

#[tokio::test]
async fn answers_are_kept_and_read_again_without_the_server() {
    let counties = Counties::embedded().unwrap();
    let [x, y] = counties.get("55025").unwrap().interior;
    let root = support::serve(support::Grid::around(x, y)).await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    adapter(&registry, "wi-dane", &format!("{root}/arcgis/good/FeatureServer/0"), json!({"counties": ["55025"]}));
    let cache = dir.path().join("cache");
    let first = read(&registry, &cache, dane_box(&counties)).await;
    // The same box again, with the source moved away: the kept answer serves it.
    adapter(&registry, "wi-dane", "http://127.0.0.1:9/arcgis/gone/FeatureServer/0", json!({"counties": ["55025"]}));
    let again = read(&registry, &cache, dane_box(&counties)).await;
    assert_eq!(again.counties[0].sources[0].error, None, "kept by the source it was read from, not the URL");
    assert_eq!(again.features.len(), first.features.len());
}

#[tokio::test]
async fn parcels_stacked_on_one_outline_are_one_feature_listing_its_units() {
    let counties = Counties::embedded().unwrap();
    let [x, y] = counties.get("55025").unwrap().interior;
    let root = support::serve(support::Grid::around(x, y)).await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    adapter(&registry, "wi-dane", &format!("{root}/arcgis/condos/FeatureServer/0"), json!({"counties": ["55025"]}));
    write(&registry.join("index.json"), &json!({"version": 1, "counties": {"55025": ["wi-dane"]}}));

    let view = read(&registry, &dir.path().join("cache"), dane_box(&counties)).await;
    let units = |f: &Value| f["properties"]["unit_count"].as_u64().unwrap_or(1);
    let three = view.features.iter().find(|f| units(f) == 3).expect("a building of three units");
    let listed = three["properties"]["units"].as_array().unwrap();
    let apns: Vec<&str> = listed.iter().map(|u| u["apn"].as_str().unwrap()).collect();
    assert_eq!(apns.len(), 3);
    assert!(apns.iter().all(|a| a.ends_with("-U1") || a.ends_with("-U2") || a.ends_with("-U3")), "{apns:?}");
    assert!(listed.iter().all(|u| u["address"].as_str().unwrap().contains(" Unit ")));
    let key = three["properties"]["key"].as_str().unwrap();
    assert_eq!(key, format!("wi-dane:{}", apns.iter().min().unwrap()), "the smallest unit's key");

    // Two units under one parcel number are both listed, not dropped as one key.
    let shared = view.features.iter().find(|f| units(f) == 2).expect("two units sharing a parcel number");
    let addresses: Vec<&str> =
        shared["properties"]["units"].as_array().unwrap().iter().map(|u| u["address"].as_str().unwrap()).collect();
    assert_eq!(addresses.len(), 2);
    assert_ne!(addresses[0], addresses[1]);

    // Parcels of their own stay as they are, and every outline is drawn once.
    let single = view.features.iter().find(|f| units(f) == 1).unwrap();
    assert!(single["properties"].get("units").is_none());
    let outlines: std::collections::HashSet<String> = view.features.iter().map(|f| f["geometry"].to_string()).collect();
    assert_eq!(outlines.len(), view.features.len(), "one feature per outline");
}

#[tokio::test]
async fn the_map_gets_repaired_outlines() {
    use geo::Validation;
    let counties = Counties::embedded().unwrap();
    let [x, y] = counties.get("55025").unwrap().interior;
    let root = support::serve(support::Grid::around(x, y)).await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    adapter(&registry, "wi-dane", &format!("{root}/arcgis/bowties/FeatureServer/0"), json!({"counties": ["55025"]}));
    write(&registry.join("index.json"), &json!({"version": 1, "counties": {"55025": ["wi-dane"]}}));

    let view = read(&registry, &dir.path().join("cache"), dane_box(&counties)).await;
    assert!(view.features.len() > 3);
    for feature in &view.features {
        let geometry = erisgis::geom::from_geojson(&feature["geometry"]).unwrap();
        assert!(geometry.is_valid(), "{}", feature["properties"]["key"]);
    }
}
