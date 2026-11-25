// Picking a pet up, pure: a press becomes a drag once the pointer has moved a little (so a plain
// click still pats), the pet keeps the offset it was grabbed at, and its feet stay inside the room.

import { clampTo } from "./space.ts";
import type { Bounds, Pos } from "./space.ts";

/** How far (px) the pointer must move after the press before it counts as picking the pet up. */
export const DRAG_THRESHOLD = 4;
/** How far an arrow key nudges a pet, in px (with shift, a larger step). */
export const NUDGE = 12;
export const NUDGE_FAR = 48;

export type Drag = {
  /** Where the press happened, in room coordinates. */
  start: Pos;
  /** From the pointer to the pet's feet when it was grabbed. */
  offset: Pos;
  /** True once the pointer has moved past the threshold. */
  lifted: boolean;
};

/** A press at `pointer` on a pet whose feet are at `feet` (both in room coordinates). */
export const grab = (pointer: Pos, feet: Pos): Drag => ({ start: pointer, offset: { x: feet.x - pointer.x, y: feet.y - pointer.y }, lifted: false });

/** The drag after the pointer moved to `pointer`, and where the pet's feet are now (null until lifted). */
export function follow(drag: Drag, pointer: Pos, b: Bounds, threshold = DRAG_THRESHOLD): { drag: Drag; feet: Pos | null } {
  const lifted = drag.lifted || Math.hypot(pointer.x - drag.start.x, pointer.y - drag.start.y) >= threshold;
  const next = lifted === drag.lifted ? drag : { ...drag, lifted };
  return { drag: next, feet: lifted ? clampTo(b, { x: pointer.x + drag.offset.x, y: pointer.y + drag.offset.y }) : null };
}

/** The pointer in room coordinates, given the room's box on screen. */
export const inRoom = (client: Pos, box: { left: number; top: number }): Pos => ({ x: client.x - box.left, y: client.y - box.top });

/** The feet after an arrow key (null for any other key), kept inside the room. */
export function nudge(key: string, feet: Pos, b: Bounds, far = false): Pos | null {
  const step = far ? NUDGE_FAR : NUDGE;
  const d = key === "ArrowLeft" ? [-step, 0] : key === "ArrowRight" ? [step, 0] : key === "ArrowUp" ? [0, -step] : key === "ArrowDown" ? [0, step] : null;
  return d ? clampTo(b, { x: feet.x + d[0]!, y: feet.y + d[1]! }) : null;
}
