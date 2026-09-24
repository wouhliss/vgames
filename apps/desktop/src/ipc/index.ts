// The only entry point the UI uses for the Rust core: the generated tauri-specta bindings (Agent 2),
// plus the not-yet-delivered commands and events from ./contract. Generated entries win.
import { commands as generatedCommands, events as generatedEvents } from "../bindings";
import { pendingCommands, pendingEvents } from "./contract";

export const commands = { ...pendingCommands, ...generatedCommands };
export const events = { ...pendingEvents, ...generatedEvents };

export type * from "../bindings";
export type * from "./contract";
