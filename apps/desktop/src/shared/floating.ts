// Where a floating thing (a select's list, a menu, a tooltip) goes relative to the element that opened
// it. Pure functions over rectangles, so the rules are testable and every popover places itself the
// same way: below its anchor, flipped above when there is not enough room below and more room above.

export type Box = { left: number; top: number; right: number; bottom: number; width: number; height: number };
export type Viewport = { width: number; height: number };

/** Room kept clear between a popover and the window's edge. */
const MARGIN = 8;
/** The gap between the anchor and the popover. */
const GAP = 4;

export type ListPlacement = {
  /** The anchor's left edge, for a popover that starts under it. */
  left: number;
  /** Distance from the window's right edge to the anchor's right edge, for a popover that ends under it. */
  right: number;
  /** `position: fixed` top: just under the anchor, or just over it (with `above`, the popover then shifts up by its own height). */
  top: number;
  width: number;
  /** The most height the popover may take before it scrolls. */
  maxHeight: number;
  above: boolean;
};

/**
 * A list that opens under (or over) its anchor. `needed` is the height it would like: when there is
 * less than that below the anchor and more room above, it opens upward. `maxHeight` caps it and
 * `minHeight` keeps it usable in a cramped window.
 */
export function placeList(anchor: Box, viewport: Viewport, options: { needed: number; maxHeight?: number; minHeight?: number }): ListPlacement {
  const below = viewport.height - anchor.bottom - MARGIN;
  const aboveRoom = anchor.top - MARGIN;
  const above = below < options.needed && aboveRoom > below;
  const room = above ? aboveRoom : below;
  return {
    left: anchor.left,
    right: viewport.width - anchor.right,
    top: above ? anchor.top - GAP : anchor.bottom + GAP,
    width: anchor.width,
    maxHeight: Math.max(Math.min(room, options.maxHeight ?? Number.POSITIVE_INFINITY), options.minHeight ?? 0),
    above,
  };
}

export type TooltipPlacement = { left: number; top: number; above: boolean };

/** A tooltip centred on its anchor, over it when it fits and under it otherwise, kept inside the window. */
export function placeTooltip(anchor: Box, size: { width: number; height: number }, viewport: Viewport, gap = 6): TooltipPlacement {
  const above = anchor.top - gap - size.height >= MARGIN || viewport.height - anchor.bottom - gap - size.height < MARGIN;
  const centre = anchor.left + anchor.width / 2 - size.width / 2;
  const left = Math.min(Math.max(centre, MARGIN), Math.max(viewport.width - size.width - MARGIN, MARGIN));
  return { left, top: above ? anchor.top - gap - size.height : anchor.bottom + gap, above };
}
