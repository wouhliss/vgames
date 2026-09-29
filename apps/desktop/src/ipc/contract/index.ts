// Every command and event the UI needs that is not in the generated bindings yet, by domain.
import { catalogCommands } from "./catalog";
import { compatCommands } from "./compat";
import { coreCommands, coreEvents } from "./core";
import { downloadCommands, downloadEvents } from "./downloads";
import { libraryCommands, libraryEvents } from "./library";
import { settingsCommands } from "./settings";
import { socialCommands, socialEvents } from "./social";

export const pendingCommands = {
  ...coreCommands,
  ...libraryCommands,
  ...catalogCommands,
  ...downloadCommands,
  ...settingsCommands,
  ...compatCommands,
  ...socialCommands,
};

export const pendingEvents = {
  ...coreEvents,
  ...libraryEvents,
  ...downloadEvents,
  ...socialEvents,
};

export type * from "./catalog";
export type * from "./compat";
export type * from "./core";
export type * from "./downloads";
export type * from "./library";
export type * from "./settings";
export type * from "./social";
