//! A dataset is one kind of public GIS layer that counties publish, such as tax parcels or
//! zoning districts. The specs, registry, runtime, validator and lookup are the same for every
//! dataset; what differs is described here: the normalized fields a spec maps, how the validator
//! samples and judges a layer, how features become map features, and what agents are told to
//! look for. Each dataset's registry is its own folder, named after it.

use crate::spec::Reason;
use serde_json::{Map, Value};

/// A normalized field: its name in specs and features, and what it holds.
pub struct Field {
    pub name: &'static str,
    pub about: &'static str,
}

pub struct Dataset {
    /// The registry folder and the scraper's `--dataset`, such as `parcels`.
    pub name: &'static str,
    /// What one feature is, and several, in reports and prompts: `parcel`, `parcels`.
    pub one: &'static str,
    pub many: &'static str,
    /// The normalized fields, in the order they are shown.
    pub fields: &'static [Field],
    /// A spec must map at least one of these.
    pub identifying: &'static [&'static str],
    /// The field that names a feature within its layer, for its map key; without one, or when a
    /// feature has none, a hash of its attributes names it.
    pub key: Option<&'static str>,
    /// Fewest features a validation sample must hold.
    pub min_features: usize,
    /// Sample box sizes in degrees, tried in turn until one holds enough features.
    pub sample_sizes: &'static [f64],
    /// Whether features drawn on one outline are one map feature listing each as a unit, as a
    /// building's condominium parcels are.
    pub stacks: bool,
    /// Whether a county may need several adapters, one per jurisdiction publishing its own
    /// layer, as a county's zoning and its cities' zoning are. Otherwise one adapter covers it.
    pub per_jurisdiction: bool,
    /// Why a county may be recorded as having no usable layer.
    pub reasons: &'static [Reason],
    /// Fields computed from the normalized ones on every feature read, such as zoning's `class`.
    pub derive: Option<fn(&mut Map<String, Value>)>,
    pub agents: Instructions,
}

/// What agents are told about a dataset; the scraper's prompts are built around it.
pub struct Instructions {
    /// A county job's goal.
    pub county_goal: &'static str,
    /// A state job's goal.
    pub state_goal: &'static str,
    /// How to find the layer and choose among layers.
    pub how: &'static str,
    /// The `fields` of the example spec, as JSON.
    pub example_fields: &'static str,
    /// An example `arcgis search` and `web search`, for the tools' help.
    pub search: &'static str,
    /// What `spec submit` answers once a spec is saved: the job ends, or more layers follow.
    pub saved: &'static str,
    /// In a dataset with an adapter per jurisdiction, what an agent that saved one layer and
    /// stopped is asked once, about the jurisdictions it may have missed.
    pub follow_up: &'static str,
    /// In a dataset with an adapter per jurisdiction, the goal of a pass over a covered county
    /// for the jurisdictions its saved layers leave out; `{most}` is how many one pass saves.
    pub jurisdictions_goal: &'static str,
}

impl Dataset {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The datasets, by name.
    pub fn named(name: &str) -> Option<&'static Dataset> {
        ALL.iter().copied().find(|d| d.name == name)
    }
}

pub const ALL: [&Dataset; 2] = [&crate::parcels::PARCELS, &crate::zoning::ZONING];
