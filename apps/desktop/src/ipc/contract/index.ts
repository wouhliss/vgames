// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { catalogCommands } from "./catalog";
import { compatCommands } from "./compat";
import { coreCommands, coreEvents } from "./core";
import { libraryCommands, libraryEvents } from "./library";
import { settingsCommands } from "./settings";

export const pendingCommands = {
  ...coreCommands,
  ...libraryCommands,
  ...catalogCommands,
  ...settingsCommands,
  ...compatCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...libraryEvents,
};

export type * from "./catalog";
export type * from "./compat";
export type * from "./core";
export type * from "./library";
export type * from "./settings";
