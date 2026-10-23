// Every declared router path has a real browser scan. The package id is resolved from its fixture.
export const ROUTE_SCANS = [
  { pattern: "/", path: "/", preset: "ready", heading: "Library" },
  { pattern: "/*", path: "/quality-unknown-route", preset: "ready", heading: "Library" },
  { pattern: "/dev/gallery", path: "/dev/gallery", preset: "ready", heading: "Component gallery" },
  { pattern: "/onboarding", path: "/onboarding", preset: "fresh", heading: "Welcome to vgames" },
  { pattern: "/library", path: "/library", preset: "ready", heading: "Library" },
  { pattern: "/browse", path: "/browse", preset: "ready", heading: "Browse" },
  { pattern: "/package/:packageId", path: "/package/:packageId", preset: "ready", heading: "" },
  { pattern: "/friends/*", path: "/friends", preset: "ready", heading: "Friends" },
  { pattern: "/downloads", path: "/downloads", preset: "ready", heading: "Downloads" },
  { pattern: "/publish", path: "/publish", preset: "admin", heading: "Publish" },
  { pattern: "/settings/*", path: "/settings/general", preset: "ready", heading: "Settings" },
] as const;
