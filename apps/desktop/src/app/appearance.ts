// Applies the appearance settings to <html>: data-theme (dark | light | high-contrast) and
// data-reduce-motion. "system" follows the OS color scheme and contrast preference live.
import { useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { type AppearanceSettings, commands, type Theme } from "../ipc";
import { queryKeys } from "../ipc/query";

export function resolveTheme(
  theme: Theme,
  prefersLight: boolean,
  prefersMoreContrast: boolean,
): string {
  switch (theme) {
    case "dark":
      return "dark";
    case "light":
      return "light";
    case "high_contrast":
      return "high-contrast";
    case "system":
      if (prefersMoreContrast) return "high-contrast";
      return prefersLight ? "light" : "dark";
  }
}

export function applyAppearance(settings: AppearanceSettings): () => void {
  const root = document.documentElement;
  root.dataset.reduceMotion = String(settings.reduce_motion);
  const light = window.matchMedia?.("(prefers-color-scheme: light)");
  const contrast = window.matchMedia?.("(prefers-contrast: more)");
  const update = () => {
    root.dataset.theme = resolveTheme(
      settings.theme,
      light?.matches ?? false,
      contrast?.matches ?? false,
    );
  };
  update();
  if (settings.theme !== "system") return () => {};
  light?.addEventListener("change", update);
  contrast?.addEventListener("change", update);
  return () => {
    light?.removeEventListener("change", update);
    contrast?.removeEventListener("change", update);
  };
}

export function useAppearanceQuery() {
  return useQuery({ queryKey: queryKeys.appearance, queryFn: () => commands.appearanceGet() });
}

export function AppearanceSync(): null {
  const { data } = useAppearanceQuery();
  useEffect(() => (data ? applyAppearance(data) : undefined), [data]);
  return null;
}
