// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { catalogCommands } from "./catalog";
import { compatCommands } from "./compat";
import { coreCommands, coreEvents } from "./core";
import { settingsCommands } from "./settings";

export const pendingCommands = {
  ...coreCommands,
  ...catalogCommands,
  ...settingsCommands,
  ...compatCommands,
};

export const pendingEvents = {
  ...coreEvents,
};

export type * from "./catalog";
export type * from "./compat";
export type * from "./core";
export type * from "./settings";
