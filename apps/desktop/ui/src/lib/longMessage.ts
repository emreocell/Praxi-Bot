/** A message longer than this many characters or lines is folded in the list until opened. */
export const LONG_MESSAGE_CHARS = 900;
export const LONG_MESSAGE_LINES = 14;

/** Should a user's message start collapsed? Pure: the view only decides how to draw it. */
export function isLongMessage(text: string): boolean {
  if (text.length > LONG_MESSAGE_CHARS) {
    return true;
  }
  let lines = 1;
  for (let i = 0; i < text.length; i += 1) {
    if (text.charCodeAt(i) === 10 && (lines += 1) > LONG_MESSAGE_LINES) {
      return true;
    }
  }
  return false;
}
