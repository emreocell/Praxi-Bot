import { wordDiff, type DiffPart } from "../lib/textDiff.ts";
import { Check, ChevronDown, Search, Server, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { upsertApp } from "../lib/composioApps";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import { BrandIcon } from "../components/BrandIcon";
import { brandIcon } from "../lib/brandIcons";
import { botView } from "../lib/botProfile";
import { McpToolList } from "./McpToolList.tsx";
import { ApiError } from "../api/client";
import type { McpAuthState, McpCatalogEntry, McpServer } from "../api/types";
import { ConnectionTestButton } from "../components/ConnectionTest";
import formStyles from "../components/Form.module.css";
import { BotAccessPicker } from "../components/BotAccessPicker";
import { McpTrustChoice } from "../components/McpTrustChoice";
import { Modal } from "../components/Modal";
import { Segmented } from "../components/SettingsUi";
import { useToast } from "../components/Toast";
import { openExternal } from "../lib/externalLink";
import { mcpErrorHintKey } from "../lib/mcpError";
import {
  ALL_CATEGORIES,
  type BotScope,
  botAccessLabels,
  botScope,
  botSelection,
  saveServerScope,
  connectCatalogEntry,
  customServerConfig,
  filterCatalog,
  filterServers,
  isConnected,
  MissingFolderError,
  McpBrowserOpenError,
  MissingSecretError,
  signInMcpServer,
  statusTone,
  withFolder
} from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import { useMcp } from "../state/useMcp";
import styles from "./AppsModal.module.css";
import { AppTile, ComposioCell, GroupChips, SkeletonTiles } from "./AppsGrid";
import { ConnectSheet } from "./ConnectSheet";
import { type FlowCancel, type FlowState, runConnectFlow } from "../lib/connectFlow";
import { ComposioAdvanced } from "./ComposioAdvanced";
import { MissingComponent, componentFor } from "./MissingComponent";
import { useComposio } from "../state/useComposio";
import { SIGN_IN_WAIT_MINUTES, composioTileState, directEntries, filterDirect, isRemoteServer, settledApps, unsettledApps, visibleToolkits } from "../lib/appTiles";
import { openComposioLink } from "../lib/externalLink";
import type { McpServer as McpServerT } from "../api/types";
import own from "./AppsGrid.module.css";
import { humanLine } from "../lib/humanError";

function errorText(e: unknown): string {
  return humanLine(e);
}

/** Most bot faces shown next to an app; the rest collapse into "+n". */
const MAX_FACES = 4;

/**
 * A connected app: bare logo, name, the bots that use it, state.  "Manage"
 * (on hover / focus, always while open or broken) opens bot access,
 * reconnect and disconnect below the row.  An HTTP server that waits for
 * OAuth offers "Sign in" (opens the browser); a signed-in one "Sign out".
 */

/** The failure of one server: what to do first, then the full message and what the server printed. */
function DiffText({ parts }: { parts: DiffPart[] }) {
  return (
    <p className={styles.diffText}>
      {parts.map((p, i) => (
        <span
          key={i}
          className={p.kind === "added" ? styles.diffAdded : p.kind === "removed" ? styles.diffRemoved : undefined}
        >
          {p.text}
        </span>
      ))}
    </p>
  );
}

/** A tool the server changed after approval: off until the owner re-approves. */
function McpChangedTools({
  server,
  title,
  busy,
  onApprove
}: {
  server: McpServer;
  title: string;
  busy: boolean;
  onApprove: (tool: string, hash: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.changedList}>
      {(server.changed_tools ?? []).map((c) => {
        // The server's text is untrusted: it is only ever rendered as text.
        const diff = wordDiff(c.before, c.after);
        return (
          <section
            key={c.tool}
            className={styles.changedCard}
            role="alert"
            data-testid={`mcp-changed-${server.name}-${c.tool}`}
          >
            <b>{t("apps.changed.title")}</b>
            <p className={styles.errorHint}>
              {t(c.new_tool ? "apps.changed.newIntro" : "apps.changed.intro", { name: title, tool: c.tool })}
            </p>
            <span className={styles.panelLabel}>{t("apps.changed.before")}</span>
            <DiffText parts={diff.before} />
            <span className={styles.panelLabel}>{t("apps.changed.after")}</span>
            <DiffText parts={diff.after} />
            {c.schema_changed ? <p className={styles.errorHint}>{t("apps.changed.schema")}</p> : null}
            {c.other_changed ? <p className={styles.errorHint}>{t("apps.changed.other")}</p> : null}
            <p className={styles.errorHint}>{t("apps.changed.advice")}</p>
            <Button
              size="sm"
              variant="secondary"
              disabled={busy}
              data-testid={`mcp-approve-${server.name}-${c.tool}`}
              onClick={() => onApprove(c.tool, c.hash)}
            >
              {t("apps.changed.approve")}
            </Button>
          </section>
        );
      })}
    </div>
  );
}

function McpErrorBlock({
  server,
  busy,
  onChooseFolder,
  onReconnect
}: {
  server: McpServer;
  busy: boolean;
  onChooseFolder: (folder: string) => void;
  onReconnect: () => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [folder, setFolder] = useState("");
  const hintKey = mcpErrorHintKey(server.error_kind, server.command);
  const missing = server.error_kind === "runtime_missing" ? componentFor(server.command) : null;
  const stderr = server.stderr_tail ?? [];
  const id = `mcp-error-${server.name}`;
  return (
    <div className={styles.errorBlock} data-testid={`mcp-error-${server.name}`}>
      {missing ? (
        <MissingComponent id={missing} onReady={onReconnect} />
      ) : (
        <p className={styles.errorHint}>{hintKey ? t(hintKey) : t("apps.error.generic")}</p>
      )}
      {server.error_kind === "needs_folder" ? (
        <form
          className={styles.folderForm}
          onSubmit={(e) => {
            e.preventDefault();
            if (folder.trim()) {
              onChooseFolder(folder.trim());
            }
          }}
        >
          <label className={formStyles.label} htmlFor={`mcp-folder-${server.name}`}>
            {t("apps.folder.label")}
          </label>
          <input
            id={`mcp-folder-${server.name}`}
            className={formStyles.input}
            value={folder}
            placeholder={t("apps.folder.placeholder")}
            spellCheck={false}
            data-testid={`mcp-folder-input-${server.name}`}
            onChange={(e) => setFolder(e.target.value)}
          />
          <Button type="submit" size="sm" variant="primary" disabled={busy || !folder.trim()}>
            {t("apps.folder.save")}
          </Button>
        </form>
      ) : null}
      <button
        type="button"
        className={styles.errorToggle}
        aria-expanded={open}
        aria-controls={id}
        data-testid={`mcp-error-toggle-${server.name}`}
        onClick={() => setOpen((o) => !o)}
      >
        {open ? t("apps.error.hideDetails") : t("apps.error.showDetails")}
      </button>
      {open ? (
        <div className={styles.errorDetails} id={id}>
          <span className={styles.errorLabel}>{t("apps.error.message")}</span>
          <pre className={styles.errorPre}>{server.error}</pre>
          <span className={styles.errorLabel}>{t("apps.error.stderr")}</span>
          <pre className={styles.errorPre}>
            {stderr.length > 0 ? stderr.join("\n") : t("apps.error.noStderr")}
          </pre>
        </div>
      ) : null}
    </div>
  );
}

function ConnectedCell({
  server,
  title,
  icon,
  description,
  busy,
  signingIn,
  onReconnect,
  onApproveChange,
  onSignIn,
  onCancelSignIn,
  onPasteRedirect,
  onSignOut,
  onDisconnect,
  onSaveBots,
  onSaveTools,
  onSaveTrust,
  onChooseFolder
}: {
  server: McpServer;
  title?: string;
  icon?: string;
  description?: string;
  busy: boolean;
  onChooseFolder: (folder: string) => void;
  signingIn: boolean;
  onReconnect: () => void;
  onApproveChange: (tool: string, hash: string) => void;
  onSignIn: () => void;
  onCancelSignIn: () => void;
  onPasteRedirect: (url: string) => void;
  onSignOut: () => void;
  onDisconnect: () => void;
  onSaveBots: (from: BotScope, to: BotScope) => void;
  onSaveTools: (exclude: string[]) => void;
  onSaveTrust: (trusted: boolean) => void;
}) {
  const { t } = useTranslation();
  const { bots, client } = useDaemon();
  const [confirm, setConfirm] = useState(false);
  const [open, setOpen] = useState(false);
  const [auth, setAuth] = useState<McpAuthState | null>(null);
  const [pasted, setPasted] = useState("");
  const isHttp = server.transport === "http";
  const needsSignIn = server.status === "auth_required";
  // Ask the daemon (never the server) whether a sign-in is stored, so
  // header-authenticated servers do not get a meaningless "Sign out".
  useEffect(() => {
    if (!open || !isHttp || !client) {
      return;
    }
    let live = true;
    client
      .mcpAuthStatus(server.name)
      .then((state) => {
        if (live) setAuth(state);
      })
      .catch(() => {
        if (live) setAuth(null);
      });
    return () => {
      live = false;
    };
  }, [open, isHttp, client, server.name, server.status, signingIn]);
  const [scope, setScope] = useState<BotScope>(() => botScope(server));
  // The scope the editor opened with: the saved diff is against this, not
  // against whatever the live server says at click time.
  const [baseline, setBaseline] = useState<BotScope>(() => botScope(server));
  const access = botAccessLabels(server, bots, t("apps.access.deletedBot"));
  const tone = statusTone(server.status);
  const needsReconnect =
    needsSignIn ||
    server.status === "error" ||
    server.status === "disconnected" ||
    server.status === "quarantined";
  const users = bots.filter((b) => access.all || server.bots?.includes(b.id));
  const usedBy = access.all
    ? t("apps.botsAll")
    : access.names.length === 0
      ? t("apps.botsNone")
      : t("apps.botsSome", { names: access.names.join(", ") });
  const known = brandIcon(server.name);
  const panelId = `mcp-manage-${server.name}`;
  return (
    <li
      className={`${styles.cell} ${open ? styles.cellOpen : ""}`}
      data-testid={`mcp-server-${server.name}`}
    >
      <div className={styles.cellRow}>
        <BrandIcon name={icon ?? server.name} bare size={26} />
        <div className={styles.cellText}>
          <b className={styles.cellName}>
            <span className={styles.cellTitle}>{title ?? (known.known ? known.name : server.name)}</span>
            <span className={styles.faces} title={usedBy}>
              {users.slice(0, MAX_FACES).map((b) => (
                <BotAvatar key={b.id} bot={botView(b)} size="xs" decorative />
              ))}
              {users.length > MAX_FACES ? (
                <span className={styles.more}>+{users.length - MAX_FACES}</span>
              ) : null}
            </span>
          </b>
          <small className={styles.cellSub}>
            {description ?? t("apps.toolCount", { count: server.tool_count })}
            {" · "}
            {usedBy}
            {" · "}
            <span
              className={server.trust_hints ? styles.trustOn : styles.trustOff}
              data-testid={`mcp-trust-badge-${server.name}`}
            >
              {server.trust_hints ? t("apps.trust.badgeTrusted") : t("apps.trust.badgeAsks")}
            </span>
          </small>
        </div>
        <span className={`${styles.state} ${styles[signingIn ? "tone_busy" : `tone_${tone}`]}`}>
          {tone === "ok" && !signingIn ? <Check aria-hidden="true" /> : <span className={styles.dot} aria-hidden="true" />}
          {signingIn ? t("apps.composio.waiting") : t(`apps.status.${server.status}`)}
        </span>
        <button
          type="button"
          className={`${styles.manage} ${needsReconnect ? styles.manageShown : ""}`}
          aria-expanded={open}
          aria-controls={panelId}
          data-testid={`mcp-manage-${server.name}`}
          onClick={() => {
            if (!open) {
              setBaseline(botScope(server));
              setScope(botSelection(botScope(server), bots));
            }
            setConfirm(false);
            setOpen((o) => !o);
          }}
        >
          {t("apps.manage")}
          <ChevronDown aria-hidden="true" />
        </button>
      </div>
      {server.error && !signingIn && !needsSignIn ? (
        <McpErrorBlock server={server} busy={busy} onChooseFolder={onChooseFolder} onReconnect={onReconnect} />
      ) : null}
      {(server.changed_tools ?? []).length > 0 ? (
        <McpChangedTools
          server={server}
          title={title ?? (known.known ? known.name : server.name)}
          busy={busy}
          onApprove={onApproveChange}
        />
      ) : null}
      {open ? (
        <div className={styles.panel} id={panelId}>
          <span className={styles.panelLabel}>{t("apps.access.label")}</span>
          <BotAccessPicker scope={scope} bots={bots} onChange={setScope} />
          <McpTrustChoice
            id={`server-${server.name}`}
            value={server.trust_hints}
            disabled={busy}
            onChange={onSaveTrust}
          />
          <McpToolList server={server} client={client} busy={busy} onChange={onSaveTools} />
          <ConnectionTestButton
            testId={`mcp-connection-test-${server.name}`}
            disabled={busy || !client}
            run={() =>
              client
                ? client.testConnection("mcp", { name: server.name })
                : Promise.reject(new Error("daemon not connected"))
            }
          />
          <div className={styles.actions}>
            <Button
              size="sm"
              variant="primary"
              disabled={busy}
              data-testid={`mcp-access-save-${server.name}`}
              onClick={() => {
                setOpen(false);
                onSaveBots(baseline, scope);
              }}
            >
              {t("apps.access.save")}
            </Button>
            {needsSignIn && !signingIn ? (
              <Button
                size="sm"
                variant="secondary"
                disabled={busy}
                data-testid={`mcp-sign-in-${server.name}`}
                onClick={onSignIn}
              >
                {t("apps.auth.signIn")}
              </Button>
            ) : null}
            {signingIn ? (
              <>
                <span className={styles.panelLabel} role="status">
                  {t("apps.auth.waiting")}
                </span>
                <Button size="sm" variant="ghost" onClick={onCancelSignIn}>
                  {t("common.cancel")}
                </Button>
              </>
            ) : null}
          </div>
          {signingIn ? (
            <form
              className={styles.actions}
              onSubmit={(e) => {
                e.preventDefault();
                if (pasted.trim()) {
                  onPasteRedirect(pasted.trim());
                  setPasted("");
                }
              }}
            >
              <input
                className={formStyles.input}
                value={pasted}
                onChange={(e) => setPasted(e.target.value)}
                placeholder="http://127.0.0.1:…/callback?…"
                aria-label={t("apps.auth.pasteLabel")}
                data-testid={`mcp-paste-${server.name}`}
              />
              <Button size="sm" variant="secondary" type="submit" disabled={!pasted.trim()}>
                {t("apps.auth.pasteSubmit")}
              </Button>
            </form>
          ) : null}
          {signingIn ? <p className={formStyles.hint}>{t("apps.auth.pasteHint")}</p> : null}
          <div className={styles.actions}>
            {needsReconnect && !needsSignIn ? (
              <Button size="sm" variant="secondary" disabled={busy} onClick={onReconnect}>
                {t("apps.reconnect")}
              </Button>
            ) : null}
            {isHttp && auth === "signed_in" && !signingIn ? (
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                data-testid={`mcp-sign-out-${server.name}`}
                onClick={onSignOut}
              >
                {t("apps.auth.signOut")}
              </Button>
            ) : null}
            <span className={styles.grow} />
            {confirm ? (
              <>
                <Button
                  size="sm"
                  variant="danger"
                  disabled={busy}
                  onClick={() => {
                    setConfirm(false);
                    setOpen(false);
                    onDisconnect();
                  }}
                >
                  {t("apps.disconnectConfirm")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>
                  {t("common.cancel")}
                </Button>
              </>
            ) : (
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => setConfirm(true)}>
                {t("apps.disconnect")}
              </Button>
            )}
          </div>
        </div>
      ) : null}
    </li>
  );
}

/** A catalog app: one click connects; apps that need a key ask for it right under the row. */
function DiscoverCell({
  entry,
  onConnect
}: {
  entry: McpCatalogEntry;
  onConnect: (values: Record<string, string>, folder?: string, trustHints?: boolean) => Promise<void>;
}) {
  const { t } = useTranslation();
  const secrets = entry.install.required_secrets;
  const wantsFolder = Boolean(entry.install.folder);
  const [folder, setFolder] = useState(entry.install.folder?.suggestion ?? "");
  const [asking, setAsking] = useState(false);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  // The safe answer is the default: every action of this server asks.
  const [trust, setTrust] = useState(false);

  const submit = async (v: Record<string, string>) => {
    setBusy(true);
    try {
      await onConnect(v, wantsFolder ? folder : undefined, trust);
      setAsking(false);
      setValues({});
      setTrust(false);
    } finally {
      setBusy(false);
    }
  };

  const complete =
    secrets.every((s) => (values[s.name] ?? "").trim() !== "") &&
    (!wantsFolder || folder.trim() !== "");

  return (
    <li
      className={`${styles.cell} ${asking ? styles.cellOpen : ""}`}
      data-testid={`mcp-catalog-${entry.id}`}
    >
      <div className={styles.cellRow}>
        <BrandIcon name={entry.name} bare size={26} />
        <div className={styles.cellText}>
          <b className={styles.cellName}>
            <span className={styles.cellTitle}>{entry.name}</span>
          </b>
          <small className={styles.cellSub} title={entry.description}>{entry.description}</small>
        </div>
        <button
          type="button"
          className={styles.connect}
          disabled={busy || asking}
          aria-label={t("apps.connectApp", { name: entry.name })}
          onClick={() => setAsking(true)}
        >
          {t("apps.connect")}
        </button>
      </div>
      {asking ? (
        <form
          className={styles.panel}
          onSubmit={(e) => {
            e.preventDefault();
            if (complete) {
              void submit(values);
            }
          }}
        >
          {wantsFolder ? (
            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor={`${entry.id}-folder`}>
                {t("apps.folder.label")}
              </label>
              <input
                id={`${entry.id}-folder`}
                className={formStyles.input}
                autoFocus
                spellCheck={false}
                value={folder}
                placeholder={t("apps.folder.placeholder")}
                data-testid={`mcp-catalog-folder-${entry.id}`}
                onChange={(e) => setFolder(e.target.value)}
              />
              <p className={formStyles.hint}>{t("apps.folder.hint")}</p>
            </div>
          ) : null}
          {secrets.map((s, i) => (
            <div key={s.name} className={formStyles.field}>
              <label className={formStyles.label} htmlFor={`${entry.id}-${s.name}`}>
                {s.label}
              </label>
              <input
                id={`${entry.id}-${s.name}`}
                className={formStyles.input}
                type="password"
                autoComplete="off"
                autoFocus={i === 0}
                value={values[s.name] ?? ""}
                onChange={(e) =>
                  setValues((v) => ({ ...v, [s.name]: e.target.value }))
                }
              />
              {s.help_url ? (
                <button
                  type="button"
                  className={styles.link}
                  onClick={() => void openExternal(s.help_url ?? "")}
                >
                  {t("apps.getKey")}
                </button>
              ) : null}
            </div>
          ))}
          {secrets.length > 0 ? (
            <p className={formStyles.hint}>{t("apps.secretHint")}</p>
          ) : null}
          <McpTrustChoice id={entry.id} value={trust} onChange={setTrust} disabled={busy} />
          <div className={styles.actions}>
            <Button type="submit" size="sm" variant="primary" disabled={!complete || busy}>
              {t("apps.connectNow")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                setAsking(false);
                setValues({});
                setTrust(false);
              }}
            >
              {t("common.cancel")}
            </Button>
          </div>
        </form>
      ) : null}
    </li>
  );
}

function CustomServerForm({
  existing,
  onSubmit,
  onCancel
}: {
  existing: string[];
  onSubmit: (cfg: ReturnType<typeof customServerConfig>) => Promise<void>;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState({
    name: "",
    transport: "stdio" as "stdio" | "http",
    command: "",
    url: "",
    env: ""
  });
  const [error, setError] = useState<"name" | "command" | "url" | null>(null);
  const [busy, setBusy] = useState(false);
  const { bots } = useDaemon();
  const [scope, setScope] = useState<BotScope>({ mode: "all" });
  const [trust, setTrust] = useState(false);
  const set = (patch: Partial<typeof form>) => setForm((f) => ({ ...f, ...patch }));

  return (
    <form
      className={styles.custom}
      onSubmit={(e) => {
        e.preventDefault();
        const result = customServerConfig(form, existing, botSelection(scope, bots), trust);
        setError(result.ok ? null : result.error);
        if (result.ok) {
          setBusy(true);
          void onSubmit(result).finally(() => setBusy(false));
        }
      }}
    >
      <p className={formStyles.hint}>{t("apps.custom.intro")}</p>
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor="mcp-name">
          {t("apps.custom.name")}
        </label>
        <input
          id="mcp-name"
          className={formStyles.input}
          value={form.name}
          placeholder="my-server"
          onChange={(e) => set({ name: e.target.value })}
        />
        {error === "name" ? (
          <p className={formStyles.error} role="alert">{t("apps.custom.errName")}</p>
        ) : null}
      </div>
      <Segmented
        label={t("apps.custom.transport")}
        value={form.transport}
        onChange={(transport) => set({ transport })}
        options={[
          { value: "stdio", label: t("apps.custom.stdio") },
          { value: "http", label: t("apps.custom.http") }
        ]}
      />
      {form.transport === "stdio" ? (
        <>
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="mcp-cmd">
              {t("apps.custom.command")}
            </label>
            <input
              id="mcp-cmd"
              className={formStyles.input}
              value={form.command}
              placeholder="npx -y @scope/server"
              onChange={(e) => set({ command: e.target.value })}
            />
            {error === "command" ? (
              <p className={formStyles.error} role="alert">{t("apps.custom.errCommand")}</p>
            ) : null}
          </div>
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="mcp-env">
              {t("apps.custom.env")}
            </label>
            <textarea
              id="mcp-env"
              className={formStyles.textarea}
              value={form.env}
              placeholder="KEY=value"
              onChange={(e) => set({ env: e.target.value })}
            />
            <p className={formStyles.hint}>{t("apps.custom.envHint")}</p>
          </div>
        </>
      ) : (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor="mcp-url">
            {t("apps.custom.url")}
          </label>
          <input
            id="mcp-url"
            className={formStyles.input}
            value={form.url}
            placeholder="https://"
            onChange={(e) => set({ url: e.target.value })}
          />
          {error === "url" ? (
            <p className={formStyles.error} role="alert">{t("apps.custom.errUrl")}</p>
          ) : null}
        </div>
      )}
      <div className={formStyles.field}>
        <span className={formStyles.label}>{t("apps.access.label")}</span>
        <BotAccessPicker scope={scope} bots={bots} onChange={setScope} />
      </div>
      <McpTrustChoice id="custom" value={trust} onChange={setTrust} disabled={busy} />
      <div className={styles.formActions}>
        <Button type="submit" variant="primary" size="sm" disabled={busy}>
          {t("apps.custom.add")}
        </Button>
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {t("common.cancel")}
        </Button>
      </div>
    </form>
  );
}

/**
 * "Uygulamalar": search, Connected and Discover.  Apps connect in one click;
 * "Your own MCP server" adds a custom stdio / http server.
 */
export function AppsModal({
  open,
  onClose
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const { servers, catalog, error, reload } = useMcp(open);
  const [query, setQuery] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [group, setGroup] = useState("all");
  const [busy, setBusy] = useState<string | null>(null);
  const c = useComposio(open, query, group);
  const [linking, setLinking] = useState<string | null>(null);
  // The one connect flow that runs now (anchored to its tile), and its stop flag.
  const [flow, setFlow] = useState<FlowState | null>(null);
  const flowCancel = useRef<FlowCancel>({ cancelled: false });
  // Pending sign-ins by server; the flag stops the poll and cancels the flow.
  const [signing, setSigning] = useState<Record<string, { aborted: boolean }>>({});
  // Servers this session created for a sign-in that has not finished: they are tiles with a
  // Cancel, never "Connected", and are removed again when the sign-in is cancelled or expires.
  const [fresh, setFresh] = useState<Record<string, true>>({});

  const signIn = async (name: string) => {
    if (!client || signing[name]) {
      return;
    }
    const signal = { aborted: false };
    let expired = false;
    // A sign-in nobody finishes expires (the documented wait) and is cleaned up.
    const timer = setTimeout(() => {
      expired = true;
      signal.aborted = true;
    }, SIGN_IN_WAIT_MINUTES * 60 * 1000);
    setSigning((m) => ({ ...m, [name]: signal }));
    let signedIn = false;
    try {
      const state = await signInMcpServer(client, name, openExternal, { signal });
      signedIn = state === "signed_in";
      if (signedIn) {
        toast.show(t("apps.auth.signedInToast", { name }), "success");
      } else if (expired) {
        toast.show(t("apps.auth.expiredToast", { name, minutes: SIGN_IN_WAIT_MINUTES }), "info");
      } else if (!signal.aborted) {
        toast.show(t("apps.auth.failedToast", { name }), "danger");
      }
    } catch (e) {
      toast.show(
        e instanceof McpBrowserOpenError ? t("apps.auth.openFailed", { name }) : errorText(e),
        "danger"
      );
    } finally {
      clearTimeout(timer);
      setSigning((m) => {
        const next = { ...m };
        delete next[name];
        return next;
      });
      // A connection that never finished is not kept as a half-made server.
      if (!signedIn && fresh[name]) {
        await client.deleteMcpServer(name).catch(() => undefined);
      }
      setFresh((m) => {
        const next = { ...m };
        delete next[name];
        return next;
      });
      await reload();
    }
  };

  useEffect(() => {
    if (open) {
      setQuery("");
      setGroup("all");
      setFlow(null);
      setAdvanced(false);
    }
  }, [open]);

  const shownServers = useMemo(() => filterServers(servers, query), [servers, query]);
  const available = useMemo(
    () =>
      filterCatalog(catalog, query, ALL_CATEGORIES).filter(
        (entry) => !isConnected(entry, servers)
      ),
    [catalog, query, servers]
  );
  // Remote apps Praxi signs in to itself are tiles in the grid; local developer
  // servers and token cards live under Advanced.
  const allDirect = useMemo(
    () => catalog.filter((e) => e.install.transport === "http" && e.install.required_secrets.length === 0),
    [catalog]
  );
  const pendingNames = useMemo(() => new Set([...Object.keys(signing), ...Object.keys(fresh)]), [signing, fresh]);
  const directShown = useMemo(
    () => filterDirect(directEntries(catalog, servers.filter((s) => !pendingNames.has(s.name))), query, group),
    [catalog, servers, query, group, pendingNames]
  );
  const shownCatalog = useMemo(
    () =>
      available.filter(
        (e) => e.install.required_secrets.length === 0 && e.install.transport !== "http"
      ),
    [available]
  );
  const tokenCatalog = useMemo(
    () => available.filter((e) => e.install.required_secrets.length > 0),
    [available]
  );
  const remoteServers = useMemo(
    () => shownServers.filter((s) => isRemoteServer(s) && !pendingNames.has(s.name)),
    [shownServers, pendingNames]
  );
  const localServers = useMemo(() => shownServers.filter((s) => !isRemoteServer(s)), [shownServers]);
  const toolkits = useMemo(
    () => visibleToolkits(c.items, c.apps, allDirect, []),
    [c.items, c.apps, allDirect]
  );
  const entryOf = (name: string) => catalog.find((e) => e.id === name);
  const describe = (name: string) => catalog.find((c) => c.id === name)?.description;

  const run = async (key: string, fn: () => Promise<void>) => {
    setBusy(key);
    try {
      await fn();
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setBusy(null);
    }
  };

  const connect = async (
    entry: McpCatalogEntry,
    values: Record<string, string>,
    folder?: string,
    trustHints = false
  ) => {
    if (!client) {
      return;
    }
    try {
      await connectCatalogEntry(client, entry, values, folder, trustHints);
      toast.show(t("apps.connectedToast", { name: entry.name }), "success");
      await reload();
    } catch (e) {
      toast.show(
        e instanceof MissingSecretError
          ? t("apps.missingSecret")
          : e instanceof MissingFolderError
            ? t("apps.folder.missing")
            : errorText(e),
        "danger"
      );
      throw e;
    }
  };

  /**
   * One continuous Connect flow anchored to the app's tile: with a Composio credential the app's
   * own consent page opens at once; without one the Composio sign-in opens first and the flow
   * carries on to the app by itself when it finishes (nothing to press twice).
   */
  const connectApp = (slug: string, name: string) => {
    if (!client) {
      return;
    }
    flowCancel.current.cancelled = true; // a flow of another tile is replaced
    const cancel: FlowCancel = { cancelled: false };
    flowCancel.current = cancel;
    setLinking(slug);
    void runConnectFlow(
      {
        configured: () => c.status?.configured === true,
        loginStart: () => client.composioLoginStart(),
        loginStatus: () => client.composioLoginStatus(),
        appConnect: (s) => client.composioConnect(s),
        open: (url, login) => openComposioLink(url, login),
        sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
        now: () => Date.now()
      },
      slug,
      name,
      (state) => {
        if (flowCancel.current !== cancel) {
          return;
        }
        setFlow(state);
        if (state.step === "app") {
          void c.reloadStatus();
        }
        if (state.step === "opened") {
          c.setApps((prev) =>
            upsertApp(prev, { slug, name, state: "pending", enabled: true, native_first: false, bots: null, tools: 0 })
          );
        }
      },
      cancel
    ).finally(() => {
      if (flowCancel.current === cancel) {
        setLinking(null);
      }
    });
  };

  /** Stop the flow of this tile; a consent page that was already opened is cancelled too. */
  const cancelFlow = (slug: string) => {
    flowCancel.current.cancelled = true;
    setFlow(null);
    setLinking(null);
    void run(slug, async () => {
      await client?.composioDisconnect(slug);
      await c.reloadApps();
    });
  };

  const pressComposio = (slug: string, name: string) => connectApp(slug, name);

  const connectDirect = async (entry: McpCatalogEntry) => {
    if (!client) {
      return;
    }
    try {
      // Created only to be signed in: not "connected" until the sign-in finishes.
      await connectCatalogEntry(client, entry, {});
    } catch (e) {
      toast.show(errorText(e), "danger");
      return;
    }
    setFresh((m) => ({ ...m, [entry.id]: true }));
    await signIn(entry.id);
  };

  const renderServer = (s: McpServerT) => (
    <ConnectedCell
      key={s.name}
      server={s}
      title={entryOf(s.name)?.name}
      icon={entryOf(s.name)?.icon ?? undefined}
      description={describe(s.name)}
      busy={busy === s.name}
      onChooseFolder={(folder) =>
        void run(s.name, async () => {
          await client?.updateMcpServer(s.name, { args: withFolder(s.args ?? [], folder) });
          await reload();
        })
      }
      signingIn={Boolean(signing[s.name])}
      onSignIn={() => void signIn(s.name)}
      onCancelSignIn={() => {
        const signal = signing[s.name];
        if (signal) {
          signal.aborted = true;
        }
      }}
      onPasteRedirect={(url) =>
        void run(s.name, async () => {
          await client?.completeMcpAuth(s.name, url);
        })
      }
      onSignOut={() =>
        void run(s.name, async () => {
          await client?.logoutMcpAuth(s.name);
          toast.show(t("apps.auth.signedOutToast", { name: s.name }), "info");
          await reload();
        })
      }
      onReconnect={() =>
        void run(s.name, async () => {
          await client?.reconnectMcpServer(s.name);
          await reload();
        })
      }
      onApproveChange={(tool, hash) =>
        void run(s.name, async () => {
          try {
            await client?.approveMcpToolChanges(s.name, tool, hash);
          } catch (error) {
            // The server changed the definition again after the card was painted:
            // nothing was approved. Reload so the owner reviews the new text.
            if (error instanceof ApiError && error.code === "mcp_tool_changed_again") {
              await reload();
            }
            throw error;
          }
          toast.show(t("apps.changed.toast", { name: s.name, tool }), "success");
          await reload();
        })
      }
      onSaveTools={(exclude) =>
        void run(s.name, async () => {
          await client?.updateMcpServer(s.name, { tool_exclude: exclude });
          await reload();
        })
      }
      onSaveTrust={(trusted) =>
        void run(s.name, async () => {
          await client?.updateMcpServer(s.name, { trust_hints: trusted });
          toast.show(
            t(trusted ? "apps.trust.savedYes" : "apps.trust.savedNo", { name: s.name }),
            "success"
          );
          await reload();
        })
      }
      onSaveBots={(from, to) =>
        void run(s.name, async () => {
          if (!client) {
            return;
          }
          const { applied } = await saveServerScope(client, s.name, from, to);
          if (applied) {
            toast.show(t("apps.access.savedToast", { name: s.name }), "success");
          } else {
            toast.show(t("apps.access.notApplied", { name: s.name }), "danger");
          }
          await reload();
        })
      }
      onDisconnect={() =>
        void run(s.name, async () => {
          await client?.deleteMcpServer(s.name);
          toast.show(t("apps.disconnectedToast", { name: s.name }), "info");
          await reload();
        })
      }
    />
  );

  const firstLoad = c.items.length === 0 && c.loading && !c.error;
  const catalogError = c.meta?.error ?? null;

  return (
    <Modal open={open} onClose={onClose} label={t("apps.title")} size="apps">
      <div className={styles.root}>
        <header className={styles.top}>
          <div className={styles.find}>
            <Search aria-hidden="true" />
            <input
              type="search"
              data-autofocus
              value={query}
              placeholder={t("apps.search")}
              aria-label={t("apps.search")}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <button
            type="button"
            className={`${styles.pill} ${advanced ? styles.pillOn : ""}`}
            aria-pressed={advanced}
            data-testid="apps-advanced"
            onClick={() => setAdvanced((a) => !a)}
          >
            <Server aria-hidden="true" />
            {t("apps.mcpServers")}
          </button>
          <button
            type="button"
            className={styles.close}
            title={t("common.close")}
            aria-label={t("common.close")}
            onClick={onClose}
          >
            <X />
          </button>
        </header>

        <div className={styles.body}>
          {error ? (
            <p className={formStyles.error} role="alert">
              {error}
            </p>
          ) : null}

          {advanced ? (
            <>
            <h3 className={styles.sectionLabel}>{t("apps.advanced.title")}</h3>
            <CustomServerForm
              existing={servers.map((s) => s.name)}
              onCancel={() => setAdvanced(false)}
              onSubmit={async (result) => {
                if (!client || !result.ok) {
                  return;
                }
                await run("custom", async () => {
                  await client.createMcpServer(result.config);
                  toast.show(
                    t("apps.connectedToast", { name: result.config.name }),
                    "success"
                  );
                  await reload();
                  setAdvanced(false);
                });
              }}
            />
            {localServers.length > 0 || shownCatalog.length > 0 ? (
              <section className={styles.section} aria-labelledby="apps-local-h">
                <h3 className={styles.sectionLabel} id="apps-local-h">{t("apps.advanced.localTitle")}</h3>
                <ul className={styles.grid}>
                  {localServers.map(renderServer)}
                  {shownCatalog.map((entry) => (
                    <DiscoverCell
                      key={entry.id}
                      entry={entry}
                      onConnect={(values, folder, trust) => connect(entry, values, folder, trust)}
                    />
                  ))}
                </ul>
              </section>
            ) : null}
            {tokenCatalog.length > 0 ? (
              <section className={styles.section} aria-labelledby="apps-token-h">
                <h3 className={styles.sectionLabel} id="apps-token-h">{t("apps.advanced.tokenCards")}</h3>
                <p className={formStyles.hint}>{t("apps.advanced.tokenHint")}</p>
                <ul className={styles.grid}>
                  {tokenCatalog.map((entry) => (
                    <DiscoverCell
                      key={entry.id}
                      entry={entry}
                      onConnect={(values, _folder, trust) => connect(entry, values, undefined, trust)}
                    />
                  ))}
                </ul>
              </section>
            ) : null}
            <ComposioAdvanced
              status={c.status}
              onChanged={async () => {
                await c.reloadStatus();
                await c.reloadApps();
              }}
            />
            </>
          ) : (
            <>
              {settledApps(c.apps).length > 0 || remoteServers.length > 0 ? (
                <section className={styles.section} aria-labelledby="apps-connected-h">
                  <h3 className={styles.sectionLabel} id="apps-connected-h">{t("apps.connectedSection")}</h3>
                  <ul className={styles.grid}>
                    {settledApps(c.apps).map((a) => (
                      <ComposioCell
                        key={a.slug}
                        app={a}
                        busy={busy === a.slug}
                        onEnabled={(enabled) =>
                          void run(a.slug, async () => {
                            const next = await client?.composioSetEnabled(a.slug, enabled);
                            if (next) {
                              c.setApps((prev) => upsertApp(prev, next));
                            }
                          })
                        }
                        onReconnect={() => pressComposio(a.slug, a.name)}
                        onDisconnect={() =>
                          void run(a.slug, async () => {
                            const r = await client?.composioDisconnect(a.slug);
                            toast.show(
                              r && !r.remote_removed
                                ? t("apps.composio.disconnectedLocal", { name: a.name })
                                : t("apps.disconnectedToast", { name: a.name }),
                              "info"
                            );
                            await c.reloadApps();
                          })
                        }
                      />
                    ))}
                    {remoteServers.map(renderServer)}
                  </ul>
                </section>
              ) : null}

              <GroupChips value={group} onChange={setGroup} />

              <section className={styles.section} aria-labelledby="apps-all-h">
                <h3 className={styles.sectionLabel} id="apps-all-h">{t("apps.catalogSection")}</h3>
                {firstLoad ? (
                  <SkeletonTiles />
                ) : c.error && c.items.length === 0 && directShown.length === 0 ? (
                  <p className={own.quiet} role="alert">
                    {t("apps.composio.loadFailed")}
                    <Button size="sm" variant="secondary" onClick={() => void c.retryCatalog()}>
                      {t("apps.composio.retry")}
                    </Button>
                  </p>
                ) : toolkits.length === 0 && directShown.length === 0 ? (
                  <p className={styles.empty}><Search aria-hidden="true" />{t("apps.composio.noResults")}</p>
                ) : (
                  <ul className={styles.grid} data-testid="apps-grid">
                    {unsettledApps(c.apps).map((a) => (
                      <AppTile
                        key={`pending-${a.slug}`}
                        testId={`composio-tile-${a.slug}`}
                        slug={a.slug}
                        name={a.name}
                        description={a.name}
                        state={composioTileState(a, false)}
                        onConnect={() => pressComposio(a.slug, a.name)}
                        onCancel={() => cancelFlow(a.slug)}
                        sheet={
                          flow?.slug === a.slug ? (
                            <ConnectSheet
                              flow={flow}
                              onCancel={() => cancelFlow(a.slug)}
                              onRetry={() => pressComposio(a.slug, a.name)}
                              onClose={() => setFlow(null)}
                            />
                          ) : null
                        }
                      />
                    ))}
                    {[...Object.keys(signing), ...Object.keys(fresh)]
                      .filter((n, i, all) => all.indexOf(n) === i)
                      .map((n) => {
                        const e = entryOf(n);
                        return e ? (
                          <AppTile
                            key={`signing-${n}`}
                            testId={`mcp-catalog-${n}`}
                            name={e.name}
                            logo={e.icon ?? e.name}
                            description={e.description}
                            state="waiting"
                            onConnect={() => undefined}
                            onCancel={() => {
                              const sig = signing[n];
                              if (sig) {
                                sig.aborted = true;
                              }
                            }}
                          />
                        ) : null;
                      })}
                    {directShown.map((e) => (
                      <AppTile
                        key={e.id}
                        testId={`mcp-catalog-${e.id}`}
                        name={e.name}
                        logo={e.icon ?? e.name}
                        description={e.description}
                        state={busy === e.id ? "linking" : "idle"}
                        onConnect={() => void connectDirect(e)}
                      />
                    ))}
                    {toolkits.map((tk) => (
                      <AppTile
                        key={tk.slug}
                        testId={`composio-tile-${tk.slug}`}
                        slug={tk.slug}
                        name={tk.name}
                        description={tk.description || tk.categories.join(", ")}
                        state={composioTileState(undefined, linking === tk.slug)}
                        onConnect={() => pressComposio(tk.slug, tk.name)}
                        onCancel={() => cancelFlow(tk.slug)}
                        sheet={
                          flow?.slug === tk.slug ? (
                            <ConnectSheet
                              flow={flow}
                              onCancel={() => cancelFlow(tk.slug)}
                              onRetry={() => pressComposio(tk.slug, tk.name)}
                              onClose={() => setFlow(null)}
                            />
                          ) : null
                        }
                      />
                    ))}
                  </ul>
                )}
                {catalogError ? (
                  <p className={own.quiet} data-testid="apps-catalog-notice">
                    {t(`apps.composio.catalogError.${catalogError}`)}
                    <Button size="sm" variant="ghost" onClick={() => void c.retryCatalog()}>
                      {t("apps.composio.retry")}
                    </Button>
                  </p>
                ) : null}
                <div className={styles.actions}>
                  {c.hasMore ? (
                    <Button size="sm" variant="secondary" disabled={c.loading} onClick={() => void c.loadMore()}>
                      {t("apps.composio.loadMore")}
                    </Button>
                  ) : null}
                </div>
              </section>
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}
