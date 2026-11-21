/**
 * Sentence chunking for the call's streamed read-aloud: speech starts at the first complete
 * sentence instead of waiting for the whole reply. Pure.
 */

const END = /[.!?…。！？]+["')\]]*(\s+|$)|\n{1,}/g;

/** Longest piece spoken at once when the text has no sentence end (a long list, a stream of words). */
const MAX_CHUNK = 220;

/**
 * The next speakable chunks of `text` beyond the first `spokenLen` characters. Only complete
 * sentences are returned while the reply still streams; with `final` the rest is flushed.
 * `end` is the new spoken offset to pass in next time.
 */
export function takeSpeakable(text: string, spokenLen: number, final: boolean): { chunks: string[]; end: number } {
  const chunks: string[] = [];
  let pos = spokenLen;
  const rest = text.slice(pos);
  let last = 0;
  END.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = END.exec(rest)) !== null) {
    // A "3.14" or "e.g. x" style dot has no space after it and is not a sentence end; the regex
    // requires whitespace or the text end, so a trailing "." at the very end of a still-streaming
    // text only counts once more text follows (or on final).
    const atTextEnd = m.index + m[0].length === rest.length;
    if (atTextEnd && !final && !/\s$/.test(m[0]) && !/\n/.test(m[0])) {
      break;
    }
    const piece = rest.slice(last, m.index + m[0].length).trim();
    if (piece) {
      chunks.push(piece);
    }
    last = m.index + m[0].length;
  }
  let tail = rest.slice(last);
  // A very long unbroken run is cut at the last space so speech does not stall.
  while (tail.length > MAX_CHUNK) {
    const cut = tail.lastIndexOf(" ", MAX_CHUNK);
    const at = cut > 40 ? cut : MAX_CHUNK;
    const piece = tail.slice(0, at).trim();
    if (piece) {
      chunks.push(piece);
    }
    last += at;
    tail = tail.slice(at);
  }
  if (final && tail.trim()) {
    chunks.push(tail.trim());
    last += tail.length;
  }
  pos += last;
  return { chunks, end: pos };
}
