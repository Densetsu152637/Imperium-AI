// The window's width in layout pixels (window pixels over the UI scale, the unit `--px` stands for),
// kept current as the window is resized: what fold.ts decides on.

import { useEffect, useState } from "react";

import { knownWidth } from "./fold";

/** The width the page lays out in: the root element's, which follows the window (and an emulated viewport) without a lag. */
const windowWidth = (): number => document.documentElement.clientWidth || window.innerWidth;

export function useLayoutWidth(uiScale: number): number {
  // A window that is minimised or hidden reads 0: that is no reading, the last width stays (none yet: unknown, shown whole).
  const [width, setWidth] = useState(() => knownWidth(windowWidth(), Number.NaN));
  useEffect(() => {
    const update = () => setWidth((previous) => knownWidth(windowWidth(), previous));
    update();
    window.addEventListener("resize", update);
    // An emulated or embedded viewport can change size without a resize event; the root's box follows it.
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update);
    observer?.observe(document.documentElement);
    return () => {
      window.removeEventListener("resize", update);
      observer?.disconnect();
    };
  }, []);
  return width / (uiScale > 0 ? uiScale : 1);
}
