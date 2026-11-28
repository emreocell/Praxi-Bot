import {
  Check,
  Download,
  Eye,
  File as FileIcon,
  FileText,
  Folder,
  FolderOpen,
  Image as ImageIcon,
  Pencil,
  RotateCcw,
  Trash2,
  X
} from "lucide-react";
import { type KeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { isTauriRuntime, openWorkspaceFolder } from "../api/connection";
import type { BotWorkspaceListDto, WorkspaceEntryDto, WorkspacesOverviewDto } from "../api/types";
import { humanLine } from "../lib/humanError";
import { formatBytes, workspaceBreadcrumbs } from "../lib/workspaceFormat";
import { useDaemon } from "../state/DaemonContext";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import styles from "./BotPanelWorkspace.module.css";

/** The disk line gets its own row from this fill level on. */
const DISK_WARN_PERCENT = 85;
/** A folder with this many entries gets a search field. */
const SEARCH_FROM = 8;

interface PreviewState {
  entry: WorkspaceEntryDto;
  kind: "text" | "image";
  text?: string;
  imageUrl?: string;
}

export function BotPanelWorkspace({
  botId,
  active
}: {
  botId: string;
  active: boolean;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, activeComputerId } = useDaemon();
  const [currentPath, setCurrentPath] = useState("");
  const [query, setQuery] = useState("");
  const [data, setData] = useState<BotWorkspaceListDto | null>(null);
  const [overview, setOverview] = useState<WorkspacesOverviewDto | null>(null);
  const requestId = useRef(0);
  const [loading, setLoading] = useState(false);
  const [editingPath, setEditingPath] = useState<string | null>(null);
  const [draftName, setDraftName] = useState("");
  const [preview, setPreview] = useState<PreviewState | null>(null);

  const load = useCallback(
    async (path: string, q: string) => {
      if (!client) return;
      const request = ++requestId.current;
      setLoading(true);
      try {
        const [res, usage] = await Promise.all([
          client.getBotWorkspace(botId, {
            path: path || undefined,
            q: q.trim() || undefined
          }),
          client.listWorkspaces()
        ]);
        if (request === requestId.current) {
          setData(res);
          setOverview(usage);
        }
      } catch (err) {
        if (request === requestId.current) toast.show(humanLine(err), "danger");
      } finally {
        if (request === requestId.current) setLoading(false);
      }
    },
    [botId, client, toast]
  );

  useEffect(() => {
    setCurrentPath("");
    setQuery("");
    setEditingPath(null);
    setData(null);
    setOverview(null);
    setPreview(null);
    requestId.current += 1;
  }, [botId, client]);

  useEffect(() => {
    if (!active) return;
    const timer = window.setTimeout(() => {
      void load(currentPath, query);
    }, query ? 160 : 0);
    return () => {
      window.clearTimeout(timer);
      requestId.current += 1;
    };
  }, [active, currentPath, query, load]);

  useEffect(() => {
    return () => {
      if (preview?.imageUrl) {
        URL.revokeObjectURL(preview.imageUrl);
      }
    };
  }, [preview]);

  const closePreview = useCallback(() => {
    setPreview((prev) => {
      if (prev?.imageUrl) URL.revokeObjectURL(prev.imageUrl);
      return null;
    });
  }, []);

  const handleOpenFolder = async () => {
    if (!data) return;
    if (isTauriRuntime() && activeComputerId === "local") {
      try {
        await openWorkspaceFolder(data.folder_name);
        return;
      } catch (err) {
        toast.show(humanLine(err), "danger");
        return;
      }
    }
    if (data.workspace_path) {
      toast.show(data.workspace_path, "info");
    }
  };

  const handlePreview = async (entry: WorkspaceEntryDto) => {
    if (!client || entry.preview_kind === "none") return;
    try {
      const blob = await client.getBotWorkspaceFile(botId, entry.path, false);
      if (entry.preview_kind === "image") {
        const url = URL.createObjectURL(blob);
        setPreview({ entry, kind: "image", imageUrl: url });
      } else {
        const text = await blob.text();
        setPreview({ entry, kind: "text", text });
      }
    } catch (err) {
      toast.show(humanLine(err), "danger");
    }
  };

  const handleDownload = async (entry: WorkspaceEntryDto) => {
    if (!client) return;
    try {
      const blob = await client.getBotWorkspaceFile(botId, entry.path, true);
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = entry.name;
      document.body.appendChild(a);
      a.click();
      a.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (err) {
      toast.show(humanLine(err), "danger");
    }
  };

  const commitRename = async () => {
    if (!client || !editingPath) return;
    const trimmed = draftName.trim();
    if (!trimmed) {
      setEditingPath(null);
      return;
    }
    try {
      await client.renameBotWorkspaceEntry(botId, {
        path: editingPath,
        new_name: trimmed
      });
      setEditingPath(null);
      await load(currentPath, query);
    } catch (err) {
      toast.show(humanLine(err), "danger");
    }
  };

  const handleDelete = async (entry: WorkspaceEntryDto) => {
    if (!client) return;
    try {
      await client.deleteBotWorkspaceEntry(botId, { path: entry.path });
      await load(currentPath, query);
    } catch (err) {
      toast.show(humanLine(err), "danger");
    }
  };

  const handleRestore = async (undoId: string) => {
    if (!client) return;
    try {
      await client.restoreBotWorkspaceEntry(botId, { undo_id: undoId });
      await load(currentPath, query);
    } catch (err) {
      toast.show(humanLine(err), "danger");
    }
  };

  const onRenameKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      void commitRename();
    } else if (e.key === "Escape") {
      e.preventDefault();
      setEditingPath(null);
    }
  };

  const crumbs = workspaceBreadcrumbs(currentPath);
  const limitLabel = data?.hard_limit_bytes
    ? ` / ${formatBytes(data.hard_limit_bytes)}`
    : data?.soft_limit_bytes
      ? ` / ${formatBytes(data.soft_limit_bytes)}`
      : "";
  const badgeClass = [
    styles.usageBadge,
    data?.hard_limit_exceeded
      ? styles.usageBadgeDanger
      : data?.soft_limit_exceeded
        ? styles.usageBadgeWarn
        : ""
  ]
    .filter(Boolean)
    .join(" ");

  const latestUndo = data?.undo_items[0];

  const diskSummary = overview?.disk
    ? t("workspace.diskSummary", {
        percent: overview.disk.used_percent,
        free: formatBytes(overview.disk.available_bytes),
        total: formatBytes(overview.disk.total_bytes),
        botsTotal: formatBytes(overview.total_workspace_bytes)
      })
    : null;

  return (
    <div className={styles.wrap} data-testid="bot-panel-workspace">
      <div className={styles.headerRow}>
        <span className={badgeClass} data-testid="bot-workspace-usage" title={diskSummary ?? undefined}>
          {data ? `${formatBytes(data.used_bytes)}${limitLabel}` : "—"}
        </span>
        {data?.workspace_path ? (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => void handleOpenFolder()}
            data-testid="bot-workspace-open-folder"
          >
            <FolderOpen aria-hidden="true" />
            {t("workspace.openFolder")}
          </Button>
        ) : null}
      </div>

      {/* The disk line is only worth a line of its own when the disk is nearly full; otherwise it is the usage badge's tooltip. */}
      {diskSummary && (overview?.disk?.used_percent ?? 0) >= DISK_WARN_PERCENT ? (
        <p className={styles.emptyNote} data-testid="bot-workspace-disk">{diskSummary}</p>
      ) : null}

      {/* A search field for a short folder is clutter: it appears once the list is long, or while a search is on. */}
      {query.trim() || (data?.entries.length ?? 0) >= SEARCH_FROM ? (
        <div className={styles.controls}>
          <input
            type="search"
            className={styles.searchInput}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("workspace.searchPlaceholder")}
            aria-label={t("workspace.searchPlaceholder")}
            data-testid="bot-workspace-search"
          />
        </div>
      ) : null}

      {crumbs.length > 0 ? (
        <nav className={styles.breadcrumbs} aria-label={t("workspace.breadcrumbs")}>
          <button
            type="button"
            className={styles.crumbBtn}
            onClick={() => setCurrentPath("")}
          >
            {data?.folder_name ?? t("workspace.root")}
          </button>
          {crumbs.map((c, idx) => (
            <span key={c.path}>
              {" / "}
              <button
                type="button"
                className={[
                  styles.crumbBtn,
                  idx === crumbs.length - 1 ? styles.crumbCurrent : ""
                ]
                  .filter(Boolean)
                  .join(" ")}
                onClick={() => setCurrentPath(c.path)}
              >
                {c.label}
              </button>
            </span>
          ))}
        </nav>
      ) : null}

      {latestUndo ? (
        <div className={styles.undoBar} role="status" data-testid="workspace-undo-bar">
          <span>
            {t("workspace.deletedUndoLabel", { name: latestUndo.name })}
          </span>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => void handleRestore(latestUndo.undo_id)}
            data-testid="workspace-undo-button"
          >
            <RotateCcw aria-hidden="true" />
            {t("workspace.undo")}
          </Button>
        </div>
      ) : null}

      {loading && !data ? (
        <p className={styles.emptyNote}>{t("workspace.loading")}</p>
      ) : !data || data.entries.length === 0 ? (
        <p className={styles.emptyNote}>
          {query.trim() ? t("workspace.noMatches") : t("workspace.emptyFolder")}
        </p>
      ) : (
        <div className={styles.list}>
          {data.entries.map((entry) => {
            const isEditing = editingPath === entry.path;
            const EntryIcon =
              entry.kind === "directory"
                ? Folder
                : entry.preview_kind === "image"
                  ? ImageIcon
                  : entry.preview_kind === "text"
                    ? FileText
                    : FileIcon;
            return (
              <div key={entry.path} className={styles.row} data-testid={`ws-entry-${entry.name}`}>
                {isEditing ? (
                  <div className={styles.renameForm}>
                    <input
                      type="text"
                      className={styles.renameInput}
                      value={draftName}
                      autoFocus
                      onChange={(e) => setDraftName(e.target.value)}
                      onKeyDown={onRenameKeyDown}
                      aria-label={t("workspace.rename")}
                    />
                    <button
                      type="button"
                      className={styles.iconAction}
                      title={t("common.save")}
                      aria-label={t("common.save")}
                      onClick={() => void commitRename()}
                    >
                      <Check size={14} aria-hidden="true" />
                    </button>
                    <button
                      type="button"
                      className={styles.iconAction}
                      title={t("common.cancel")}
                      aria-label={t("common.cancel")}
                      onClick={() => setEditingPath(null)}
                    >
                      <X size={14} aria-hidden="true" />
                    </button>
                  </div>
                ) : (
                  <>
                    <button
                      type="button"
                      className={[
                        styles.rowMain,
                        entry.kind === "file" && entry.preview_kind === "none"
                          ? styles.rowMainStatic
                          : ""
                      ]
                        .filter(Boolean)
                        .join(" ")}
                      onClick={() => {
                        if (entry.kind === "directory") {
                          setQuery("");
                          setCurrentPath(entry.path);
                        } else if (entry.preview_kind !== "none") {
                          void handlePreview(entry);
                        }
                      }}
                    >
                      <EntryIcon className={styles.rowIcon} aria-hidden="true" />
                      <span className={styles.rowName} title={entry.path}>
                        {entry.name}
                      </span>
                    </button>
                    {entry.kind === "file" ? (
                      <span className={styles.rowMeta}>{formatBytes(entry.size_bytes)}</span>
                    ) : null}
                    <div className={styles.rowActions}>
                      {entry.kind === "file" && entry.preview_kind !== "none" ? (
                        <button
                          type="button"
                          className={styles.iconAction}
                          title={t("workspace.preview")}
                          aria-label={t("workspace.preview")}
                          onClick={() => void handlePreview(entry)}
                        >
                          <Eye size={14} aria-hidden="true" />
                        </button>
                      ) : null}
                      {entry.kind === "file" ? (
                        <button
                          type="button"
                          className={styles.iconAction}
                          title={t("workspace.download")}
                          aria-label={t("workspace.download")}
                          onClick={() => void handleDownload(entry)}
                        >
                          <Download size={14} aria-hidden="true" />
                        </button>
                      ) : null}
                      <button
                        type="button"
                        className={styles.iconAction}
                        title={t("workspace.rename")}
                        aria-label={t("workspace.rename")}
                        onClick={() => {
                          setEditingPath(entry.path);
                          setDraftName(entry.name);
                        }}
                      >
                        <Pencil size={14} aria-hidden="true" />
                      </button>
                      <button
                        type="button"
                        className={[styles.iconAction, styles.iconActionDanger].join(" ")}
                        title={t("workspace.delete")}
                        aria-label={t("workspace.delete")}
                        onClick={() => void handleDelete(entry)}
                      >
                        <Trash2 size={14} aria-hidden="true" />
                      </button>
                    </div>
                  </>
                )}
              </div>
            );
          })}
        </div>
      )}

      <Modal
        open={preview !== null}
        onClose={closePreview}
        label={preview?.entry.name ?? ""}
        size="md"
      >
        {preview ? (
          <div className={styles.modalWrap} data-testid="workspace-preview-modal">
            <div className={styles.modalHead}>
              <h3 className={styles.modalTitle}>{preview.entry.name}</h3>
              <div className={styles.modalActions}>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void handleDownload(preview.entry)}
                >
                  <Download aria-hidden="true" />
                  {t("workspace.download")}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={closePreview}
                  title={t("common.close")}
                  aria-label={t("common.close")}
                >
                  <X aria-hidden="true" />
                </Button>
              </div>
            </div>
            <div className={styles.modalBody}>
              {preview.kind === "image" && preview.imageUrl ? (
                <div className={styles.previewImageWrap}>
                  <img
                    src={preview.imageUrl}
                    alt={preview.entry.name}
                    className={styles.previewImage}
                  />
                </div>
              ) : (
                <pre className={styles.previewText}>{preview.text ?? ""}</pre>
              )}
            </div>
          </div>
        ) : null}
      </Modal>
    </div>
  );
}
