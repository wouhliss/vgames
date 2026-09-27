// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { catalogCommands } from "./catalog";
import { coreCommands, coreEvents } from "./core";
import { downloadCommands, downloadEvents } from "./downloads";
import { libraryCommands, libraryEvents } from "./library";
import { settingsCommands } from "./settings";

export const pendingCommands = {
  ...coreCommands,
  ...libraryCommands,
  ...catalogCommands,
  ...downloadCommands,
  ...settingsCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...libraryEvents,
  ...downloadEvents,
};

export type * from "./catalog";
export type * from "./core";
export type * from "./downloads";
export type * from "./library";
export type * from "./settings";
