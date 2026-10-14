// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { controllerCommands } from "./controllers";
import { coreCommands } from "./core";

export const pendingCommands = {
  ...coreCommands,
  ...controllerCommands,
};

export const pendingEvents = {};

export type * from "./controllers";
export type * from "./core";
