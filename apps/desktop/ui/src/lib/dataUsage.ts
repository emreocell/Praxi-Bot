/** Sizes shown in Settings > Data: "0 B", "12 KB", "1.4 MB", "2.3 GB". */
export function formatSize(n: number): string {
  if (n < 1024) {
    return `${n} B`;
  }
  if (n < 1024 * 1024) {
    return `${Math.round(n / 1024)} KB`;
  }
  if (n < 1024 * 1024 * 1024) {
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/** Category ids the daemon reports, in display order; each has `settingsModal.data.category.<id>`. */
export const DATA_CATEGORY_IDS = ["chats", "memory", "knowledge", "backups", "caches", "other"] as const;
