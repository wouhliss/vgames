// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { coreCommands, coreEvents } from "./core";
import { libraryCommands, libraryEvents } from "./library";

export const pendingCommands = {
  ...coreCommands,
  ...libraryCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...libraryEvents,
};

export type * from "./core";
export type * from "./library";
