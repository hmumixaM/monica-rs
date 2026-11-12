# Offline city reference

This is an adaptation of [GeoNames geographical data](https://www.geonames.org/),
provided by GeoNames under the [Creative Commons Attribution 4.0 International
license](https://creativecommons.org/licenses/by/4.0/). GeoNames and its contributors
are credited for the original geographical names and WGS84 city coordinates.
The source database is supplied without warranty; city-center coordinates do not
identify a person's address. This project is not endorsed by GeoNames.

Extracted from the already installed open-source `geonamescache` 3.0.2 package's
`cities15000`, countries and US states data during isolated development. No user
location data is included. Changes: selected six city columns, normalized alternate
names with Unicode NFKC/case folding, grouped names into a sorted index, and added
the pinned Monica baseline's country aliases. `PROVENANCE.json` records exact
output SHA-256 hashes, row counts and source/license links. Package code is not
vendored and is not needed at runtime.

`cities.tsv`, `names.tsv`, `countries.tsv`, and `states.tsv` are embedded in the Rust
binary. A lazy process-wide index borrows static text instead of allocating a copy
of every name; it is built once on the first city lookup. Ambiguous names remain
unresolved unless a country/state constraint identifies exactly one city.
