// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { catalogCommands } from "./catalog";
import { compatCommands } from "./compat";
import { controllerCommands, controllerEvents } from "./controllers";
import { coreCommands, coreEvents } from "./core";
import { savesCommands, savesEvents } from "./saves";
import { settingsCommands } from "./settings";

export const pendingCommands = {
  ...coreCommands,
  ...catalogCommands,
  ...settingsCommands,
  ...compatCommands,
  ...savesCommands,
  ...controllerCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...controllerEvents,
  ...savesEvents,
};

export type * from "./catalog";
export type * from "./compat";
export type * from "./controllers";
export type * from "./core";
export type * from "./saves";
export type * from "./settings";
