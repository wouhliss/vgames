// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { catalogCommands } from "./catalog";
import { compatCommands } from "./compat";
import { controllerCommands, controllerEvents } from "./controllers";
import { coreCommands, coreEvents } from "./core";
import { downloadCommands, downloadEvents } from "./downloads";
import { libraryCommands, libraryEvents } from "./library";
import { savesCommands, savesEvents } from "./saves";
import { settingsCommands } from "./settings";

export const pendingCommands = {
  ...coreCommands,
  ...libraryCommands,
  ...catalogCommands,
  ...downloadCommands,
  ...settingsCommands,
  ...compatCommands,
  ...savesCommands,
  ...controllerCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...libraryEvents,
  ...downloadEvents,
  ...savesEvents,
  ...controllerEvents,
};

export type * from "./catalog";
export type * from "./compat";
export type * from "./controllers";
export type * from "./core";
export type * from "./downloads";
export type * from "./library";
export type * from "./saves";
export type * from "./settings";
