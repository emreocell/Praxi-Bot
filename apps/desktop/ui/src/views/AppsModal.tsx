import { wordDiff, type DiffPart } from "../lib/textDiff.ts";
import { ArrowLeft, Check, ChevronDown, Search, Server, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { upsertApp, withoutConnected } from "../lib/composioApps";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import { BrandIcon } from "../components/BrandIcon";
import { brandIcon } from "../lib/brandIcons";
import { botView } from "../lib/botProfile";
import { McpToolList } from "./McpToolList.tsx";
import { ApiError } from "../api/client";
import type { ComposioToolkit, McpAuthState, McpCatalogEntry, McpServer } from "../api/types";
import { ConnectionTestButton } from "../components/ConnectionTest";
import formStyles from "../components/Form.module.css";
import { BotAccessPicker } from "../components/BotAccessPicker";
import { McpTrustChoice } from "../components/McpTrustChoice";
import { Modal } from "../components/Modal";
import { Segmented } from "../components/SettingsUi";
import { useToast } from "../components/Toast";
import { oauthRelay, openExternal, openSignInLink } from "../lib/externalLink";
import { addGoogleApp, GoogleClientError, googleEntries } from "../lib/googleSetup";
import { type SignInControl, type SignInState, expectedSignInHosts, runSignIn } from "../lib/mcpSignIn";
import { isLocalBaseUrl } from "../lib/liveViewState";
import { GoogleApps } from "./GoogleApps";
import { McpSampleTest } from "./McpSampleTest";
import { McpSignInPanel } from "./McpSignInPanel";
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
  MissingSecretError,
  statusTone,
  withFolder
} from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import { useMcp } from "../state/useMcp";
import styles from "./AppsModal.module.css";
import { AppTile, ComposioCard, GroupChips, SkeletonTiles } from "./AppsGrid";
import { ComposioAppPage } from "./ComposioAppPage";
import { type AppTestState, appsToTestNow, runAppTest } from "../lib/appTest";
import { type FlowCancel, type FlowState, runConnectFlow } from "../lib/connectFlow";
import { ComposioAccount } from "./ComposioAccount";
import { HomeAssistantCard } from "./HomeAssistantCard";
import { MissingComponent, componentFor } from "./MissingComponent";
import { useComposio } from "../state/useComposio";
import { directEntries, filterDirect, isRemoteServer } from "../lib/appTiles";
import { openComposioLink } from "../lib/externalLink";
import type { McpServer as McpServerT } from "../api/types";
import own from "./AppsGrid.module.css";
import { humanError, humanLine } from "../lib/humanError";

function errorText(e: unknown): string {
  return humanLine(e);
}

/** How long a started sign-in is waited for (the daemon's own limit). */
const SIGN_IN_EXPIRES_MINUTES = 5;

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
      <div className={styles.actions}>
        {server.error_kind === "needs_folder" || missing ? null : (
          <Button size="sm" variant="primary" disabled={busy} data-testid={`mcp-error-reconnect-${server.name}`} onClick={onReconnect}>
            {t("apps.reconnect")}
          </Button>
        )}
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
      </div>
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
  sample,
  onSignOut,
  onDisconnect,
  lastUserOfSecret = false,
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
  /** The catalog entry has a fixed read-only test (Google apps): offer it under Manage. */
  sample: boolean;
  onSignOut: () => void;
  /** No other app uses this one's client secret: forgetting it is offered ticked. */
  lastUserOfSecret?: boolean;
  /** `forgetClientSecret`: also delete the sign-in client secret (when no other app uses it). */
  onDisconnect: (forgetClientSecret: boolean) => void;
  onSaveBots: (from: BotScope, to: BotScope) => void;
  onSaveTools: (exclude: string[]) => void;
  onSaveTrust: (trusted: boolean) => void;
}) {
  const { t } = useTranslation();
  const { bots, client } = useDaemon();
  const [confirm, setConfirm] = useState(false);
  const [forgetSecret, setForgetSecret] = useState(false);
  const [open, setOpen] = useState(false);
  const [fixOpen, setFixOpen] = useState(false);
  const [auth, setAuth] = useState<McpAuthState | null>(null);
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
  const broken = Boolean(server.error) && !signingIn && !needsSignIn;
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
          {broken ? (
            <small className={`${styles.cellSub} ${styles.tone_bad}`} data-testid={`mcp-error-short-${server.name}`}>
              {t("apps.error.short")}
            </small>
          ) : (
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
          )}
        </div>
        {broken ? null : (
          <span className={`${styles.state} ${styles[signingIn ? "tone_busy" : `tone_${tone}`]}`}>
            {tone === "ok" && !signingIn ? <Check aria-hidden="true" /> : <span className={styles.dot} aria-hidden="true" />}
            {signingIn ? t("apps.composio.waiting") : t(`apps.status.${server.status}`)}
          </span>
        )}
        {broken ? (
          <Button
            size="sm"
            variant="secondary"
            aria-expanded={fixOpen}
            data-testid={`mcp-fix-${server.name}`}
            onClick={() => setFixOpen((v) => !v)}
          >
            {fixOpen ? t("apps.error.close") : t("apps.error.fix")}
          </Button>
        ) : null}
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
      {broken && fixOpen ? (
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
          {sample ? (
            <McpSampleTest client={client} name={server.name} disabled={busy || server.status !== "connected"} />
          ) : null}
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
                {server.oauth?.client_secret_ref ? (
                  <label className={formStyles.hint}>
                    <input
                      type="checkbox"
                      checked={forgetSecret}
                      data-testid={`mcp-forget-secret-${server.name}`}
                      onChange={(e) => setForgetSecret(e.target.checked)}
                    />{" "}
                    {t("apps.forgetClientSecret")}
                  </label>
                ) : null}
                <Button
                  size="sm"
                  variant="danger"
                  disabled={busy}
                  onClick={() => {
                    setConfirm(false);
                    setOpen(false);
                    onDisconnect(forgetSecret);
                    setForgetSecret(false);
                  }}
                >
                  {t("apps.disconnectConfirm")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>
                  {t("common.cancel")}
                </Button>
              </>
            ) : (
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                onClick={() => {
                  setForgetSecret(lastUserOfSecret);
                  setConfirm(true);
                }}
              >
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

type AppsModalProps = { open: boolean; onClose: () => void };

export function AppsModal(props: AppsModalProps) {
  const { activeComputerId } = useDaemon();
  return <AppsDialog key={activeComputerId} {...props} />;
}

function AppsDialog({
  open,
  onClose
}: AppsModalProps) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const { servers, catalog, error, reload } = useMcp(open);
  const [query, setQuery] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [group, setGroup] = useState("all");
  const [busy, setBusy] = useState<string | null>(null);
  const c = useComposio(open, query, group);
  const [selected, setSelected] = useState<{ slug: string; name: string; toolkit?: ComposioToolkit } | null>(null);
  const [linking, setLinking] = useState<string | null>(null);
  const findInput = useRef<HTMLInputElement>(null);
  const accountSection = useRef<HTMLDivElement>(null);
  const linked = c.status?.configured === true && !c.status.login_expired;
  const selectedApp = c.apps.find((app) => app.slug === selected?.slug);
  // A hosted app consent request, separate from sign-in to the hub.
  const [flow, setFlow] = useState<FlowState | null>(null);
  const flowCancel = useRef<FlowCancel>({ cancelled: false });
  useEffect(() => () => {
    flowCancel.current.cancelled = true;
  }, []);
  // Pending sign-ins by server; the flag stops the poll and cancels the flow.
  const [signing, setSigning] = useState<Record<string, SignInControl>>({});
  // The sign-in shown in the status card at the top, and whether its app has a test to run after.
  const [signPanel, setSignPanel] = useState<{ state: SignInState; title: string; test: boolean } | null>(null);
  // Servers this session created for a sign-in that has not finished: they are tiles with a
  // Cancel, never "Connected", and are removed again when the sign-in is cancelled or expires.
  const [fresh, setFresh] = useState<Record<string, true>>({});
  // What the connection test of each connected app found ("Connected as <account>").
  const [tests, setTests] = useState<Record<string, AppTestState>>({});
  const testing = useRef<Set<string>>(new Set());
  const seenApps = useRef<typeof c.apps>([]);

  const testApp = (slug: string) => {
    if (!client || testing.current.has(slug)) {
      return;
    }
    testing.current.add(slug);
    void runAppTest(
      {
        test: (s) => client.composioTest(s),
        describe: (e) => {
          const h = humanError(e);
          return { title: h.title, hint: h.hint, details: h.details };
        }
      },
      slug,
      (s, state) => setTests((m) => ({ ...m, [s]: state }))
    ).then((state) => {
      if (state.status === "failed") void Promise.all([c.reloadApps(), c.reloadStatus()]);
    }).finally(() => testing.current.delete(slug));
  };

  // An app that has just finished signing in is tested by itself; a disconnected one forgets its test.
  useEffect(() => {
    const now = appsToTestNow(seenApps.current, c.apps, tests);
    seenApps.current = c.apps;
    for (const slug of now) {
      testApp(slug);
    }
    setTests((m) => {
      const keep = Object.keys(m).filter((slug) => c.apps.some((a) => a.slug === slug));
      return keep.length === Object.keys(m).length ? m : Object.fromEntries(keep.map((k) => [k, m[k]]));
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `tests` only gates; the effect follows the app list
  }, [c.apps]);

  const signIn = async (name: string) => {
    if (!client || signing[name]) {
      return;
    }
    const control: SignInControl = { cancelled: false, pasted: null };
    const title = catalog.find((e) => e.id === name)?.name ?? name;
    const hasTest = Boolean(catalog.find((e) => e.id === name)?.install.test);
    const expectedHosts = expectedSignInHosts(
      servers.find((x) => x.name === name),
      catalog.find((e) => e.id === name)
    );
    setSigning((m) => ({ ...m, [name]: control }));
    // A daemon on another computer is finished by the redirect this app catches and forwards.
    const remote = client.hostName !== null || !isLocalBaseUrl(client.baseUrl);
    let signedIn = false;
    let failed = false;
    try {
      const last = await runSignIn(
        {
          remote,
          expectedHosts,
          start: (port) => client.startMcpAuth(name, port),
          status: () => client.mcpAuthStatus(name),
          complete: (url) => client.completeMcpAuth(name, url),
          cancel: () => client.cancelMcpAuth(name),
          open: (url) => openSignInLink(url),
          relay: oauthRelay(),
          sleep: (ms) => new Promise<void>((resolve) => setTimeout(resolve, ms)),
          now: () => Date.now()
        },
        name,
        (state) => setSignPanel({ state, title, test: hasTest }),
        control
      );
      signedIn = last.step === "done";
      failed = last.step === "failed";
      if (signedIn) {
        toast.show(t("apps.auth.signedInToast", { name }), "success");
      } else if (last.step === "expired") {
        toast.show(t("apps.auth.expiredToast", { name, minutes: SIGN_IN_EXPIRES_MINUTES }), "info");
      }
      // A finished, cancelled or expired flow needs no card; a failed one stays to say why.
      if (last.step !== "failed" && !(signedIn && hasTest)) {
        setSignPanel(null);
      }
    } finally {
      setSigning((m) => {
        const next = { ...m };
        delete next[name];
        return next;
      });
      // A connection nobody finished is not kept as a half-made server. A failed one stays, so
      // "Try again" has something to sign in to and its Sign in button is under Manage.
      // The client secret typed for it goes too, unless another app still uses it.
      if (!signedIn && !failed && fresh[name]) {
        await client.deleteMcpServer(name, { forgetClientSecret: true }).catch(() => undefined);
      }
      setFresh((m) => {
        const next = { ...m };
        delete next[name];
        return next;
      });
      await reload();
    }
  };

  /** Google app: save the client when one was typed, add the app (read-only), sign in. */
  const connectGoogle = async (
    entry: McpCatalogEntry,
    typed: { clientId: string; clientSecret: string } | null
  ) => {
    if (!client) {
      return;
    }
    try {
      await addGoogleApp(client, entry, typed, servers, ownClient);
    } catch (e) {
      toast.show(
        e instanceof GoogleClientError ? t(`apps.google.error.${e.field}`) : errorText(e),
        "danger"
      );
      return;
    }
    setFresh((m) => ({ ...m, [entry.id]: true }));
    await signIn(entry.id);
  };

  useEffect(() => {
    if (open) {
      setQuery("");
      setGroup("all");
      setSelected(null);
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
  // Apps that need the owner's own sign-in client (Google) have their own section under Advanced.
  const ownClient = useMemo(() => googleEntries(catalog), [catalog]);
  const mainCatalog = useMemo(() => catalog.filter((e) => !e.install.oauth_client), [catalog]);
  const pendingNames = useMemo(() => new Set([...Object.keys(signing), ...Object.keys(fresh)]), [signing, fresh]);
  const directShown = useMemo(
    () => filterDirect(directEntries(mainCatalog, servers.filter((s) => !pendingNames.has(s.name))), query, "all"),
    [mainCatalog, servers, query, pendingNames]
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
  const firstCatalogPage = !query.trim() && group === "all";
  const featured = linked && firstCatalogPage
    ? c.apps.filter((app) => app.state !== "not_connected")
    : [];
  const appBySlug = new Map(c.apps.map((app) => [app.slug, app]));
  const toolkits = withoutConnected(c.items, featured);
  const summary = (toolkit?: ComposioToolkit) => toolkit
    ? t(`apps.composioHub.summaries.${toolkit.slug}`, { defaultValue: toolkit.description })
    : undefined;

  const backToCatalog = () => {
    setSelected(null);
    requestAnimationFrame(() => findInput.current?.focus());
  };
  const focusAccount = () => {
    setSelected(null);
    setFlow(null);
    setAdvanced(false);
    requestAnimationFrame(() => accountSection.current?.focus());
  };
  const entryOf = (name: string) => catalog.find((e) => e.id === name);
  // A server is that catalog entry only when the transport is the same: an old stdio server that
  // happens to carry the name of a newer remote app (Google Drive) is not it.
  const entryForServer = (s: McpServerT) =>
    catalog.find((e) => e.id === s.name && e.install.transport === s.transport);

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

  const connectApp = (slug: string, name: string) => {
    if (!client || linking !== null) return;
    const cancel: FlowCancel = { cancelled: false };
    flowCancel.current = cancel;
    setLinking(slug);
    setTests((old) => {
      const next = { ...old };
      delete next[slug];
      return next;
    });
    void runConnectFlow(
      {
        configured: () => c.status?.configured === true,
        loginExpired: () => c.status?.login_expired === true,
        describe: (e) => {
          const h = humanError(e);
          return { title: h.title, hint: h.hint, details: h.details };
        },
        appConnect: (s) => client.composioConnect(s),
        open: openComposioLink
      },
      slug,
      name,
      (state) => {
        if (flowCancel.current === cancel && state.step !== "cancelled") setFlow(state);
      },
      cancel
    ).then(async (state) => {
      if (state.step === "opened") {
        c.setApps((prev) => upsertApp(prev, {
          slug, name, state: "pending", enabled: true, native_first: false,
          bots: prev.find((app) => app.slug === slug)?.bots ?? null, tools: 0
        }));
      } else if (state.hasLink) {
        try {
          await client.composioDisconnect(slug);
          c.setApps((prev) => prev.filter((app) => app.slug !== slug));
        } catch (e) {
          toast.show(errorText(e), "danger");
          await c.reloadApps();
        }
      }
      if (state.reason === "login_expired") await c.reloadStatus();
    }).finally(() => {
      if (flowCancel.current === cancel) setLinking(null);
    });
  };

  const disconnectApp = (slug: string, name: string, quiet = false) => {
    if (!client) return;
    void run(slug, async () => {
      const result = await client.composioDisconnect(slug);
      c.setApps((prev) => prev.filter((app) => app.slug !== slug));
      if (!result.remote_removed) {
        toast.show(t("apps.composio.disconnectedLocal", { name }), "info");
      } else if (!quiet) {
        toast.show(t("apps.disconnectedToast", { name }), "info");
      }
    });
  };

  const cancelFlow = (slug: string, name: string) => {
    setFlow(null);
    if (linking === slug) {
      // The in-flight request owns cleanup after it learns whether a link was made.
      flowCancel.current.cancelled = true;
    } else if (c.apps.some((app) => app.slug === slug && app.state === "pending")) {
      disconnectApp(slug, name, true);
    }
  };

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
      title={entryForServer(s)?.name}
      icon={entryForServer(s)?.icon ?? undefined}
      description={entryForServer(s)?.description}
      sample={Boolean(entryForServer(s)?.install.test)}
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
          signal.cancelled = true;
        }
      }}
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
      lastUserOfSecret={
        !!s.oauth?.client_secret_ref &&
        !servers.some((o) => o.name !== s.name && o.oauth?.client_secret_ref === s.oauth?.client_secret_ref)
      }
      onDisconnect={(forgetClientSecret) =>
        void run(s.name, async () => {
          await client?.deleteMcpServer(s.name, { forgetClientSecret });
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
          {selected ? (
            <>
              <Button size="sm" variant="ghost" onClick={backToCatalog}>
                <ArrowLeft aria-hidden="true" />
                {t("apps.composioHub.back")}
              </Button>
              <span className={styles.grow} />
            </>
          ) : (
          <>
          <div className={styles.find}>
            <Search aria-hidden="true" />
            <input
              ref={findInput}
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
            {t("apps.composioHub.mcpServers")}
          </button>
          </>
          )}
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

        <div className={styles.body} key={selected?.slug ?? (advanced ? "mcp" : "hub")}>
          {advanced && error ? (
            <p className={formStyles.error} role="alert">
              {error}
            </p>
          ) : null}

          {signPanel ? (
            <McpSignInPanel
              state={signPanel.state}
              title={signPanel.title}
              onCancel={() => {
                const control = signing[signPanel.state.name];
                if (control) control.cancelled = true;
              }}
              onPaste={(url) => {
                const control = signing[signPanel.state.name];
                if (control) control.pasted = url;
              }}
              onRetry={() => {
                const name = signPanel.state.name;
                setSignPanel(null);
                void signIn(name);
              }}
              onClose={() => setSignPanel(null)}
            >
              {signPanel.state.step === "done" && signPanel.test ? (
                <McpSampleTest client={client} name={signPanel.state.name} disabled={false} autoRun />
              ) : null}
            </McpSignInPanel>
          ) : null}

          {selected ? (
            <ComposioAppPage
              key={selected.slug}
              slug={selected.slug}
              name={selected.name}
              description={summary(selected.toolkit)}
              app={selectedApp}
              flow={flow?.slug === selected.slug && (
                flow.step !== "opened" || selectedApp?.state === "pending"
              ) ? flow : null}
              test={tests[selected.slug]}
              pollError={c.pollErrors[selected.slug]}
              linked={linked}
              busy={busy !== null || linking !== null}
              onAccount={focusAccount}
              onConnect={() => connectApp(selected.slug, selected.name)}
              onCancel={() => cancelFlow(selected.slug, selected.name)}
              onCloseFlow={() => setFlow(null)}
              onTest={() => testApp(selected.slug)}
              onSaveBots={(bots) => void run(selected.slug, async () => {
                if (!client) return;
                const next = await client.composioSetBots(selected.slug, bots);
                c.setApps((prev) => upsertApp(prev, next));
                toast.show(t("apps.access.savedToast", { name: selected.name }), "success");
              })}
              onEnabled={(enabled) => void run(selected.slug, async () => {
                if (!client) return;
                const next = await client.composioSetEnabled(selected.slug, enabled);
                c.setApps((prev) => upsertApp(prev, next));
              })}
              onDisconnect={() => disconnectApp(selected.slug, selected.name)}
            />
          ) : advanced ? (
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
            {ownClient.length > 0 ? (
              <GoogleApps
                entries={ownClient}
                servers={servers}
                busy={busy}
                onConnect={(entry, typed) => run(`google-${entry.id}`, () => connectGoogle(entry, typed))}
              />
            ) : null}
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
            {remoteServers.length > 0 || directShown.length > 0 || pendingNames.size > 0 ? (
              <section className={styles.section} aria-labelledby="apps-remote-h">
                <h3 className={styles.sectionLabel} id="apps-remote-h">{t("apps.composioHub.remoteServers")}</h3>
                <ul className={styles.grid}>
                  {remoteServers.map(renderServer)}
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
                                sig.cancelled = true;
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
                </ul>
              </section>
            ) : null}
            </>
          ) : (
            <>
              <div ref={accountSection} tabIndex={-1}>
                <ComposioAccount
                  status={c.status}
                  login={c.login}
                  startLogin={c.startLogin}
                  cancelLogin={c.cancelLogin}
                  disabled={linking !== null || busy !== null || (linked && c.apps.some((app) => app.state === "pending"))}
                  onChanged={async () => {
                    await Promise.all([c.reloadStatus(), c.reloadApps()]);
                    await c.retryCatalog();
                  }}
                />
                {c.statusError ? (
                  <div className={styles.actions}>
                    <p className={formStyles.error} role="alert">{c.statusError}</p>
                    <Button size="sm" variant="secondary" onClick={() => void c.reloadStatus()}>
                      {t("apps.composio.retry")}
                    </Button>
                  </div>
                ) : null}
              </div>
              {/home\s?assistant|smart\s?home|hass/i.test(query) || query.trim() === "" ? (
                <HomeAssistantCard />
              ) : null}
              {featured.length > 0 ? (
                <section className={styles.section} aria-labelledby="apps-connected-h">
                  <h3 className={styles.sectionLabel} id="apps-connected-h">{t("apps.composioHub.yourApps")}</h3>
                  <ul className={styles.grid}>
                    {featured.map((app) => {
                      const toolkit = c.items.find((item) => item.slug === app.slug);
                      return (
                        <ComposioCard key={app.slug} name={app.name} slug={app.slug} app={app}
                          description={app.account ?? summary(toolkit)}
                          onOpen={() => setSelected({ name: app.name, slug: app.slug, toolkit })} />
                      );
                    })}
                  </ul>
                </section>
              ) : null}
              <section className={styles.section} aria-labelledby="apps-all-h">
                <h3 className={styles.sectionLabel} id="apps-all-h">{t("apps.catalogSection")}</h3>
                <GroupChips value={group} onChange={setGroup} />
                {firstLoad ? (
                  <SkeletonTiles />
                ) : c.error && c.items.length === 0 ? (
                  <p className={own.quiet} role="alert">
                    {t("apps.composio.loadFailed")}
                    <Button size="sm" variant="secondary" onClick={() => void c.retryCatalog()}>
                      {t("apps.composio.retry")}
                    </Button>
                  </p>
                ) : toolkits.length === 0 && featured.length === 0 ? (
                  <p className={styles.empty}><Search aria-hidden="true" />{t("apps.composio.noResults")}</p>
                ) : (
                  <ul className={styles.grid} data-testid="apps-grid">
                    {toolkits.map((toolkit) => (
                      <ComposioCard key={toolkit.slug} name={toolkit.name} slug={toolkit.slug}
                        app={linked ? appBySlug.get(toolkit.slug) : undefined} description={summary(toolkit)}
                        onOpen={() => setSelected({ name: toolkit.name, slug: toolkit.slug, toolkit })} />
                    ))}
                  </ul>
                )}
                {catalogError && (catalogError !== "unauthorized" || linked) ? (
                  <p className={own.quiet} data-testid="apps-catalog-notice" role="status">
                    {t(`apps.composio.catalogError.${catalogError}`)}
                    {catalogError !== "unauthorized" ? (
                      <Button size="sm" variant="ghost" onClick={() => void c.retryCatalog()}>
                        {t("apps.composio.retry")}
                      </Button>
                    ) : null}
                  </p>
                ) : null}
                {c.hasMore ? (
                  <div className={styles.actions}>
                    <Button size="sm" variant="secondary" disabled={c.loading} onClick={() => void c.loadMore()}>
                      {t("apps.composio.loadMore")}
                    </Button>
                  </div>
                ) : null}
              </section>
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}
