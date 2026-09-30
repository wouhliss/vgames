// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { catalogCommands } from "./catalog";
import { compatCommands } from "./compat";
import { controllerCommands, controllerEvents } from "./controllers";
import { coreCommands, coreEvents } from "./core";
import { downloadCommands, downloadEvents } from "./downloads";
import { libraryCommands, libraryEvents } from "./library";
import { publishCommands, publishEvents } from "./publish";
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
  ...publishCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...libraryEvents,
  ...downloadEvents,
  ...savesEvents,
  ...controllerEvents,
  ...publishEvents,
};

export type * from "./catalog";
export type * from "./compat";
export type * from "./controllers";
export type * from "./core";
export type * from "./downloads";
export type * from "./library";
export type * from "./publish";
export type * from "./saves";
export type * from "./settings";
