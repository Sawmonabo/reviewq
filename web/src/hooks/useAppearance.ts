import { useEffect, useLayoutEffect, useState } from "react";
import { savedValue } from "../lib/view-preferences";

export type Appearance = {
  readonly palette: "blue" | "violet";
  readonly mode: "system" | "light" | "dark";
  readonly spacing: "regular" | "compact";
};

const STORAGE_KEY = "reviewq.v1.appearance";
const COLOR_QUERY = "(prefers-color-scheme: dark)";

export function useAppearance() {
  const [appearance, setAppearance] = useState(readAppearance);

  useEffect(() => {
    function refresh(event: StorageEvent) {
      if (event.key === STORAGE_KEY || event.key === null) setAppearance(readAppearance());
    }
    window.addEventListener("storage", refresh);
    return () => window.removeEventListener("storage", refresh);
  }, []);

  useLayoutEffect(() => {
    const system = window.matchMedia(COLOR_QUERY);
    function apply() {
      const { dataset } = document.documentElement;
      dataset.palette = appearance.palette;
      dataset.theme =
        appearance.mode === "system" ? (system.matches ? "dark" : "light") : appearance.mode;
      dataset.density = appearance.spacing;
    }
    apply();
    if (appearance.mode === "system") system.addEventListener("change", apply);
    return () => system.removeEventListener("change", apply);
  }, [appearance]);

  return {
    appearance,
    change: (next: Appearance) => {
      setAppearance(next);
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
      } catch {
        // Keep the chosen appearance for the current page when storage is disabled.
      }
    },
  };
}

function readAppearance(): Appearance {
  const legacy: Appearance = {
    palette: "blue",
    mode: savedValue("theme") === "light" ? "light" : "dark",
    spacing: savedValue("compact") === true ? "compact" : "regular",
  };

  try {
    const saved: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null");
    if (!saved || typeof saved !== "object") return legacy;
    return {
      palette: "palette" in saved && saved.palette === "violet" ? "violet" : "blue",
      mode:
        "mode" in saved &&
        (saved.mode === "system" || saved.mode === "light" || saved.mode === "dark")
          ? saved.mode
          : legacy.mode,
      spacing: "spacing" in saved && saved.spacing === "compact" ? "compact" : "regular",
    };
  } catch {
    return legacy;
  }
}
