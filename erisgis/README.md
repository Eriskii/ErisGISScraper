# erisgis

Declarative adapters for the public GIS layers US counties publish, one dataset at a time: tax
parcels and zoning districts. A spec points at a county's (or city's, or state's) layer and maps
its attributes to the dataset's normalized fields; this crate holds the registry of them, reads
features through them, proves a spec works, and reads what a map view's box holds.

| Module | What it is |
| --- | --- |
| `dataset`, `parcels`, `zoning` | What differs between datasets: fields, sampling, how features become map features, unavailable reasons, agent instructions |
| `spec` | The adapter spec and the unavailable record |
| `registry` | A dataset's registry folder: `adapters/`, `unavailable/`, `index.json` |
| `runtime` | Features in a box through a spec: ArcGIS REST, WFS, OGC API Features, bulk downloads; invalid polygons repaired (`geom::repair`) |
| `validate` | Runs a spec for a county and reports every check |
| `lookup` | A map view's box: the counties it crosses, the adapters each reads through, the features |
| `counties` | Every county's boundary, embedded |
| `http` | The one client for county servers: GET only, honest User-Agent, size caps, per-host limits, no private addresses |
| `testing` | (feature `testing`) A fake county GIS server on loopback |

```rust
use erisgis::{counties::Counties, geom::Bbox, lookup, registry::Registry, zoning::ZONING};

let registry = Registry::open("data/ErisData/GIS/zoning".as_ref())?;
let view = lookup::read(&runtime, &registry, &ZONING, &Counties::embedded()?, Bbox::around(-89.38, 43.07, 0.01), None).await;
```

[ErisGISScraper](..)'s agents write the registries, which live in
[ErisData](https://github.com/Eriskii/ErisData) under `GIS/`.
