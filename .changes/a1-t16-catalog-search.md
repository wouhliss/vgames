---
audience: internal
component: server
type: changed
---
Catalog: `sort=recent` now orders by `(updated_at, id) DESC`, matching its keyset cursor and `packages_updated_idx`; before, packages sharing an `updated_at` could repeat or be skipped across pages (regression test `catalog_pages_visit_every_package_once_with_ties`). The genre filter is `genres @> ARRAY[$genre]` with a new GIN index (on 100k packages: a rare genre 25 → 0.5 ms, genre + linux 11 → 5 ms). Searches sort their matches before checking for a release (by recency 34–42 → 21–35 ms, two words 17 → 10 ms; a broad one-word search by title stays about 40 ms).
