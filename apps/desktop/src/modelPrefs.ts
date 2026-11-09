// Favourite and hidden models: the user's marks on models, keyed by the spec a session uses
// (`<runtime>:<id>` for a local model, the table row's spec for a hosted one). Favourites come
// first everywhere; hidden models sink to the end of the management lists and are left out of
// the lists sessions choose from. Kept in the app's settings document (settings.ts), so they
// survive updates and reinstalls. Pure, so the rules are tested without a window
// (test/modelPrefs.test.ts); ModelSelect, the model panels and the command palette draw them.

/** A model's mark: a favourite, or hidden from the choosers. Unmarked models have no entry. */
export type ModelMark = "favourite" | "hidden";

/** Marks by model spec. */
export type ModelMarks = Record<string, ModelMark>;

/** The mark on `spec`, or `null`. */
export const markOf = (marks: ModelMarks | undefined, spec: string): ModelMark | null => marks?.[spec] ?? null;

/** `marks` with `spec` set to `mark`, or unmarked when `mark` is `null`. */
export function withMark(marks: ModelMarks | undefined, spec: string, mark: ModelMark | null): ModelMarks {
  const { [spec]: _old, ...rest } = marks ?? {};
  return mark ? { ...rest, [spec]: mark } : rest;
}

/** `marks` with `mark` on `spec` switched: set when it was not there, cleared when it was. */
export const toggleMark = (marks: ModelMarks | undefined, spec: string, mark: ModelMark): ModelMarks =>
  withMark(marks, spec, markOf(marks, spec) === mark ? null : mark);

/** A management list in three parts, each in the order given: favourites, the rest, hidden. */
export type Arranged<T> = { favourites: T[]; normal: T[]; hidden: T[] };

/** Splits `items` for a management list ([`Arranged`]), keeping their order within each part. */
export function arrangeModels<T>(items: readonly T[], specOf: (item: T) => string, marks: ModelMarks | undefined): Arranged<T> {
  const arranged: Arranged<T> = { favourites: [], normal: [], hidden: [] };
  for (const item of items) {
    const mark = markOf(marks, specOf(item));
    (mark === "favourite" ? arranged.favourites : mark === "hidden" ? arranged.hidden : arranged.normal).push(item);
  }
  return arranged;
}

/** Which part of a list a model is in. */
export type ModelGroup = "favourite" | "normal" | "hidden";

/**
 * The models to choose from for a session, in order: favourites first, then the rest. Hidden
 * models are left out, except `current` — the session's model stays listed (last, in the
 * `hidden` group) so the selection still shows what it is.
 */
export function chooserModels<T>(
  items: readonly T[],
  specOf: (item: T) => string,
  marks: ModelMarks | undefined,
  current: string | null = null,
): { item: T; group: ModelGroup }[] {
  const { favourites, normal, hidden } = arrangeModels(items, specOf, marks);
  return [
    ...favourites.map((item) => ({ item, group: "favourite" as const })),
    ...normal.map((item) => ({ item, group: "normal" as const })),
    ...hidden.filter((item) => current !== null && specOf(item) === current).map((item) => ({ item, group: "hidden" as const })),
  ];
}
