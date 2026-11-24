/**
 * A fresh host (or install) can carry an internal, validation-only provider with a stand-in model.
 * It cannot answer anything, so a person must never see it as a model: the app treats "only the
 * placeholder exists" as "no model connected yet" and offers to connect a real one.
 */
const PLACEHOLDER = /validation[\s_-]*(only|placeholder)/i;

/** True for the internal validation-only provider id or display name (`validation-only`). */
export function isPlaceholderProvider(id: string | null | undefined): boolean {
  return PLACEHOLDER.test(id ?? "");
}

/** True for a model id or a `provider/model` ref that belongs to the placeholder. */
export function isPlaceholderModel(ref: string | null | undefined): boolean {
  return PLACEHOLDER.test(ref ?? "");
}
