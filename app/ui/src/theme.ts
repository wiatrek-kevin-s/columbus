// Light or dark, and the colours the session's layers take on the map.
//
// The interface is black and white; colour belongs to the data. The active
// output is drawn in the "hit" colour; every other output rests in a quiet
// colour that stays its own, by order of appearance.

import { useEffect, useState } from "react";
import type { Theme } from "./basemap";

export type { Theme };

export interface LayerTones {
  /** The active output. */
  hit: string;
  /** The others, by order of appearance, round and round. */
  rest: readonly string[];
  /** Around a point, so it reads on anything under it. */
  ring: string;
  /** The measured line. */
  ink: string;
  panel: string;
}

/** `hit`, `ink` and `panel` are the page's tokens in styles.css
 * (`e2e/unit.mjs` checks they agree); the dark resting colours are the
 * light ones, lifted to read on the dark paper. */
export const LAYER_TONES: Record<Theme, LayerTones> = {
  light: {
    hit: "#ef4b23",
    rest: ["#1b1c1f", "#5b6b8c", "#7a6a4f", "#4f7a6a", "#8c5b7a", "#6b6c70"],
    ring: "#ffffff",
    ink: "#17181a",
    panel: "#ffffff",
  },
  dark: {
    hit: "#ff5b34",
    rest: ["#f1efea", "#9db0d6", "#c9b48f", "#93c4b0", "#d3a1c1", "#b4b5b8"],
    ring: "#121315",
    ink: "#f3f2ef",
    panel: "#1c1d20",
  },
};

/** The theme in force: the one the Columbus app chose for this page
 * (`<html data-theme>`, set from its message), or else the system's. */
function themeNow(): Theme {
  const chosen = document.documentElement.dataset.theme;
  if (chosen === "light" || chosen === "dark") return chosen;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/** The theme, kept in step with the system and with the app's choice, for
 * what CSS cannot colour: the map. */
export function useTheme(): Theme {
  const [theme, setTheme] = useState<Theme>(themeNow);
  useEffect(() => {
    const update = () => setTheme(themeNow());
    const system = window.matchMedia("(prefers-color-scheme: dark)");
    system.addEventListener("change", update);
    const chosen = new MutationObserver(update);
    chosen.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    update();
    return () => {
      system.removeEventListener("change", update);
      chosen.disconnect();
    };
  }, []);
  return theme;
}
