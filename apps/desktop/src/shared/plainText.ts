// Text from outside the app (a repository, a marketplace, GitHub) cleaned for display: the views
// draw it only as text, never as markup.

// Control, bidirectional-override and zero-width characters can hide or reorder text.
// eslint-disable-next-line no-control-regex
const HIDDEN = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u00ad\u061c\u200b-\u200f\u2028\u2029\u202a-\u202e\u2060-\u2064\u2066-\u2069\u206a-\u206f\ufeff]/g;

/** Repository-, marketplace- or GitHub-supplied text with control and direction-changing characters removed. */
export function plain(text: string | null | undefined): string {
  return (text ?? "").replace(HIDDEN, "").trim();
}

export const message = (error: unknown) => plain(error instanceof Error ? error.message : String(error));
