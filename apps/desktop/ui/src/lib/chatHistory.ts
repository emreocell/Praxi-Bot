export type ExportFormat = "markdown" | "json";

/**
 * Fetch one page of hits. A page whose hits were all omitted by the daemon comes back empty
 * with a cursor; keep going (bounded) so the list never shows "0 matches" next to "More".
 */
export async function fetchHistoryPage<P extends { results: unknown[]; next_offset?: number | null }>(
  fetchPage: (offset: number) => Promise<P>,
  offset: number,
  maxEmptyPages = 5
): Promise<P> {
  let page = await fetchPage(offset);
  for (let i = 0; i < maxEmptyPages && page.results.length === 0 && page.next_offset != null; i++) {
    page = await fetchPage(page.next_offset);
  }
  return page;
}

/** Group ranked hits without discarding their order within a chat. */
export function groupHistoryHits<T extends { chat_id: string }>(hits: readonly T[]): T[][] {
  const groups = new Map<string, T[]>();
  for (const hit of hits) {
    const group = groups.get(hit.chat_id) ?? [];
    group.push(hit);
    groups.set(hit.chat_id, group);
  }
  return [...groups.values()];
}

function fold(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").replace(/ı/g, "i").toLowerCase();
}

/** Text nodes and <mark>, never HTML from the index. Preserve the original spelling. */
export function historySnippetParts(text: string, query: string): { text: string; match: boolean }[] {
  const needle = fold(query.trim());
  if (!needle) return [{ text, match: false }];
  const chars = [...text];
  const normalized = chars.map(fold);
  const haystack = normalized.join("");
  const start = haystack.indexOf(needle);
  if (start < 0) return [{ text, match: false }];
  let offset = 0;
  return chars.reduce<{ text: string; match: boolean }[]>((parts, char, i) => {
    const match = offset >= start && offset < start + needle.length;
    offset += normalized[i].length;
    if (parts.at(-1)?.match === match) parts[parts.length - 1].text += char;
    else parts.push({ text: char, match });
    return parts;
  }, []);
}

/** The daemon supplies redacted bytes; filenames contain no conversation content. */
export async function saveChatExport(
  client: { exportChat: (id: string, format: ExportFormat) => Promise<Blob> },
  chatId: string,
  format: ExportFormat
): Promise<void> {
  const blob = await client.exportChat(chatId, format);
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = `chat-${chatId}.${format === "json" ? "json" : "md"}`;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  // Give the webview time to consume the object URL before releasing it.
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
