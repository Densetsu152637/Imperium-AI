// Frame timing and sizing, pure. frameStops, phaseDelay and fitSize are ported from Pixel Pets
// (MIT; see ../NOTICE); frameAt is the nursery's own: one shared loop picks every pet's frame.

const round2 = (n: number): number => Math.round(n * 100) / 100;

/** The size the cell is drawn at: as large as fits in size x size, aspect ratio kept. */
export function fitSize(cell: { width: number; height: number }, size: number): { width: number; height: number } {
  const scale = size / Math.max(cell.width, cell.height);
  return { width: round2(cell.width * scale), height: round2(cell.height * scale) };
}

/** Keyframe stops: the percentage at which each frame starts, from cumulative durations. */
export function frameStops(durations: number[]): { total: number; stops: number[] } {
  const total = durations.reduce((a, b) => a + b, 0);
  let acc = 0;
  const stops = durations.map((d) => {
    const p = total > 0 ? Math.round((acc / total) * 1e4) / 1e2 : 0;
    acc += d;
    return p;
  });
  return { total, stops };
}

/** The playhead's position in ms: wraps for a loop, holds the end for a one-shot. */
export function phaseDelay(elapsed: number, total: number, loop: boolean): number {
  const e = Math.max(0, Math.floor(elapsed));
  if (total <= 0) return 0;
  return loop ? e % total : Math.min(e, total);
}

/** The frame showing `elapsed` ms into an animation: a loop wraps, a one-shot holds its last frame. */
export function frameAt(durations: number[], elapsed: number, loop: boolean): number {
  if (durations.length === 0) return 0;
  const { total } = frameStops(durations);
  const t = phaseDelay(elapsed, total, loop);
  let acc = 0;
  for (let i = 0; i < durations.length; i++) {
    acc += durations[i] ?? 0;
    if (t < acc) return i;
  }
  return durations.length - 1;
}
