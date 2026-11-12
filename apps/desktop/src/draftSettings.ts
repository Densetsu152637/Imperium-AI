// The speculative decoding fields of the model settings form, checked before they are sent
// (the daemon is authoritative). Pure, so the rules are tested without a window
// (test/draftSettings.test.ts).

/** llama.cpp's own default for the draft's max tokens per step (b12180), when none is set. */
export const DEFAULT_DRAFT_MAX = 3;

export type DraftFields = {
  model: string | null;
  max: number | null;
  min: number | null;
  gpuLayers: number | null;
};

/**
 * Throws the first problem with the draft fields of the model `id`: parameters without a draft,
 * a model drafting for itself, or a minimum above the maximum (`effectiveMax` is the maximum in
 * effect when none is set, llama.cpp's default unless the daemon says otherwise).
 */
export function checkDraft(id: string, fields: DraftFields, effectiveMax: number | null = null): void {
  if (fields.model === null) {
    if (fields.max !== null || fields.min !== null || fields.gpuLayers !== null) {
      throw new Error("The draft settings need a draft model");
    }
    return;
  }
  if (fields.model === id) throw new Error("A model cannot draft for itself");
  const max = fields.max ?? effectiveMax ?? DEFAULT_DRAFT_MAX;
  if (fields.min !== null && fields.min > max) {
    throw new Error(
      `Draft min tokens (${fields.min}) may not exceed draft max tokens (${max}${fields.max === null ? ", the default" : ""})`,
    );
  }
}
