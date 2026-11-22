import { useCallback, useEffect, useRef, useState } from "react";
import { Brain } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { MemoryItem, MemoryKind, MemoryPreview } from "../../api/types";
import form from "../../components/Form.module.css";
import { EmptyState } from "../../components/EmptyState";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./KnowledgeCard.module.css";

const KINDS: readonly MemoryKind[] = ["profile", "bot", "episodic", "procedural", "lesson"];
const message = (error: unknown) => humanLine(error);

/** Per-bot and shared personal memory inspector: view, search, add, edit, forget, undo, and preview recall. */
export function MemoryCard() {
  const { t, i18n } = useTranslation();
  const { client, bots } = useDaemon();
  const [bot, setBot] = useState("");
  const activeBot = bot || bots[0]?.id || "";
  const [botOnly, setBotOnly] = useState(false);
  const [kindFilter, setKindFilter] = useState<MemoryKind | "">("");
  const [search, setSearch] = useState("");
  const [includeDeleted, setIncludeDeleted] = useState(false);
  const [items, setItems] = useState<MemoryItem[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [lastUndoId, setLastUndoId] = useState<string | null>(null);

  const [addScope, setAddScope] = useState<"bot" | "shared">("bot");
  const [addKind, setAddKind] = useState<MemoryKind>("bot");
  const [addContent, setAddContent] = useState("");

  const [editing, setEditing] = useState<MemoryItem | null>(null);
  const [editText, setEditText] = useState("");

  const [previewQuery, setPreviewQuery] = useState("");
  const [preview, setPreview] = useState<MemoryPreview | null>(null);

  const epoch = useRef(0);

  const reload = useCallback(async () => {
    if (!client || !activeBot) return;
    const current = ++epoch.current;
    const list = await client.listMemories({
      bot: activeBot,
      botOnly,
      kind: kindFilter || null,
      q: search,
      includeDeleted
    });
    if (current !== epoch.current) return;
    setItems(list);
  }, [client, activeBot, botOnly, kindFilter, search, includeDeleted]);

  useEffect(() => {
    setError(null);
    void reload().catch((e: unknown) => setError(message(e)));
    return () => {
      epoch.current += 1;
    };
  }, [reload]);

  const act = async (operation: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    setNotice("");
    setLastUndoId(null);
    try {
      await operation();
      await reload();
    } catch (e) {
      setError(message(e));
      await reload().catch(() => undefined);
    } finally {
      setBusy(false);
    }
  };

  const add = () =>
    act(async () => {
      if (!client || !addContent.trim()) return;
      await client.createMemory({
        bot_id: addScope === "shared" ? null : activeBot,
        kind: addKind,
        content: addContent.trim()
      });
      setAddContent("");
      setNotice(t("memory.addedNotice"));
    });

  const saveEdit = () =>
    act(async () => {
      if (!client || !editing || !editText.trim()) return;
      await client.updateMemory(editing.id, { content: editText.trim() });
      setEditing(null);
      setNotice(t("memory.updatedNotice"));
    });

  const forget = (item: MemoryItem) =>
    act(async () => {
      if (!client) return;
      await client.deleteMemory(item.id);
      setNotice(t("memory.forgottenNotice"));
      setLastUndoId(item.id);
    });

  const undo = (memoryId: string) =>
    act(async () => {
      if (!client) return;
      await client.undoMemory(memoryId);
      setNotice(t("memory.undoneNotice"));
    });

  const runPreview = () =>
    act(async () => {
      if (!client || !activeBot) return;
      setPreview(await client.previewBotMemory(activeBot, previewQuery));
    });

  const botName = (id: string | null | undefined) =>
    id ? (bots.find((b) => b.id === id)?.name ?? id) : t("memory.scopeShared");

  return (
    <Card padding="md" className={styles.root} data-testid="memory-card">
      <h3 className={styles.heading}>{t("memory.title")}</h3>
      <p className={form.hint}>{t("memory.description")}</p>
      <label className={form.field}>
        {t("memory.bot")}
        <select
          className={form.select}
          value={activeBot}
          disabled={busy}
          onChange={(e) => {
            setBot(e.target.value);
            setPreview(null);
          }}
        >
          {bots.map((item) => (
            <option key={item.id} value={item.id}>
              {item.name}
            </option>
          ))}
        </select>
      </label>
      {!activeBot ? (
        <p className={form.hint}>{t("memory.noBot")}</p>
      ) : (
        <>
          <div className={form.row}>
            <label className={`${form.field} ${form.grow}`}>
              {t("memory.search")}
              <input
                className={form.input}
                type="search"
                value={search}
                placeholder={t("memory.searchPlaceholder")}
                onChange={(e) => setSearch(e.target.value)}
              />
            </label>
            <label className={form.field}>
              {t("memory.kindFilter")}
              <select
                className={form.select}
                value={kindFilter}
                onChange={(e) => setKindFilter(e.target.value as MemoryKind | "")}
              >
                <option value="">{t("memory.allKinds")}</option>
                {KINDS.map((k) => (
                  <option key={k} value={k}>
                    {t(`memory.kinds.${k}`)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <div className={styles.actions}>
            <label className={form.hint}>
              <input
                type="checkbox"
                checked={botOnly}
                onChange={(e) => setBotOnly(e.target.checked)}
              />{" "}
              {t("memory.botOnly")}
            </label>
            <label className={form.hint}>
              <input
                type="checkbox"
                checked={includeDeleted}
                onChange={(e) => setIncludeDeleted(e.target.checked)}
              />{" "}
              {t("memory.includeForgotten")}
            </label>
          </div>
          {error ? (
            <p role="alert" className={form.error}>
              {error}
            </p>
          ) : null}
          {notice ? (
            <div className={styles.actions} role="status">
              <span className={form.success}>{notice}</span>
              {lastUndoId ? (
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={busy}
                  onClick={() => void undo(lastUndoId)}
                >
                  {t("memory.undo")}
                </Button>
              ) : null}
            </div>
          ) : null}
          <ul className={styles.list}>
            {items.map((item) => (
              <li key={item.id} className={styles.source} data-testid="memory-item">
                <strong>{item.content}</strong>
                <span className={form.hint}>
                  {t(`memory.kinds.${item.kind}`)} · {botName(item.bot_id)} ·{" "}
                  {t(`memory.trust.${item.trust}`)} ·{" "}
                  {new Date(item.created_ms).toLocaleDateString(i18n.language)}
                  {item.deleted ? ` · ${t("memory.forgottenBadge")}` : ""}
                </span>
                {item.source_quote ? (
                  <code className={styles.locator}>“{item.source_quote}”</code>
                ) : null}
                <div className={styles.actions}>
                  {!item.deleted ? (
                    <>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy}
                        onClick={() => {
                          setEditing(item);
                          setEditText(item.content);
                        }}
                      >
                        {t("memory.edit")}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy}
                        onClick={() => void forget(item)}
                      >
                        {t("memory.forget")}
                      </Button>
                    </>
                  ) : null}
                  {item.undo_journal_id !== null && item.undo_journal_id !== undefined ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={busy}
                      onClick={() => void undo(item.id)}
                    >
                      {t("memory.undo")}
                    </Button>
                  ) : null}
                </div>
              </li>
            ))}
          </ul>
          {items.length === 0 ? <EmptyState icon={Brain} title={t("memory.emptyTitle")}>{t("memory.empty")}</EmptyState> : null}
          {editing ? (
            <section className={styles.review} aria-label={t("memory.edit")}>
              <label className={form.field}>
                {t("memory.editLabel")}
                <textarea
                  className={form.textarea}
                  value={editText}
                  maxLength={8192}
                  onChange={(e) => setEditText(e.target.value)}
                />
              </label>
              <div className={styles.actions}>
                <Button disabled={busy || !editText.trim()} onClick={() => void saveEdit()}>
                  {t("memory.saveEdit")}
                </Button>
                <Button variant="secondary" disabled={busy} onClick={() => setEditing(null)}>
                  {t("common.cancel")}
                </Button>
              </div>
            </section>
          ) : null}
          <details className={styles.group}>
            <summary>{t("memory.addTitle")}</summary>
            <div className={styles.form}>
              <label className={form.field}>
                {t("memory.scope")}
                <select
                  className={form.select}
                  value={addScope}
                  onChange={(e) => setAddScope(e.target.value as "bot" | "shared")}
                >
                  <option value="bot">
                    {t("memory.scopeThisBot", { bot: botName(activeBot) })}
                  </option>
                  <option value="shared">{t("memory.scopeShared")}</option>
                </select>
              </label>
              <label className={form.field}>
                {t("memory.kind")}
                <select
                  className={form.select}
                  value={addKind}
                  onChange={(e) => setAddKind(e.target.value as MemoryKind)}
                >
                  {KINDS.map((k) => (
                    <option key={k} value={k}>
                      {t(`memory.kinds.${k}`)}
                    </option>
                  ))}
                </select>
              </label>
              <label className={form.field}>
                {t("memory.content")}
                <textarea
                  className={form.textarea}
                  value={addContent}
                  placeholder={t("memory.contentPlaceholder")}
                  maxLength={8192}
                  onChange={(e) => setAddContent(e.target.value)}
                />
              </label>
              <Button disabled={busy || !addContent.trim()} onClick={() => void add()}>
                {t("memory.add")}
              </Button>
            </div>
          </details>
          <details className={styles.group}>
            <summary>{t("memory.previewTitle")}</summary>
            <div className={styles.form}>
              <p className={form.hint}>{t("memory.previewHint")}</p>
              <label className={form.field}>
                {t("memory.previewQuery")}
                <input
                  className={form.input}
                  value={previewQuery}
                  placeholder={t("memory.previewPlaceholder")}
                  onChange={(e) => setPreviewQuery(e.target.value)}
                />
              </label>
              <Button
                variant="secondary"
                disabled={busy || !previewQuery.trim()}
                onClick={() => void runPreview()}
              >
                {t("memory.previewRun")}
              </Button>
              {preview ? (
                <>
                  <p className={form.hint}>
                    {t("memory.previewTokens", {
                      used: preview.estimated_tokens,
                      budget: preview.budget_tokens,
                      count: preview.hits.length
                    })}
                  </p>
                  {preview.hits.length === 0 ? (
                    <p className={form.hint}>{t("memory.previewEmpty")}</p>
                  ) : (
                    <pre className={styles.locator}>{preview.rendered_block}</pre>
                  )}
                </>
              ) : null}
            </div>
          </details>
        </>
      )}
    </Card>
  );
}
