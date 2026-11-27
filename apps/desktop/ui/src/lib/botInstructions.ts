/** Example instructions offered under the "What it should do" box (texts in `botEdit.instructions.examples`). */
export const INSTRUCTION_EXAMPLES = ["research", "inbox", "coding", "writing"] as const;

/** An example fills an empty box; otherwise it goes below what is written, after a blank line. */
export function appendInstruction(current: string, example: string): string {
  const head = current.trimEnd();
  return head ? `${head}\n\n${example}` : example;
}
