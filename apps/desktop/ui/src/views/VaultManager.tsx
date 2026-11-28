import {
  AtSign,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Eye,
  EyeOff,
  FileUp,
  Globe,
  KeyRound,
  Mail,
  Pencil,
  Plus,
  Search,
  ShieldCheck,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
import {
  type FormEvent,
  type KeyboardEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import { ApiError } from "../api/client";
import type { EmailAccount, FillGrantInfo, FillSecretInfo, VaultKind } from "../api/types";
import { BrandIcon } from "../components/BrandIcon";
import { ConfirmDialog } from "../components/DialogParts";
import formStyles from "../components/Form.module.css";
import { Popover } from "../components/Popover";
import { Segmented, Switch } from "../components/SettingsUi";
import { useFaviconUrl } from "../components/SiteFavicon";
import { useToast } from "../components/Toast";
import { botView } from "../lib/botProfile";
import { brandIcon } from "../lib/brandIcons";
import { humanLine } from "../lib/humanError";
import { isValidSecretName } from "../lib/secretDetect";
import {
  brandQueryOf,
  filterVaultItems,
  groupGrants,
  isLastGrant,
  type GrantGroup,
  normalizeOrigin,
  pinnedBotIdentities,
  siteLabelOf,
  suggestSecretName,
  type VaultTab,
  vaultFaviconDomain,
} from "../lib/vault";
import { useDaemon } from "../state/DaemonContext";
import { VaultCsvImport } from "./VaultCsvImport";
import { VaultGrantsPanel } from "./VaultGrantsPanel";
import styles from "./VaultManager.module.css";

const CLIPBOARD_CLEAR_SECS = 30;
const REVEAL_HIDE_SECS = 30;

/**
 * The clipboard wipe outlives the Vault page: leaving Settings (which unmounts the page) right after
 * copying a password must not keep the password on the clipboard. Only the countdown is per page.
 * Each copy restarts the 30 s; like before, whatever is on the clipboard then is cleared.
 */
let clipboardClearTimer: number | undefined;

function scheduleClipboardClear(): void {
  window.clearTimeout(clipboardClearTimer);
  clipboardClearTimer = window.setTimeout(() => {
    void navigator.clipboard.writeText("").catch(() => undefined);
  }, CLIPBOARD_CLEAR_SECS * 1000);
}

type DrawerMode =
  | { kind: "empty" }
  | { kind: "view"; name: string }
  | { kind: "add"; itemKind: VaultKind }
  | { kind: "edit"; item: FillSecretInfo }
  | { kind: "import" }
  | { kind: "grants" };

/**
 * A login's tile: the app's own brand icon when we ship one, else the site's favicon (asked from the
 * daemon's local cache by bare host name: no path, query or tracking parameters ever leave the app,
 * and the daemon only goes to the network for a site a bot may already visit), else its initial.
 */
function SiteTile({ item }: { item: Pick<FillSecretInfo, "name" | "site_name" | "origins" | "kind"> }) {
  const brandKey = brandQueryOf(item);
  const brand = brandIcon(brandKey);
  const domain = brand.known || item.kind === "other" ? null : vaultFaviconDomain(item.origins);
  const favicon = useFaviconUrl(domain ?? "", domain !== null);
  if (brand.known) {
    return <BrandIcon name={brandKey} small />;
  }
  if (favicon.src && !favicon.failed) {
    return (
      <span className={`${styles.rowTile} ${styles.rowTileIcon}`} aria-hidden="true">
        <img src={favicon.src} alt="" draggable={false} onError={favicon.onError} data-testid="vault-favicon" />
      </span>
    );
  }
  const initial = siteLabelOf(item).replace(/^https?:\/\//i, "").replace(/^www\./i, "").trim().charAt(0).toLocaleUpperCase();
  return (
    <span className={styles.rowTile} aria-hidden="true">
      {item.kind === "other" ? <KeyRound /> : /\p{L}|\p{N}/u.test(initial) ? initial : <Globe />}
    </span>
  );
}

/**
 * Vault ("Kasa"), its own page in Settings: bot-usable accounts and secrets.
 * - Pinned bot e-mail identity at the top with a Manage button
 * - Tabs (All / Logins / Other), search, New item menu (+ CSV import)
 * - Master-detail drawer to view, add, edit, delete, toggle per-bot access,
 *   and toggle "Ask before use"
 * - Passwords hidden by default; Reveal & Copy require owner confirmation,
 *   auto-hide and auto-clear the clipboard, and are never logged.
 */
export function VaultManager({ onManageEmail }: { onManageEmail: () => void }) {
  const { t } = useTranslation();
  const { client, bots } = useDaemon();
  const toast = useToast();

  const [items, setItems] = useState<FillSecretInfo[]>([]);
  const [emailAccounts, setEmailAccounts] = useState<EmailAccount[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tab, setTab] = useState<VaultTab>("all");
  const [query, setQuery] = useState("");
  const [mode, setMode] = useState<DrawerMode>({ kind: "empty" });
  const [newMenuOpen, setNewMenuOpen] = useState(false);
  const [grants, setGrants] = useState<FillGrantInfo[]>([]);
  const [grantsLoading, setGrantsLoading] = useState(false);
  const [grantsError, setGrantsError] = useState<string | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState<{ grant: FillGrantInfo; group: GrantGroup; last: boolean } | null>(null);
  const [revokeBusy, setRevokeBusy] = useState<string | null>(null);
  // Mode to go back to when the standing grants view is closed.
  const grantsReturn = useRef<DrawerMode>({ kind: "empty" });
  const newBtnRef = useRef<HTMLButtonElement | null>(null);

  // Ephemeral reveal / clipboard auto-clear state (never persisted, wiped on close/switch).
  const [revealed, setRevealed] = useState<{ name: string; value: string } | null>(null);
  const [revealLocalText, setRevealLocalText] = useState(false);
  const [confirmReveal, setConfirmReveal] = useState<{ name: string; copyAfter: boolean } | null>(null);
  const [revealBusy, setRevealBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<FillSecretInfo | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [clipSecs, setClipSecs] = useState<number | null>(null);
  const [savingBots, setSavingBots] = useState(false);
  const [formBusy, setFormBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  // Add/Edit form fields
  const [fKind, setFKind] = useState<VaultKind>("login");
  const [fSite, setFSite] = useState("");
  const [fOrigin, setFOrigin] = useState("");
  const [fLoginId, setFLoginId] = useState("");
  const [fName, setFName] = useState("");
  const [fNameTouched, setFNameTouched] = useState(false);
  const [fNote, setFNote] = useState("");
  const [fBots, setFBots] = useState<string[]>([]);
  const [fAsk, setFAsk] = useState(true);
  const secretInputRef = useRef<HTMLInputElement | null>(null);

  const revealTimer = useRef<number | undefined>(undefined);
  const clipInterval = useRef<number | undefined>(undefined);

  const clearRevealed = useCallback(() => {
    window.clearTimeout(revealTimer.current);
    setRevealed(null);
  }, []);

  const clearClipboardCountdown = useCallback(() => {
    window.clearInterval(clipInterval.current);
    setClipSecs(null);
  }, []);

  const wipeSecretInput = useCallback(() => {
    if (secretInputRef.current) secretInputRef.current.value = "";
    setRevealLocalText(false);
  }, []);

  const load = useCallback(async () => {
    if (!client) return;
    setLoading(true);
    setLoadError(null);
    try {
      const [secretsList, emails] = await Promise.all([
        client.listFillSecrets(),
        client.listEmailAccounts().catch(() => [] as EmailAccount[]),
      ]);
      setItems(secretsList);
      setEmailAccounts(emails);
      setMode((cur) => {
        if (cur.kind === "view" && !secretsList.some((s) => s.name === cur.name)) {
          return secretsList[0] ? { kind: "view", name: secretsList[0].name } : { kind: "empty" };
        }
        if (cur.kind === "empty" && secretsList[0]) {
          return { kind: "view", name: secretsList[0].name };
        }
        return cur;
      });
    } catch (e) {
      setLoadError(humanLine(e));
    } finally {
      setLoading(false);
    }
  }, [client]);

  const loadGrants = useCallback(async () => {
    if (!client) return;
    setGrantsLoading(true);
    setGrantsError(null);
    try {
      setGrants(await client.listFillGrants());
    } catch (e) {
      setGrantsError(humanLine(e));
    } finally {
      setGrantsLoading(false);
    }
  }, [client]);

  // The page opens fresh each time; leaving it drops every revealed value with the component state.
  useEffect(() => {
    void load();
  }, [load]);

  // A copy that finishes after the page is gone still gets its clipboard wipe, but no countdown.
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      window.clearTimeout(revealTimer.current);
      window.clearInterval(clipInterval.current);
    };
  }, []);

  const visibleItems = useMemo(() => filterVaultItems(items, tab, query), [items, tab, query]);
  const loginCount = useMemo(() => items.filter((i) => i.kind === "login").length, [items]);
  const otherCount = useMemo(() => items.filter((i) => i.kind === "other").length, [items]);
  const pinnedIdentities = useMemo(
    () => pinnedBotIdentities(emailAccounts, bots),
    [emailAccounts, bots],
  );
  const selectedItem = useMemo(
    () => (mode.kind === "view" ? items.find((i) => i.name === mode.name) ?? null : null),
    [items, mode],
  );

  const selectItem = (name: string) => {
    clearRevealed();
    wipeSecretInput();
    setFormError(null);
    setMode({ kind: "view", name });
  };

  const startAdd = (itemKind: VaultKind) => {
    clearRevealed();
    wipeSecretInput();
    setNewMenuOpen(false);
    setFormError(null);
    setFKind(itemKind);
    setFSite("");
    setFOrigin("");
    setFLoginId("");
    setFName(suggestSecretName("", itemKind, items.map((i) => i.name)));
    setFNameTouched(false);
    setFNote("");
    setFBots([]);
    setFAsk(true);
    setMode({ kind: "add", itemKind });
  };

  const startEdit = (item: FillSecretInfo) => {
    clearRevealed();
    wipeSecretInput();
    setFormError(null);
    setFKind(item.kind);
    setFSite(item.site_name ?? "");
    setFOrigin(item.origins.join(", "));
    setFLoginId(item.login_id ?? "");
    setFName(item.name);
    setFNameTouched(true);
    setFNote(item.note ?? "");
    setFBots([...item.bot_ids]);
    setFAsk(item.ask_before_use);
    setMode({ kind: "edit", item });
  };

  const copyWithAutoClear = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      scheduleClipboardClear();
      if (!mounted.current) return;
      clearClipboardCountdown();
      setClipSecs(CLIPBOARD_CLEAR_SECS);
      clipInterval.current = window.setInterval(() => {
        setClipSecs((s) => {
          if (s !== null && s > 1) return s - 1;
          window.clearInterval(clipInterval.current);
          return null;
        });
      }, 1000);
      toast.show(t("vault.detail.copiedCleared", { what: label, seconds: CLIPBOARD_CLEAR_SECS }), "success");
    } catch {
      toast.show(t("vault.detail.copyFailed"), "danger");
    }
  };

  const doReveal = async () => {
    if (!client || !confirmReveal || revealBusy) return;
    const { name, copyAfter } = confirmReveal;
    setRevealBusy(true);
    try {
      const res = await client.revealFillSecret(name);
      setConfirmReveal(null);
      if (copyAfter) {
        await copyWithAutoClear(res.value, t("vault.detail.password"));
        res.value = "";
      } else {
        clearRevealed();
        setRevealed({ name: res.name, value: res.value });
        revealTimer.current = window.setTimeout(() => {
          setRevealed(null);
        }, REVEAL_HIDE_SECS * 1000);
      }
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setRevealBusy(false);
    }
  };

  const doDelete = async () => {
    if (!client || !confirmDelete || deleteBusy) return;
    setDeleteBusy(true);
    try {
      await client.deleteFillSecret(confirmDelete.name);
      clearRevealed();
      setConfirmDelete(null);
      setMode({ kind: "empty" });
      await load();
      toast.show(t("vault.detail.deleted"), "success");
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setDeleteBusy(false);
    }
  };

  const updateItemInline = async (
    item: FillSecretInfo,
    patch: { bot_ids?: string[]; ask_before_use?: boolean },
  ) => {
    if (!client || savingBots) return;
    setSavingBots(true);
    try {
      const updated = await client.updateFillSecret(item.name, patch);
      setItems((cur) => cur.map((x) => (x.name === updated.name ? updated : x)));
      if (patch.ask_before_use !== undefined) void loadGrants();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setSavingBots(false);
    }
  };

  const openGrants = () => {
    clearRevealed();
    wipeSecretInput();
    setFormError(null);
    grantsReturn.current = mode.kind === "grants" ? grantsReturn.current : mode;
    setNewMenuOpen(false);
    setMode({ kind: "grants" });
    void loadGrants();
  };

  const doRevoke = async () => {
    if (!client || !confirmRevoke || revokeBusy !== null) return;
    const { grant } = confirmRevoke;
    setRevokeBusy(grant.id);
    try {
      await client.revokeFillGrant(grant.id);
      setConfirmRevoke(null);
      await Promise.all([load(), loadGrants()]);
      toast.show(t("vault.grants.revoked"), "success");
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        setConfirmRevoke(null);
        await Promise.all([load(), loadGrants()]);
        toast.show(t("vault.grants.revoked"), "success");
      } else {
        toast.show(humanLine(e), "danger");
        void loadGrants();
      }
    } finally {
      setRevokeBusy(null);
    }
  };

  const submitForm = async (e: FormEvent) => {
    e.preventDefault();
    if (!client || formBusy) return;
    setFormError(null);

    const rawOrigins = fOrigin
      .split(/[\s,]+/)
      .map((s) => s.trim())
      .filter(Boolean);
    const origins: string[] = [];
    for (const o of rawOrigins) {
      const norm = normalizeOrigin(o);
      if (!norm) {
        setFormError(t("vault.form.badOrigin", { origin: o }));
        return;
      }
      if (!origins.includes(norm)) origins.push(norm);
    }

    const secretVal = secretInputRef.current?.value ?? "";
    const isAdd = mode.kind === "add";
    const targetName = isAdd ? fName.trim() : mode.kind === "edit" ? mode.item.name : "";
    if (!isValidSecretName(targetName)) {
      setFormError(t("vault.form.badKey"));
      return;
    }
    if (isAdd && !secretVal) {
      setFormError(t("vault.form.needValue"));
      return;
    }

    setFormBusy(true);
    try {
      const saved = isAdd
        ? await client.createFillSecret({
            name: targetName,
            value: secretVal,
            kind: fKind,
            site_name: fSite.trim() || undefined,
            login_id: fLoginId.trim() || undefined,
            note: fNote.trim() || undefined,
            origins,
            bot_ids: fBots,
            ask_before_use: fAsk,
          })
        : await client.updateFillSecret(targetName, {
            ...(secretVal ? { value: secretVal } : {}),
            kind: fKind,
            site_name: fSite.trim(),
            login_id: fLoginId.trim(),
            note: fNote.trim(),
            origins,
            bot_ids: fBots,
            ask_before_use: fAsk,
          });
      wipeSecretInput();
      await load();
      setMode({ kind: "view", name: saved.name });
      toast.show(t(isAdd ? "vault.form.created" : "vault.form.saved"), "success");
    } catch (err) {
      setFormError(humanLine(err));
    } finally {
      setFormBusy(false);
    }
  };

  const onListKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    if (visibleItems.length === 0) return;
    e.preventDefault();
    const currentIdx = selectedItem ? visibleItems.findIndex((x) => x.name === selectedItem.name) : -1;
    const nextIdx =
      e.key === "ArrowDown"
        ? (currentIdx + 1) % visibleItems.length
        : (currentIdx - 1 + visibleItems.length) % visibleItems.length;
    const next = visibleItems[nextIdx];
    selectItem(next.name);
    e.currentTarget
      .querySelector<HTMLElement>(`[data-vault-row="${CSS.escape(next.name)}"]`)
      ?.focus();
  };

  const primaryIdentity = pinnedIdentities[0] ?? null;

  return (
    <>
      <div className={styles.shell} data-testid="vault-manager">
        {/* Pinned bot e-mail identity */}
        <div className={styles.identityStrip} data-testid="vault-bot-email-identity">
          <div className={styles.identityLeft}>
            <span className={styles.identityIcon} aria-hidden="true">
              {primaryIdentity?.account.provider === "agentmail" ? <AtSign /> : <Mail />}
            </span>
            <div className={styles.identityMeta}>
              <span className={styles.identityLabel}>{t("vault.identity.label")}</span>
              {primaryIdentity ? (
                <div className={styles.identityAddressRow}>
                  <span className={styles.identityAddress} title={primaryIdentity.account.address}>
                    {primaryIdentity.account.address}
                  </span>
                  {primaryIdentity.botName ? (
                    <span className={styles.pill}>{primaryIdentity.botName}</span>
                  ) : null}
                  {pinnedIdentities.length > 1 ? (
                    <span className={styles.pill}>+{pinnedIdentities.length - 1}</span>
                  ) : null}
                </div>
              ) : (
                <span className={styles.identityEmpty}>{t("vault.identity.none")}</span>
              )}
            </div>
          </div>
          <Button
            variant="secondary"
            size="sm"
            data-testid="vault-manage-email"
            onClick={onManageEmail}
          >
            {t("vault.identity.manage")}
          </Button>
        </div>

        <div className={styles.toolbar}>
          <Segmented<VaultTab>
            label={t("vault.tabs.label")}
            value={tab}
            onChange={setTab}
            options={[
              { value: "all", label: `${t("vault.tabs.all")} (${items.length})`, testId: "vault-tab-all" },
              { value: "login", label: `${t("vault.tabs.logins")} (${loginCount})`, testId: "vault-tab-login" },
              { value: "other", label: `${t("vault.tabs.other")} (${otherCount})`, testId: "vault-tab-other" },
            ]}
          />
          <div className={styles.searchWrap}>
            <Search className={styles.searchIcon} aria-hidden="true" />
            <input
              className={styles.searchInput}
              type="search"
              data-testid="vault-search"
              value={query}
              placeholder={t("vault.searchPlaceholder")}
              aria-label={t("vault.searchPlaceholder")}
              onChange={(e) => setQuery(e.target.value)}
            />
            {query ? (
              <button
                type="button"
                className={styles.searchClear}
                aria-label={t("vault.actions.clear")}
                title={t("vault.actions.clear")}
                onClick={() => setQuery("")}
              >
                <X aria-hidden="true" />
              </button>
            ) : null}
          </div>
          <div className={styles.toolbarActions}>
            <Button variant="secondary" size="sm" data-testid="vault-grants-open" onClick={openGrants}>
              <ShieldCheck aria-hidden="true" />
              {t("vault.grants.open")}
            </Button>
            <Button
              ref={newBtnRef}
              variant="primary"
              size="sm"
              data-testid="vault-new-item"
              aria-haspopup="menu"
              aria-expanded={newMenuOpen}
              onClick={() => setNewMenuOpen((o) => !o)}
            >
              <Plus aria-hidden="true" />
              {t("vault.newItem")}
              <ChevronDown aria-hidden="true" />
            </Button>
            <Popover
              open={newMenuOpen}
              anchor={newBtnRef}
              onClose={() => setNewMenuOpen(false)}
              placement="bottom-end"
              label={t("vault.newItem")}
              width={220}
            >
              <div className={styles.menuList}>
                <button
                  type="button"
                  role="menuitem"
                  className={styles.menuItem}
                  data-testid="vault-new-login"
                  onClick={() => startAdd("login")}
                >
                  <Globe aria-hidden="true" />
                  <span>{t("vault.menu.login")}</span>
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className={styles.menuItem}
                  data-testid="vault-new-other"
                  onClick={() => startAdd("other")}
                >
                  <KeyRound aria-hidden="true" />
                  <span>{t("vault.menu.other")}</span>
                </button>
                <div className={styles.menuDivider} />
                <button
                  type="button"
                  role="menuitem"
                  className={styles.menuItem}
                  data-testid="vault-import-csv"
                  onClick={() => {
                    clearRevealed();
                    wipeSecretInput();
                    setNewMenuOpen(false);
                    setMode({ kind: "import" });
                  }}
                >
                  <FileUp aria-hidden="true" />
                  <span>{t("vault.menu.importCsv")}</span>
                </button>
              </div>
            </Popover>
          </div>
        </div>

        <div className={styles.body}>
          <div
            className={styles.listPane}
            role="group"
            aria-label={t("vault.listLabel")}
            onKeyDown={onListKeyDown}
          >
            {loadError ? (
              <div className={styles.emptyPane}>
                <TriangleAlert aria-hidden="true" />
                <p>{loadError}</p>
                <Button variant="secondary" size="sm" onClick={() => void load()}>
                  {t("vault.actions.retry")}
                </Button>
              </div>
            ) : loading && items.length === 0 ? (
              <div className={styles.emptyPane}>
                <p>{t("vault.actions.loading")}</p>
              </div>
            ) : visibleItems.length === 0 ? (
              <div className={styles.emptyPane} data-testid="vault-empty">
                <KeyRound aria-hidden="true" />
                <p>{items.length === 0 ? t("vault.empty.none") : t("vault.empty.noMatches")}</p>
                {items.length === 0 ? (
                  <div className={styles.emptyActions}>
                    <Button variant="primary" size="sm" onClick={() => startAdd("login")}>
                      <Plus aria-hidden="true" />
                      {t("vault.menu.login")}
                    </Button>
                    <Button variant="secondary" size="sm" onClick={() => setMode({ kind: "import" })}>
                      <FileUp aria-hidden="true" />
                      {t("vault.menu.importCsv")}
                    </Button>
                  </div>
                ) : null}
              </div>
            ) : (
              visibleItems.map((item) => {
                const active = selectedItem?.name === item.name && mode.kind === "view";
                const title = siteLabelOf(item);
                const sub = item.login_id?.trim() || item.origins[0] || item.name;
                return (
                  <button
                    key={item.name}
                    type="button"
                    aria-current={active ? "true" : undefined}
                    data-vault-row={item.name}
                    data-testid={`vault-row-${item.name}`}
                    className={[styles.row, active ? styles.rowActive : ""].filter(Boolean).join(" ")}
                    onClick={() => selectItem(item.name)}
                  >
                    <SiteTile item={item} />
                    <span className={styles.rowText}>
                      <span className={styles.rowTitle}>
                        <span>{title}</span>
                        {item.bot_ids.length === 0 ? (
                          <span
                            className={styles.unscopedDot}
                            role="img"
                            title={t("vault.noBotAssigned")}
                            aria-label={t("vault.noBotAssigned")}
                          />
                        ) : null}
                      </span>
                      <span className={styles.rowSub}>{sub}</span>
                    </span>
                    <ChevronRight className={styles.rowChevron} aria-hidden="true" />
                  </button>
                );
              })
            )}
          </div>

          {/* Right detail drawer */}
          {mode.kind === "import" ? (
            <VaultCsvImport
              existingNames={items.map((i) => i.name)}
              bots={bots}
              onImported={load}
              onCancel={() => setMode(items[0] ? { kind: "view", name: items[0].name } : { kind: "empty" })}
            />
          ) : mode.kind === "grants" ? (
            <VaultGrantsPanel
              groups={groupGrants(grants, items)}
              loading={grantsLoading}
              error={grantsError}
              busyId={revokeBusy}
              onRetry={() => void loadGrants()}
              onRevoke={(grant, group) =>
                setConfirmRevoke({ grant, group, last: isLastGrant(grants, grant) })
              }
              onBack={() => {
                const back = grantsReturn.current;
                const first: DrawerMode = items[0] ? { kind: "view", name: items[0].name } : { kind: "empty" };
                const keep =
                  back.kind === "view" ? items.some((i) => i.name === back.name) : back.kind !== "empty" && back.kind !== "grants";
                setMode(keep ? back : first);
              }}
            />
          ) : mode.kind === "add" || mode.kind === "edit" ? (
            <form
              className={styles.drawer}
              data-testid="vault-form-drawer"
              autoComplete="off"
              onSubmit={(e) => void submitForm(e)}
            >
              <div className={styles.drawerHead}>
                <h3 className={styles.drawerTitle}>
                  {mode.kind === "add" ? t("vault.form.addTitle") : t("vault.form.editTitle")}
                </h3>
                <Segmented<VaultKind>
                  label={t("vault.form.kindLabel")}
                  value={fKind}
                  onChange={(k) => {
                    setFKind(k);
                    if (mode.kind === "add" && !fNameTouched) {
                      setFName(suggestSecretName(fSite || fOrigin, k, items.map((i) => i.name)));
                    }
                  }}
                  options={[
                    { value: "login", label: t("vault.tabs.logins") },
                    { value: "other", label: t("vault.tabs.other") },
                  ]}
                />
              </div>

              <div className={styles.form}>
                <div className={formStyles.field}>
                  <label className={formStyles.label} htmlFor="vault-site">
                    {t("vault.form.siteLabel")}
                  </label>
                  <input
                    id="vault-site"
                    className={formStyles.input}
                    value={fSite}
                    placeholder={t("vault.form.sitePlaceholder")}
                    onChange={(e) => {
                      const val = e.target.value;
                      setFSite(val);
                      if (mode.kind === "add" && !fNameTouched) {
                        setFName(suggestSecretName(val || fOrigin, fKind, items.map((i) => i.name)));
                      }
                    }}
                  />
                </div>

                <div className={formStyles.field}>
                  <label className={formStyles.label} htmlFor="vault-origin">
                    {t("vault.form.originLabel")}
                  </label>
                  <input
                    id="vault-origin"
                    className={formStyles.input}
                    value={fOrigin}
                    placeholder="https://github.com"
                    spellCheck={false}
                    onChange={(e) => {
                      const val = e.target.value;
                      setFOrigin(val);
                      if (mode.kind === "add" && !fNameTouched && !fSite.trim()) {
                        setFName(suggestSecretName(val, fKind, items.map((i) => i.name)));
                      }
                    }}
                  />
                  <span className={formStyles.hint}>{t("vault.form.originHint")}</span>
                </div>

                <div className={formStyles.field}>
                  <label className={formStyles.label} htmlFor="vault-login-id">
                    {t("vault.form.loginLabel")}
                  </label>
                  <input
                    id="vault-login-id"
                    className={formStyles.input}
                    value={fLoginId}
                    placeholder={t("vault.form.loginPlaceholder")}
                    autoComplete="off"
                    onChange={(e) => setFLoginId(e.target.value)}
                  />
                </div>

                <div className={formStyles.field}>
                  <label className={formStyles.label} htmlFor="vault-secret-val">
                    {t("vault.form.secretLabel")}
                  </label>
                  <div className={styles.valueBar}>
                    <input
                      id="vault-secret-val"
                      ref={secretInputRef}
                      className={styles.monoInput}
                      type={revealLocalText ? "text" : "password"}
                      autoComplete="off"
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      placeholder={
                        mode.kind === "edit"
                          ? t("vault.form.secretKeepPlaceholder")
                          : t("vault.form.secretPlaceholder")
                      }
                    />
                    <button
                      type="button"
                      className={styles.iconBtn}
                      aria-label={revealLocalText ? t("vault.detail.hide") : t("vault.detail.showTyped")}
                      title={revealLocalText ? t("vault.detail.hide") : t("vault.detail.showTyped")}
                      onClick={() => setRevealLocalText((v) => !v)}
                    >
                      {revealLocalText ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
                    </button>
                  </div>
                </div>

                {mode.kind === "add" ? (
                  <div className={formStyles.field}>
                    <label className={formStyles.label} htmlFor="vault-ref-name">
                      {t("vault.form.refLabel")}
                    </label>
                    <input
                      id="vault-ref-name"
                      className={formStyles.input}
                      value={fName}
                      spellCheck={false}
                      onChange={(e) => {
                        setFNameTouched(true);
                        setFName(e.target.value);
                      }}
                    />
                    <span className={formStyles.hint}>{t("vault.form.refHint")}</span>
                  </div>
                ) : null}

                <div className={formStyles.field}>
                  <label className={formStyles.label} htmlFor="vault-note">
                    {t("vault.form.noteLabel")}
                  </label>
                  <textarea
                    id="vault-note"
                    className={formStyles.textarea}
                    rows={2}
                    value={fNote}
                    placeholder={t("vault.form.notePlaceholder")}
                    onChange={(e) => setFNote(e.target.value)}
                  />
                </div>

                <div className={styles.card}>
                  <div className={styles.fieldRow}>
                    <span className={styles.fieldLabel}>{t("vault.detail.botsLabel")}</span>
                    {bots.length === 0 ? (
                      <span className={formStyles.hint}>{t("vault.detail.noBots")}</span>
                    ) : (
                      <ul className={styles.botList}>
                        {bots.map((b) => {
                          const view = botView(b);
                          const checked = fBots.includes(b.id);
                          return (
                            <li key={b.id}>
                              <label className={styles.botItem}>
                                <input
                                  type="checkbox"
                                  className={styles.botCheck}
                                  checked={checked}
                                  onChange={(e) =>
                                    setFBots((cur) =>
                                      e.target.checked
                                        ? [...cur, b.id]
                                        : cur.filter((id) => id !== b.id),
                                    )
                                  }
                                />
                                <BotAvatar bot={view} size="sm" decorative />
                                <span>{view.name}</span>
                              </label>
                            </li>
                          );
                        })}
                      </ul>
                    )}
                  </div>
                  <div className={styles.switchRow}>
                    <div className={styles.switchText}>
                      <span className={styles.switchTitle}>{t("vault.detail.askTitle")}</span>
                      <span className={styles.switchDesc}>
                        {fAsk ? t("vault.detail.askHintOn") : t("vault.detail.askHintOff")}
                      </span>
                    </div>
                    <Switch label={t("vault.detail.askTitle")} checked={fAsk} onChange={setFAsk} />
                  </div>
                </div>
              </div>

              {formError ? (
                <p className={formStyles.error} role="alert">
                  {formError}
                </p>
              ) : null}

              <div className={styles.formActions}>
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  disabled={formBusy}
                  onClick={() => {
                    wipeSecretInput();
                    setMode(
                      mode.kind === "edit"
                        ? { kind: "view", name: mode.item.name }
                        : items[0]
                          ? { kind: "view", name: items[0].name }
                          : { kind: "empty" },
                    );
                  }}
                >
                  {t("common.cancel")}
                </Button>
                <Button type="submit" variant="primary" size="sm" disabled={formBusy} data-testid="vault-save">
                  {formBusy ? t("vault.actions.saving") : t("common.save")}
                </Button>
              </div>
            </form>
          ) : selectedItem ? (
            <div className={styles.drawer} data-testid="vault-detail-drawer">
              <div className={styles.drawerHead}>
                <div className={styles.drawerTitleGroup}>
                  <SiteTile item={selectedItem} />
                  <div>
                    <h3 className={styles.drawerTitle}>{siteLabelOf(selectedItem)}</h3>
                    <span className={styles.drawerSub}>{selectedItem.name}</span>
                  </div>
                </div>
                <div className={styles.drawerActions}>
                  <Button
                    variant="secondary"
                    size="sm"
                    data-testid="vault-edit-btn"
                    onClick={() => startEdit(selectedItem)}
                  >
                    <Pencil aria-hidden="true" />
                    {t("common.edit")}
                  </Button>
                  <Button
                    variant="danger"
                    size="sm"
                    data-testid="vault-delete-btn"
                    onClick={() => setConfirmDelete(selectedItem)}
                  >
                    <Trash2 aria-hidden="true" />
                    {t("common.delete")}
                  </Button>
                </div>
              </div>

              <div className={styles.card}>
                {selectedItem.login_id ? (
                  <div className={styles.fieldRow}>
                    <span className={styles.fieldLabel}>{t("vault.detail.loginId")}</span>
                    <div className={styles.valueBar}>
                      <span className={styles.monoValue}>{selectedItem.login_id}</span>
                      <button
                        type="button"
                        className={styles.iconBtn}
                        onClick={() => void copyWithAutoClear(selectedItem.login_id ?? "", t("vault.detail.loginId"))}
                      >
                        <Copy aria-hidden="true" />
                        {t("vault.actions.copy")}
                      </button>
                    </div>
                  </div>
                ) : null}

                <div className={styles.fieldRow}>
                  <span className={styles.fieldLabel}>{t("vault.detail.password")}</span>
                  <div className={styles.valueBar}>
                    <span className={styles.monoValue} data-testid="vault-secret-display">
                      {revealed && revealed.name === selectedItem.name
                        ? revealed.value
                        : "••••••••••••"}
                    </span>
                    <div className={styles.inlineBtns}>
                      {revealed && revealed.name === selectedItem.name ? (
                        <button
                          type="button"
                          className={styles.iconBtn}
                          data-testid="vault-hide-btn"
                          onClick={clearRevealed}
                        >
                          <EyeOff aria-hidden="true" />
                          {t("vault.detail.hide")}
                        </button>
                      ) : (
                        <button
                          type="button"
                          className={styles.iconBtn}
                          data-testid="vault-reveal-btn"
                          onClick={() => setConfirmReveal({ name: selectedItem.name, copyAfter: false })}
                        >
                          <Eye aria-hidden="true" />
                          {t("vault.detail.reveal")}
                        </button>
                      )}
                      <button
                        type="button"
                        className={styles.iconBtn}
                        data-testid="vault-copy-secret-btn"
                        onClick={() => {
                          if (revealed && revealed.name === selectedItem.name) {
                            void copyWithAutoClear(revealed.value, t("vault.detail.password"));
                          } else {
                            setConfirmReveal({ name: selectedItem.name, copyAfter: true });
                          }
                        }}
                      >
                        {clipSecs !== null ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                        {clipSecs !== null
                          ? t("vault.detail.clearsIn", { seconds: clipSecs })
                          : t("vault.actions.copy")}
                      </button>
                    </div>
                  </div>
                  <span className={formStyles.hint}>{t("vault.detail.neverSentToModel")}</span>
                </div>

                {selectedItem.origins.length > 0 ? (
                  <div className={styles.fieldRow}>
                    <span className={styles.fieldLabel}>{t("vault.detail.origins")}</span>
                    <span className={styles.monoValue}>{selectedItem.origins.join(", ")}</span>
                  </div>
                ) : null}

                {selectedItem.programs.length > 0 ? (
                  <div className={styles.fieldRow}>
                    <span className={styles.fieldLabel}>{t("vault.detail.programs")}</span>
                    <span className={styles.monoValue}>{selectedItem.programs.join(", ")}</span>
                  </div>
                ) : null}

                {selectedItem.note ? (
                  <div className={styles.fieldRow}>
                    <span className={styles.fieldLabel}>{t("vault.detail.note")}</span>
                    <p className={styles.noteText}>{selectedItem.note}</p>
                  </div>
                ) : null}
              </div>

              <div className={styles.card}>
                <div className={styles.fieldRow}>
                  <span className={styles.fieldLabel}>{t("vault.detail.botsLabel")}</span>
                  {selectedItem.bot_ids.length === 0 ? (
                    <div className={styles.warnBox} role="status">
                      <TriangleAlert aria-hidden="true" />
                      <span>{t("vault.detail.noBotsGrantedWarning")}</span>
                    </div>
                  ) : null}
                  {bots.length === 0 ? (
                    <span className={formStyles.hint}>{t("vault.detail.noBots")}</span>
                  ) : (
                    <ul className={styles.botList}>
                      {bots.map((b) => {
                        const view = botView(b);
                        const checked = selectedItem.bot_ids.includes(b.id);
                        return (
                          <li key={b.id}>
                            <label className={styles.botItem}>
                              <input
                                type="checkbox"
                                className={styles.botCheck}
                                data-testid={`vault-bot-${b.id}`}
                                checked={checked}
                                disabled={savingBots}
                                onChange={(e) => {
                                  const next = e.target.checked
                                    ? [...selectedItem.bot_ids, b.id]
                                    : selectedItem.bot_ids.filter((id) => id !== b.id);
                                  void updateItemInline(selectedItem, { bot_ids: next });
                                }}
                              />
                              <BotAvatar bot={view} size="sm" decorative />
                              <span>{view.name}</span>
                            </label>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </div>

                <div className={styles.switchRow}>
                  <div className={styles.switchText}>
                    <span className={styles.switchTitle}>{t("vault.detail.askTitle")}</span>
                    <span className={styles.switchDesc}>
                      {selectedItem.ask_before_use
                        ? t("vault.detail.askHintOn")
                        : t("vault.detail.askHintOff")}
                    </span>
                  </div>
                  <Switch
                    label={t("vault.detail.askTitle")}
                    checked={selectedItem.ask_before_use}
                    disabled={savingBots}
                    onChange={(next) =>
                      void updateItemInline(selectedItem, { ask_before_use: next })
                    }
                  />
                </div>
                {!selectedItem.ask_before_use ? (
                  <button
                    type="button"
                    className={styles.linkBtn}
                    data-testid="vault-see-grants"
                    onClick={openGrants}
                  >
                    {t("vault.grants.seeGrants")}
                  </button>
                ) : null}
              </div>
            </div>
          ) : (
            <div className={styles.drawerEmpty}>
              <KeyRound aria-hidden="true" />
              <p>{t("vault.selectHint")}</p>
            </div>
          )}
        </div>
      </div>

      <ConfirmDialog
        open={confirmReveal !== null}
        title={t("vault.revealConfirm.title")}
        body={t("vault.revealConfirm.body")}
        confirmLabel={
          confirmReveal?.copyAfter
            ? t("vault.revealConfirm.copyConfirm")
            : t("vault.revealConfirm.revealConfirm")
        }
        busy={revealBusy}
        onConfirm={() => void doReveal()}
        onCancel={() => setConfirmReveal(null)}
      />

      <ConfirmDialog
        open={confirmDelete !== null}
        title={t("vault.deleteConfirm.title", { name: confirmDelete ? siteLabelOf(confirmDelete) : "" })}
        body={t("vault.deleteConfirm.body")}
        confirmLabel={t("common.delete")}
        tone="danger"
        busy={deleteBusy}
        onConfirm={() => void doDelete()}
        onCancel={() => setConfirmDelete(null)}
      />

      <ConfirmDialog
        open={confirmRevoke !== null}
        title={t("vault.grants.confirmTitle")}
        body={
          confirmRevoke
            ? t(confirmRevoke.last ? "vault.grants.confirmLastBody" : "vault.grants.confirmBody", {
                name: confirmRevoke.group.label,
                origin: confirmRevoke.grant.origin,
              })
            : ""
        }
        confirmLabel={t("vault.grants.revoke")}
        tone="danger"
        busy={revokeBusy !== null}
        onConfirm={() => void doRevoke()}
        onCancel={() => setConfirmRevoke(null)}
      />
    </>
  );
}
