# Agent 2 status

## Done

## In progress
- A2-T01 Desktop shell bootstrap (AppState, event bus, logging, plugins, tauri-specta bindings)
- A2-T02 Local database (SQLite on a dedicated thread, embedded migrations)
- A2-T03 `vgames-pack` planner (coded against 02 §4 until `vgames_core::layout` lands)

## Interfaces delivered (other agents may now rely on these)

## Needs from others
- From Agent 5: `vgames_core::{layout, paths, manifest}` (A5-T02) for A2-T03;
  `sign`, `trust`, `verify_manifest` (A5-T03/T04) for A2-T04 and A2-T07.
- From Agent 1: fs storage backend protocol (A1-T07) for transfer engine tests.

## Blockers / contract questions
