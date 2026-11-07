// The daemon's pet: draws the current animation's strip frame by frame, in phase with every
// other client (frames are timed from `since_unix_ms`), scaled with nearest-neighbour.
// Clicking pats it. With reduced motion only the first frame is shown.

import { useEffect, useRef, useState } from "react";

import type { PetInfo, PetStateInfo, PetStripResult } from "@imperium/protocol";

import type { DaemonClient } from "../daemon/client";

const SIZE = 96;

function frameAt(state: PetStateInfo, now: number): number {
  const total = state.durations.reduce((sum, ms) => sum + ms, 0) || 1;
  let elapsed = Math.max(0, now - state.since_unix_ms);
  if (state.looping) elapsed %= total;
  for (let frame = 0; frame < state.durations.length; frame++) {
    elapsed -= state.durations[frame];
    if (elapsed < 0) return frame;
  }
  return state.durations.length - 1;
}

export function PetPanel({
  client,
  pet,
  state,
}: {
  client: DaemonClient | null;
  pet: PetInfo | null;
  state: PetStateInfo | null;
}) {
  const strips = useRef(new Map<string, string>());
  const [strip, setStrip] = useState<string | null>(null);
  const [frame, setFrame] = useState(0);
  const reducedMotion = typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  useEffect(() => {
    if (!client || !state) return;
    const cached = strips.current.get(state.animation);
    if (cached) {
      setStrip(cached);
      return;
    }
    client.call<PetStripResult>("pet.strip", { animation: state.animation }).then(
      ({ png_base64 }) => {
        const url = `data:image/png;base64,${png_base64}`;
        strips.current.set(state.animation, url);
        setStrip(url);
      },
      () => setStrip(null),
    );
  }, [client, state]);

  useEffect(() => {
    if (!state || reducedMotion) {
      setFrame(0);
      return;
    }
    const timer = window.setInterval(() => setFrame(frameAt(state, Date.now())), 50);
    return () => window.clearInterval(timer);
  }, [state, reducedMotion]);

  if (!pet || !state) {
    return <p className="empty">No pet. Put a Pixel Pets v3 pet in your config's pets folder.</p>;
  }
  if (state.hidden) return null;
  const scale = SIZE / Math.max(pet.cell_width, pet.cell_height);
  const width = pet.cell_width * scale;
  const height = pet.cell_height * scale;
  return (
    <div className="pet">
      <button
        className="pet-sprite"
        title={`${pet.display_name}: ${state.animation} (click to pat)`}
        aria-label={`${pet.display_name}, ${state.animation}`}
        onClick={() => client?.call("pet.interact", { kind: "pat" })}
        style={{
          width,
          height,
          backgroundImage: strip ? `url(${strip})` : undefined,
          backgroundSize: `${state.frames * width}px ${height}px`,
          backgroundPosition: `-${frame * width}px 0`,
        }}
      />
      <span className="muted">{state.animation}</span>
    </div>
  );
}
