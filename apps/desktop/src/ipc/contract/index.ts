// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { controllerCommands, controllerEvents } from "./controllers";
import { coreCommands, coreEvents } from "./core";
import { settingsCommands } from "./settings";

export const pendingCommands = {
  ...coreCommands,
  ...settingsCommands,
  ...controllerCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...controllerEvents,
};

export type * from "./controllers";
export type * from "./core";
export type * from "./settings";
