// Reviewer detection, ported from Pixel Pets (MIT; see ../NOTICE), pure. A subagent whose brief
// is tagged with a reviewing profile draws `review` instead of `working`.

/** Profiles whose work is reviewing finished output. */
export const REVIEW_PROFILES = ["critic", "validator", "inspector", "auditor"];

// The brief's tag line, e.g. `Role: leaf; Profile: critic`: the first match counts.
const PROFILE_TAG = /\bProfile:\s*([a-z-]+)/i;

/** True when a subagent is a reviewer: its type or the brief's `Profile:` tag names one. */
export function isReviewer(subagentType: string, prompt: string): boolean {
  const bare = subagentType.replace(/^[^:]*:/, "").trim().toLowerCase();
  if (REVIEW_PROFILES.includes(bare)) return true;
  const tag = PROFILE_TAG.exec(prompt)?.[1]?.toLowerCase() ?? "";
  return REVIEW_PROFILES.includes(tag);
}
