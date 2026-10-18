# ErisGISScraper

The program that writes and keeps registries of GIS adapters for US counties, one registry per
dataset: tax parcels and zoning districts. An adapter is a small declarative spec that points at
a public layer (a county's, a city's, or a statewide one) and maps its attributes to the
dataset's normalized fields. The registries hold pointers, never features, and live in
[ErisData](https://github.com/Eriskii/ErisData) under `GIS/`.

`eris-gis-scraper` runs [ErisHarness](https://github.com/Eriskii/ErisHarness) agents over the
3,222 counties and county equivalents of the 50 states, DC and Puerto Rico. Agents search for each
county's layers, write specs, and prove they work with a validator that actually queries the
service. A spec reaches the registry only by passing it.

## Crates

| Crate | What it is |
| --- | --- |
| [`erisgis`](erisgis) | The library: the datasets, specs, the registry, the runtime that reads features through a spec, the validator, the lookup that reads a map view's features, the embedded counties, and the polite HTTP client. No agents. [ErisMaps](https://github.com/Eriskii/ErisMaps) reads parcels and zoning through it. Its `testing` feature is the fake county GIS server the tests run against. |
| `erisgisscraper` | The harness: the scheduler and model ladder, the journal, the agents' command-line tools, the terminal view and the field notes, for whichever dataset `--dataset` names. |

## Datasets

| `--dataset` | What a county's adapters read | Normalized fields | Adapters per county |
| --- | --- | --- | --- |
| `parcels` (default) | Tax parcel polygons | `apn`, `owner`, `address`, `acres`, `land_use`, `assessed_value` | One |
| `zoning` | Base zoning district polygons | `zone`, `zone_name`, `jurisdiction`, `link` | One per jurisdiction: the county's own zoning, and its cities' |

A dataset (`erisgis::Dataset`) is a description the harness reads: its fields and which of them
identify a feature, how the validator samples (parcels: at least 5 in boxes of 0.01°, 0.04°,
then 0.12°; zoning districts: at least 3 in boxes of 0.02°, 0.08°, then 0.25°), how features
become map features (parcels on one outline stack into one feature listing its units; every
zoning district gets a `class`), the reasons a county may be unavailable, and what agents are
told to look for.

Zoning is set by counties for their unincorporated land and by cities and towns for their own,
so a zoning agent may save several layers for one county: a combined layer when the county
publishes one, otherwise the county's own and its largest cities', up to five. An agent that
saves one layer and stops is asked once whether the county's own zoning or its largest city's is
missing. `class` is a broad use (`residential`, `commercial`, `office`, `mixed-use`,
`industrial`, `agricultural`, `open-space`, `public`, `planned`, `other`) guessed from the
district's name, then its code, so maps can color every jurisdiction's districts alike; it is
computed whenever a feature is read, never stored.

## The registry

Each dataset's registry is the folder named after it:

```text
GIS/<dataset>/adapters/<id>.json        one spec each
GIS/<dataset>/unavailable/<fips>.json   why a county has no adapter, and when to look again
GIS/<dataset>/index.json                county FIPS → ids of the adapters verified for it
```

A spec:

```json
{
  "version": 1,
  "id": "ga-fulton",
  "publisher": "Fulton County GIS",
  "official": true,
  "source": { "kind": "arcgis-feature", "url": "https://gismaps.fultoncountyga.gov/arcgispub/rest/services/OpenData/Tax_Parcels/MapServer/0",
              "page_size": 2000, "geojson": true },
  "coverage": { "counties": ["13121"] },
  "fields": { "apn": "ParcelID", "owner": "Owner", "address": "Address", "acres": "LandAcres", "land_use": "LUCode" },
  "notes": "Fulton County open data",
  "extent": [-84.85, 33.50, -84.10, 34.19],
  "verification": { "verified_at": 1792041362000, "counties": ["13121"], "model": "…", "rung": 0 }
}
```

| Source kind      | Reads                                                                                 |
| ---------------- | ------------------------------------------------------------------------------------- |
| `arcgis-feature` | An ArcGIS REST FeatureServer or MapServer layer, as GeoJSON or Esri JSON, paged        |
| `wfs`            | A WFS 1.x or 2.0 `GetFeature` as GeoJSON, paged, with latitude-first boxes on request  |
| `ogc-features`   | An OGC API Features collection, following `next` links                                |
| `bulk-download`  | A GeoJSON file or zipped shapefile in EPSG 4326, 4269 or 3857, downloaded once         |

`official` is `true` for the jurisdiction's own layer (the county or city, its state, or a vendor
serving it for them) and `false` for a copy someone else hosts, such as a personal ArcGIS Online
item, with whose copy it is in `notes`; agents use a copy only when nothing official is
queryable, and `spec submit` refuses a spec that does not say. Specs written before the field
existed read as unknown. `notes` lists the unofficial adapters. `coverage` is a list of county
FIPS codes or a list of states. `fields` maps the dataset's normalized fields, each optional, to
the source's attribute names; `filter` is an optional static filter (an ArcGIS `where` clause).
`extent` is where the layer's features lie, `[west, south, east, north]` in degrees, as an ArcGIS
source answers when the spec is saved or passes a check; a map box it misses is not read from
it, so a county read through many cities' layers asks each only about its own ground. `spec
submit` refuses a layer saved already for the county under another id, and the whole of a layer
part of which is saved (or a part of one saved whole), whose features would show twice. An
error naming an HTML answer (a block page, a login) names it by its title.
Unavailable reasons are `no-public-gis`, `viewer-only`, `paywalled`, `login-required`,
`tos-forbids-automation`, and for zoning `none-exists` (a county that does not zone, with no
city in it that does); a record is searched again after 180 days, or 365 for `paywalled` and
`tos-forbids-automation`.

## Validation

`validate` runs a spec for one county:

- **query**: the service answers, and pages without repeating itself.
- **features**: enough features around a point inside the county, in the dataset's sample
  boxes; a county whose point has none nearby is sampled around one of the layer's own features
  inside it.
- **longitude-latitude**, **location**, **in-county**: coordinates are degrees, lie where they
  were asked for (each feature's extent meets the sample box), and mostly inside the county's
  boundary. Swapped axes and projected coordinates fail here.
- **valid-polygons**: 90% of polygons are valid as the source has them or once repaired. The
  runtime repairs every invalid feature it reads: a ring that crosses itself becomes the pieces
  it encloses, an edge that doubles back is dropped, and overlapping parts of one feature merge
  into their union. A feature with no area left, such as a ring collapsed onto a line, is past
  repair. The report says how many were repaired.
- **fields-mapped**, **field-fill**: at least one of the dataset's identifying fields is mapped
  (parcels: `apn`, `address` or `owner`; zoning: `zone` or `zone_name`); every mapped attribute
  exists and is filled for half the features or more.
- **paging**: reading the sample again in two halves returns the same features.
- **time**: the whole validation takes under two minutes.

The report lists the source's attribute names and sample attributes, which is how agents map
fields. A state's spec is validated against three counties spread across the state and passes
when two do.

All requests go through one client: GET only, an honest User-Agent, 60-second timeouts, size
caps, three requests at a time per host, and no private or loopback addresses (agents choose
the URLs).

## Runs

1. **States first.** Each selected state with open counties gets one agent looking for a
   statewide layer. When it finishes, every adapter claiming one of the state's open counties is
   checked against that county, with no model involved. Checks run 12 at a time beside the
   agents, so they never hold an agent slot, and each result is in the journal: a check that
   failed is not made again on a later run.
2. **Counties.** Each county still open gets an agent, briefed with its boundary, the adapters
   its neighbours use (counties often share a host or vendor), adapters that claim it but fail,
   and what earlier attempts tried.
3. **The model ladder.** Models are listed cheapest first. A county starts on rung 0; an attempt
   that runs out of failed validations (`--validate-budget`, 8 by default), stops without a
   result after one nudge, or fails, is retried on the next rung (after
   `--attempts-per-rung`), briefed with the last attempt's own summary and its validation
   results. The rung that succeeds is recorded in the spec and the journal. Each rung runs on
   the provider that serves its model: Codex for the ChatGPT login, the Anthropic API for the
   Claude login, both built by ErisAgentAuth. The default ladder comes from the signed-in
   logins' model listings, which ErisAgentAuth fetches: with a
   Claude login, the newest Haiku at medium effort, then Sonnet at high, then ChatGPT's flagship
   (or Opus without ChatGPT) at high; with ChatGPT alone, its small model at medium effort, then
   its flagship at medium and high. Claude models think adaptively, at the rung's effort; a rung
   takes only an effort its model lists, and Claude models that think only on a budget (the
   4.5 generation) cannot run.

Agents' tools are three command lines, each called with one `command` string, split as a
shell splits words (quotes, backslashes, no expansion) and parsed with clap through
ErisHarness's `tools::cli`. Their help is in the tool description and behind `--help`; a parse
error comes back as the tool's error. They run in-process: there is no shell, no file system
and no sandbox. `web` is [ErisHarness](https://github.com/Eriskii/ErisHarness)'s, set up with
the run's models; `arcgis` and `spec` are the scraper's.

```text
web search <query…>                                         up to 8 results: title, URL, one line
web read <url> <question…>                                  a few lines answering the question
web get <url> [--offset N] [--grep PATTERN [--context N]]   JSON endpoints, 4,000 characters a window
arcgis search <query…> [--state XX]                         ArcGIS Hub and Online, in the job's box
arcgis probe <url> [--grep PATTERN]                         directories, services, layers and fields
spec validate '<json>'                                      run a spec and read its report
spec show                                                   the spec validated last, and its report
spec submit                                                 save the spec validated last, if it passed
spec unavailable <reason> --evidence <url>… --note "…"      county jobs only
```

`web search` and `web read` run on models of their own, so only their few lines enter the agent's
context. With a Claude login, the newest Haiku searches with Claude's hosted web search and reads
pages, at low effort; with ChatGPT alone, its small model does both, searching with OpenAI's hosted
web search on the Codex endpoint. Search falls back to DuckDuckGo's results page when neither can
search. A reader gets up to 120,000 characters of a page and answers with the URLs it relied on.
The tokens these calls spend count toward the agent's attempt, and each attempt's `finished` line
in the journal lists them by command under `tools`.

`--grep` takes a case-insensitive regular expression and is how agents cut a long answer, such as
a services directory, down to the lines they need. Statewide searches get the same `spec` as
`statewide_spec`, without `unavailable`. The terminal view shows each agent's current command
line.

**Checkpoints.** `state/<dataset>/journal.jsonl` gets a line, flushed to disk, when each
attempt starts and ends. The harness keeps its agents in `state/<dataset>/harness`. A run that is killed or stopped
resumes on the next start: running agents go on where they stood, finished attempts are not
repeated, and each county keeps its rung. `--recheck` validates every saved adapter first; a
county whose adapter fails is searched again with the dead spec in its briefing.

**Jurisdictions.** In zoning, which is taken per jurisdiction, `run --jurisdictions` passes over
the selected counties that are covered, instead of searching open ones, for the jurisdictions
their saved layers leave out: a city's zoning beside the county's, say, or a county's
unincorporated land beside its largest city. The agent is told the county's saved layers (what
each holds, and its notes), lists the county's jurisdictions, and submits a spec for each that
publishes its zoning, largest first, at most 8 in a pass. Small townships and villages are
checked all at once, by a search of the county's zoning and a look at its GIS and regional
planning commission; cities and large townships one by one. `spec left` names those likely to
publish a layer that it did not get to (as a list with commas), and its reply names each
jurisdiction it checked without finding a queryable layer, and why. A pass that saves 8 or names
jurisdictions left is followed by another, briefed with its report, up to 5 such passes in a row;
the journal keeps the count, so a restarted run makes no more. The journal records each pass as a `jurisdictions` job,
so a county is passed over once; `--again` passes over those done before too, each briefed with
what its last pass said. A pass whose turn fails climbs the ladder as any job does.

## Using it

```sh
cargo build --release
eris-gis-scraper login                 # ChatGPT, in the browser; --device for a device code
eris-gis-scraper login --claude        # bind Claude Code's ~/.claude/.credentials.json
eris-gis-scraper login --claude FILE   # bind another Claude Code credentials file
eris-gis-scraper models                # the logins' models and the default ladder
eris-gis-scraper run                   # every county, in the terminal view
eris-gis-scraper run --states WI,GA --concurrency 16
eris-gis-scraper run --counties 13121 --no-tui --ladder gpt-5-codex-mini:medium,gpt-5-codex:high
eris-gis-scraper run --states WI --ladder haiku:medium,sonnet:high,opus:max
eris-gis-scraper run --recheck
eris-gis-scraper --dataset zoning run --ladder haiku:medium,sonnet:high --concurrency 64
eris-gis-scraper --dataset zoning run --jurisdictions --states MI   # covered counties' other jurisdictions
eris-gis-scraper --dataset zoning run --jurisdictions --again --counties 26163
eris-gis-scraper validate spec.json 13121
eris-gis-scraper --dataset zoning validate spec.json 13121
eris-gis-scraper status                # the registry by state; --dataset zoning for zoning's
eris-gis-scraper notes --out notes.md  # field notes: unavailable counties, what stopped agents, costs
eris-gis-scraper import-login ~/.config/ErisMaps/agents/account.json
eris-gis-scraper regenerate-counties   # rebuild erisgis/data/counties.json.gz from census.gov
```

`--dataset` (default `parcels`) chooses the dataset. `--registry` (default
`../../data/ErisData/GIS`, the ErisData checkout in the monorepo; each dataset's registry is its
folder in it) and `--state` (default `state`; each dataset's journal and agents are its folder
in it) choose the directories. The logins, shared by every dataset, live in `state/accounts/`, owner-only, through [ErisAgentAuth](../../libs/ErisAgentAuth):

- `chatgpt.json`: the ChatGPT login, kept and refreshed.
- `claude.json`: a bound Claude login, which is only the credentials file's path (by default
  `$CLAUDE_CONFIG_DIR/.credentials.json`, or `~/.claude/.credentials.json`). Every request
  reads the token from that file and nothing here refreshes it, so the Claude Code that owns it
  stays signed in. When the file's token has expired, runs pause and the terminal view says so;
  opening Claude Code refreshes the file, and `p` resumes. The binding lasts until the next
  `login --claude`.

`ERISGISSCRAPER_OPENAI_AUTH`, `ERISGISSCRAPER_CODEX_API`, `ERISGISSCRAPER_CLAUDE_API` and
`ERISGISSCRAPER_CLAUDE_AUTH` point at other servers.

The terminal view shows overall progress, each state, the claim checks under way, live agents
with the tool call each is making, the ladder's rungs with their attempts, results and tokens, and the log. Keys: `p`
pause and resume, `+`/`-` concurrency, `l` sign in with a device code, `L` sign in in the
browser, `q` quit (agents resume on the next run). `--no-tui` prints the log and a progress line
instead, and Ctrl-C stops it the same way.

## Counties

`erisgis/data/counties.json.gz` holds every county's FIPS, name, state, bounding box, an interior point
and its boundary, from the Census Bureau's 2024 1:500,000 cartographic boundaries, simplified to
about 30 m. It is embedded in the binary; `regenerate-counties` rebuilds it.

## Tests

`cargo test --workspace` runs `erisgis`'s validator, lookup and zoning tests and the
scraper's. They cover the command-line tools as agents call them (help, parse errors, quoting,
`--grep`, the spec workflow, and `web` as the scraper sets it up: its command lines in the terminal view and its search and reading tokens charged to the job, on a stand-in Claude API; ErisHarness tests the tool itself), the validator against a fake county server on loopback (ArcGIS GeoJSON and
Esri JSON, a latitude-first WFS, OGC API Features, and broken variants: projected coordinates,
ignored offsets, refusals, missing fields), zoning on a fake zoning layer (validation, a county read through several jurisdictions' layers,
district classes, each dataset's spec help and reasons), and whole runs against a stand-in
model: an agent that gives up and hands over to the next rung, a statewide layer covering its
counties without county agents, a county marked unavailable, a run stopped mid-turn that
resumes its agent, a zoning agent asked once about the jurisdictions its layer left out, a
jurisdictions pass over a covered county (its briefing, a saved layer refused a second time, the
city's layer saved with its extent, and no second pass once done), and a pass that saves as many
layers as one may, or names jurisdictions left to check, followed by another briefed with its
report, up to the limit, which a restarted run keeps, and a county passed over again on request. The lookup tests cover a box a
layer's extent misses, which is not read from it.
`ERISGISSCRAPER_LIVE_STATE=<state dir> cargo test --test live -- --ignored --nocapture` runs a
real search and page read on each login in that state directory: the bound Claude login, and the
ChatGPT login alone.
