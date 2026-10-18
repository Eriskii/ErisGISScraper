//! Zoning districts: the polygons a zoning ordinance assigns each district to, with the
//! district's code and name. Counties zone their unincorporated land and cities and towns zone
//! their own, so a county may need several adapters. Every feature read gets a `class`, a broad
//! use guessed from its code and name, so maps can color districts the same way everywhere.

use crate::dataset::{Dataset, Field, Instructions};
use crate::spec::Reason;
use serde_json::{Map, Value, json};

pub const ZONING: Dataset = Dataset {
    name: "zoning",
    one: "zoning district",
    many: "zoning districts",
    fields: &[
        Field { name: "zone", about: "the district's code as the ordinance names it, such as R-1 or C-2" },
        Field { name: "zone_name", about: "the district's name or description, such as Single-Family Residential" },
        Field {
            name: "jurisdiction",
            about: "the city, town or county whose zoning it is, in a layer holding several",
        },
        Field { name: "link", about: "a link to the district's ordinance text" },
    ],
    identifying: &["zone", "zone_name"],
    key: None,
    min_features: 3,
    sample_sizes: &[0.02, 0.08, 0.25],
    stacks: false,
    per_jurisdiction: true,
    reasons: &Reason::ALL,
    derive: Some(derive),
    agents: Instructions {
        county_goal: "You find where one US county's zoning districts are published as polygons, then write and \
            validate adapter specs that read them. The county zones its unincorporated land and each city or town \
            zones its own. Best is one layer holding every jurisdiction's zoning in the county, which county GIS \
            offices often publish with a jurisdiction field. Otherwise submit the county's own zoning layer, then \
            the zoning layers of its largest cities, up to five specs in all, each with its own id and coverage \
            {\"counties\": [\"<your county's FIPS>\"]}. Your job ends when you reply after submitting what you \
            found, or when `spec unavailable` records that the county has none.",
        state_goal: "You look for ONE public layer of zoning district polygons that covers a whole US state (a \
            statewide compilation of local zoning). When you find one, validate a spec for it with coverage \
            {\"states\": [\"XX\"]} using `statewide_spec validate`, and save it with `statewide_spec submit`. Few \
            states compile one. If you find none after a real search, stop and say so in one or two sentences, \
            naming any county or regional zoning layers you came across.",
        how: "- Zoning districts are mostly published as ArcGIS REST layers (…/FeatureServer/N or MapServer/N) named
  Zoning, Zoning Districts or Base Zoning, by a county's GIS office, a city's planning department or a
  regional planning commission. Start with `arcgis search zoning`, then
  `web search <county> county <state> zoning gis`, and for a city `web search <city> <state> zoning gis`.
- Pick the polygon layer of base zoning districts: not overlay districts, not future land use or
  comprehensive plan land use (plans, not zoning), not zoning cases or rezoning petitions, not parcels.
  A parcel layer whose attributes carry each parcel's zoning is a last resort when no district layer
  exists; say so in \"notes\".
- Map zone to the district's code (R-1, C-2, AG). In a layer holding several jurisdictions, map
  jurisdiction to the attribute naming each.
- Many counties do not zone, and in some states counties cannot. Such a county is none-exists only
  if no city or town in it publishes zoning either; when one does, submit theirs. Zoning published
  only as PDF maps or images is viewer-only.",
        example_fields: r#"{"zone": "ZONING", "zone_name": "ZONE_DESC", "jurisdiction": "MUNI",
             "link": "ORD_URL"}"#,
        search: "zoning",
        saved: "If other jurisdictions in your county publish zoning layers of their own that the layer you saved \
            does not hold (its largest cities, say), find, validate and submit those too, each with its own id, up \
            to five in all. Otherwise your job is done: reply with one line saying what you saved.",
        follow_up: "You saved one zoning layer. Before you finish, check what it leaves out: does the county itself \
            zone its unincorporated land, and does the county's largest city publish its own zoning layer? Find, \
            validate and submit those the layer you saved does not hold. If there are none, reply with one line \
            saying why.",
        jurisdictions_goal: "You complete one US county's zoning. Its saved layers, listed in your briefing, leave out \
            some of the jurisdictions in the county that zone their own land: its cities, towns, villages, boroughs \
            and townships, and the county's own unincorporated land. Find the county's jurisdictions (a web search \
            for its municipalities), and check which the saved layers hold: a layer with a jurisdiction field can \
            hold several, and its notes say what it covers. For each jurisdiction they leave out, largest first, \
            find whether it publishes its zoning districts as a queryable layer, and write, validate and submit a \
            spec for each that does, with its own id (such as mi-wayne-dearborn) and coverage {\"counties\": \
            [\"<your county's FIPS>\"]}, at most {most} in this pass. Never submit a layer that is saved already. \
            Small townships and villages rarely publish GIS of their own: one `arcgis search` of the county's \
            zoning, and a look at the county's GIS and its regional planning commission's, checks them all at \
            once, so search one by one only the cities and large townships with a GIS site or a planning \
            department. Before you reply, `spec left` the jurisdictions likely to publish a layer that you did not \
            get to check, if any: the next pass starts from them. Your job ends when you reply: what you saved, and one line for each jurisdiction you checked \
            without finding a queryable layer, saying why, so no pass checks it again.",
    },
};

/// The broad uses a district's `class` names.
pub const CLASSES: [&str; 10] = [
    "residential",
    "commercial",
    "office",
    "mixed-use",
    "industrial",
    "agricultural",
    "open-space",
    "public",
    "planned",
    "other",
];

/// Words in a district's name, and the class each means, checked in order.
const NAME_WORDS: [(&str, &str); 31] = [
    ("mixed", "mixed-use"),
    ("planned", "planned"),
    ("pud", "planned"),
    ("industr", "industrial"),
    ("manufactur", "industrial"),
    ("warehous", "industrial"),
    ("agricultur", "agricultural"),
    ("farm", "agricultural"),
    ("forest", "agricultural"),
    ("ranch", "agricultural"),
    ("open space", "open-space"),
    ("conservation", "open-space"),
    ("preserv", "open-space"),
    ("recreation", "open-space"),
    ("institution", "public"),
    ("public", "public"),
    ("government", "public"),
    ("civic", "public"),
    ("office", "office"),
    ("professional", "office"),
    ("commercial", "commercial"),
    ("business", "commercial"),
    ("retail", "commercial"),
    ("downtown", "mixed-use"),
    ("residential", "residential"),
    ("family", "residential"),
    ("dwelling", "residential"),
    ("housing", "residential"),
    ("home", "residential"),
    ("park", "open-space"),
    ("rural", "agricultural"),
];

/// District code prefixes, and the class each means, longest first.
const CODE_PREFIXES: [(&str, &str); 30] = [
    ("PUD", "planned"),
    ("CBD", "commercial"),
    ("MXD", "mixed-use"),
    ("MU", "mixed-use"),
    ("MX", "mixed-use"),
    ("PD", "planned"),
    ("OS", "open-space"),
    ("AG", "agricultural"),
    ("SF", "residential"),
    ("MF", "residential"),
    ("MH", "residential"),
    ("LI", "industrial"),
    ("HI", "industrial"),
    ("GI", "industrial"),
    ("PF", "public"),
    // Neighbourhood districts: mixed centres, shops, and the neighbourhoods themselves.
    ("NM", "mixed-use"),
    ("NC", "commercial"),
    ("NB", "commercial"),
    // Downtown districts: residential ones, and the rest, which mix uses.
    ("DR", "residential"),
    ("D", "mixed-use"),
    ("R", "residential"),
    ("A", "agricultural"),
    ("C", "commercial"),
    ("B", "commercial"),
    ("I", "industrial"),
    ("M", "industrial"),
    ("O", "office"),
    ("P", "public"),
    ("N", "residential"),
    ("U", "other"),
];

/// Codes that start like a district's but are not one, such as a right of way.
const NOT_DISTRICTS: [&str; 3] = ["ROW", "WATER", "UNZONED"];

/// A district's broad use from its code and name: the name's words when they say, otherwise
/// the code's prefix, otherwise `other`.
pub fn class(zone: Option<&str>, name: Option<&str>) -> &'static str {
    if let Some(name) = name {
        let name = name.to_lowercase();
        // Housing over shops or offices is mixed use, whichever word comes first.
        if name.contains("residential")
            && ["commercial", "business", "office", "retail"].iter().any(|w| name.contains(w))
        {
            return "mixed-use";
        }
        if let Some((_, class)) = NAME_WORDS.iter().find(|(word, _)| name.contains(word)) {
            return class;
        }
    }
    let Some(zone) = zone else { return "other" };
    let code: String = zone.trim().to_uppercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let letters: String = code.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if NOT_DISTRICTS.contains(&letters.as_str()) {
        return "other";
    }
    // Mixed codes say so anywhere: `UMX`, `CMX`, `TMU`.
    if letters.len() <= 4 && (letters.contains("MX") || letters.contains("MU")) {
        return "mixed-use";
    }
    // A prefix with at most two letters after it: `RS`, `RMF` and `CN` are districts of their
    // prefix's class; a longer word is not.
    CODE_PREFIXES
        .iter()
        .find(|(prefix, _)| letters.starts_with(prefix) && letters.len() <= prefix.len() + 2)
        .map_or("other", |(_, class)| class)
}

fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Adds `class` to a district's fields.
fn derive(fields: &mut Map<String, Value>) {
    let class = class(text(fields.get("zone")).as_deref(), text(fields.get("zone_name")).as_deref());
    fields.insert("class".into(), json!(class));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn districts_are_classed_by_their_names_then_their_codes() {
        let cases = [
            (Some("R-1"), None, "residential"),
            (Some("RS-7"), None, "residential"),
            (Some("RR"), None, "residential"),
            (Some("C-2"), None, "commercial"),
            (Some("B-3"), None, "commercial"),
            (Some("CBD"), None, "commercial"),
            (Some("M-1"), None, "industrial"),
            (Some("LI"), None, "industrial"),
            (Some("I-2"), None, "industrial"),
            (Some("A-1"), None, "agricultural"),
            (Some("AG"), None, "agricultural"),
            (Some("MU-3"), None, "mixed-use"),
            (Some("PUD"), None, "planned"),
            (Some("PD-114"), None, "planned"),
            (Some("OS"), None, "open-space"),
            (Some("O-1"), None, "office"),
            (Some("P"), None, "public"),
            (Some("ROW"), None, "other"),
            (Some("XYZ"), None, "other"),
            (None, None, "other"),
            // The name decides when it says.
            (Some("TR-C3"), Some("Traditional Residential - Consistent 3"), "residential"),
            (Some("SR-V2"), Some("Suburban Residential - Varied 2"), "residential"),
            (Some("UMX"), Some("Urban Mixed-Use"), "mixed-use"),
            (Some("CC-T"), Some("Commercial Corridor - Transitional"), "commercial"),
            (Some("R-1"), Some("Planned Unit Development"), "planned"),
            (Some("RC"), Some("Resource Conservation"), "open-space"),
            (Some("SE"), Some("Suburban Employment"), "other"),
            (Some("SE"), Some("Rural Residential"), "residential"),
            (Some("BP"), Some("Business Park"), "commercial"),
            (Some("MHP"), Some("Mobile Home Park"), "residential"),
            (Some("P-1"), Some("Park"), "open-space"),
            (Some("C-N"), None, "commercial"),
            (Some("RMF-2"), None, "residential"),
            (Some("WATER"), None, "other"),
            // Downtown and mixed codes.
            (Some("DC"), None, "mixed-use"),
            (Some("DR1"), None, "residential"),
            (Some("UMX"), None, "mixed-use"),
            (Some("CMX-2"), None, "mixed-use"),
            (Some("TMU"), None, "mixed-use"),
            (Some("RC-4"), Some("Residential- Commercial, High Density"), "mixed-use"),
            (Some("UOR"), Some("Urban Office Residential"), "mixed-use"),
            // Neighbourhood codes, as form-based ordinances name them: neighbourhoods, their
            // mixed centres, and their shops.
            (Some("N1"), Some("R-1"), "residential"),
            (Some("N2"), None, "residential"),
            (Some("NM1"), Some("R-7"), "mixed-use"),
            (Some("NC"), None, "commercial"),
            (Some("NB-2"), None, "commercial"),
        ];
        for (zone, name, expected) in cases {
            assert_eq!(class(zone, name), expected, "{zone:?} {name:?}");
            assert!(CLASSES.contains(&expected));
        }
    }

    #[test]
    fn every_district_read_gets_a_class() {
        let mut fields = Map::new();
        fields.insert("zone".into(), json!("C-2"));
        derive(&mut fields);
        assert_eq!(fields["class"], "commercial");
        let mut numeric = Map::new();
        numeric.insert("zone_name".into(), json!(12));
        derive(&mut numeric);
        assert_eq!(numeric["class"], "other");
    }
}
