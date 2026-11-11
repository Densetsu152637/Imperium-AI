// The floating *Jump to latest* button over a transcript the reader has scrolled up in
// (roadmap item 20; see `useFollowScroll`). It marks new output with a count of new items, or a
// dot when only streamed text arrived.

import { ArrowDown } from "lucide-react";

import { jumpLabel } from "../follow";
import type { FollowScroll } from "../useFollowScroll";

export function JumpToLatest({ follow }: { follow: FollowScroll }) {
  if (!follow.showJump) return null;
  const { unseen, fresh } = follow;
  return (
    <div className="jump-latest-anchor">
      <button
        type="button"
        className="jump-latest"
        onClick={follow.jump}
        aria-label={jumpLabel(unseen, fresh)}
        title="Jump to latest and follow new output"
      >
        <ArrowDown aria-hidden="true" />
        <span>Jump to latest</span>
        {unseen > 0 ? (
          <span className="jump-latest-count" aria-hidden="true">
            {unseen > 99 ? "99+" : unseen}
          </span>
        ) : (
          fresh && <span className="jump-latest-dot" aria-hidden="true" />
        )}
      </button>
    </div>
  );
}
