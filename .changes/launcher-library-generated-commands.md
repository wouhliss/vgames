---
audience: internal
component: launcher
type: changed
---
INS-01: the launcher UI uses the generated library commands (`libraries_list`, `library_pick_folder`, `library_add`, `library_set_default`, `library_remove`) and types (`LibraryInfo`, `LibraryActionError`, `LibraryRemovalError`); the pending contract entries and types are deleted, and the mock matches the Rust store (downloads keep a library in use; removing the default promotes the oldest remaining library).
