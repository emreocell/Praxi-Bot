import { Download, PackageOpen, RefreshCw, Search, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError, type ApiClient } from "../api/client";
import type { MarketIndex, MarketInstalled, MarketPreview, MarketSkill } from "../api/types";
import { humanLine } from "../lib/humanError";
import {
  NOTHING_CONFIRMED,
  type ConfirmState,
  buildInstallBody,
  canConfirm,
  checkIndexUrl,
  filterByQuery,
  needsKey,
  riskOf,
  riskTone,
  sortedNeeds,
  sourceKey
} from "../lib/skillMarket";
import { Callout, ConfirmDialog, DialogFooter, DialogHeader, dialogStyles } from "./DialogParts";
import { Modal } from "./Modal";
import { Segmented } from "./SettingsUi";
import skillStyles from "./Skills.module.css";
import styles from "./SkillMarket.module.css";
import { useToast } from "./Toast";

type Tab = "installed" | "browse";

interface Review {
  name: string;
  update: boolean;
  preview: MarketPreview;
}

/**
 * Skill library (issue 206): what is installed, what can be added, and what each skill would ask
 * for BEFORE anything is installed. Install always goes through a review step with an explicit
 * button; a skill rated above Low and an update that asks for more each need their own tick. The
 * daemon enforces the same rules, this dialog only shows them.
 */
export function SkillMarketModal({
  open,
  client,
  onClose,
  onChanged
}: {
  open: boolean;
  client: ApiClient | null;
  onClose: () => void;
  /** Called after an install, update or removal so the skill lists reload. */
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} label={t("skillMarket.title")} size="md">
      {open ? <MarketDialog client={client} onClose={onClose} onChanged={onChanged} /> : null}
    </Modal>
  );
}

function MarketDialog({
  client,
  onClose,
  onChanged
}: {
  client: ApiClient | null;
  onClose: () => void;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const [tab, setTab] = useState<Tab>("installed");
  const [installed, setInstalled] = useState<MarketInstalled[] | null>(null);
  const [installedError, setInstalledError] = useState<string | null>(null);
  const [index, setIndex] = useState<MarketIndex | null>(null);
  const [indexError, setIndexError] = useState<string | null>(null);
  const [indexLoading, setIndexLoading] = useState(false);
  const [customUrl, setCustomUrl] = useState("");
  const [activeUrl, setActiveUrl] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [review, setReview] = useState<Review | null>(null);
  const [confirm, setConfirm] = useState<ConfirmState>(NOTHING_CONFIRMED);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [removing, setRemoving] = useState<{ name: string; usedBy: string[] } | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const loadInstalled = useCallback(async () => {
    if (!client) return;
    try {
      const rows = await client.marketInstalled();
      if (alive.current) {
        setInstalled(rows);
        setInstalledError(null);
      }
    } catch (e) {
      if (alive.current) setInstalledError(humanLine(e));
    }
  }, [client]);

  const loadIndex = useCallback(
    async (url: string | null) => {
      if (!client) return;
      setIndexLoading(true);
      setIndexError(null);
      try {
        const next = await client.marketIndex(url ?? undefined);
        if (alive.current) {
          setIndex(next);
          setActiveUrl(url);
        }
      } catch (e) {
        if (alive.current) setIndexError(humanLine(e));
      } finally {
        if (alive.current) setIndexLoading(false);
      }
    },
    [client]
  );

  useEffect(() => {
    void loadInstalled();
  }, [loadInstalled]);
  useEffect(() => {
    if (tab === "browse" && !index && !indexLoading && !indexError) void loadIndex(null);
  }, [tab, index, indexLoading, indexError, loadIndex]);

  const openReview = async (name: string, update: boolean) => {
    if (!client || busy) return;
    setBusy(name);
    setError(null);
    setNotice(null);
    try {
      const res = await client.marketPreview({
        name,
        update,
        ...(!update && activeUrl ? { index_url: activeUrl } : {})
      });
      if (!alive.current) return;
      if (res.up_to_date || !res.preview) {
        setNotice(t("skillMarket.upToDate", { name }));
      } else {
        setConfirm(NOTHING_CONFIRMED);
        setReview({ name, update, preview: res.preview });
      }
    } catch (e) {
      if (alive.current) setError(humanLine(e));
    } finally {
      if (alive.current) setBusy(null);
    }
  };

  const doInstall = async () => {
    if (!client || !review || busy || !canConfirm(review.preview, confirm)) return;
    setBusy(review.name);
    setError(null);
    try {
      await client.marketInstall(
        buildInstallBody(review.preview, {
          name: review.name,
          indexUrl: activeUrl,
          update: review.update,
          confirm
        })
      );
      if (!alive.current) return;
      toast.show(t(review.update ? "skillMarket.updated" : "skillMarket.installedToast", { name: review.name }), "success");
      setReview(null);
      setTab("installed");
      onChanged();
      void loadInstalled();
      if (index) void loadIndex(activeUrl);
    } catch (e) {
      if (!alive.current) return;
      const code = e instanceof ApiError ? e.code : "";
      // The content changed under the review: show it again instead of installing something unseen.
      if (code === "skill_changed") {
        setReview(null);
        setError(humanLine(e));
      } else {
        setError(humanLine(e));
      }
    } finally {
      if (alive.current) setBusy(null);
    }
  };

  const doRemove = async (name: string, force: boolean) => {
    if (!client) return;
    setBusy(name);
    setError(null);
    try {
      await client.marketUninstall(name, force);
      if (!alive.current) return;
      toast.show(t("skillMarket.removed", { name }), "success");
      setRemoving(null);
      onChanged();
      void loadInstalled();
      if (index) void loadIndex(activeUrl);
    } catch (e) {
      if (!alive.current) return;
      const used = e instanceof ApiError && e.status === 409 ? (e.body as { used_by?: Array<{ name: string }> } | undefined)?.used_by : undefined;
      if (used && used.length > 0) {
        // Bots use it: ask once more, naming them, before removing anyway.
        setRemoving({ name, usedBy: used.map((b) => b.name) });
      } else {
        setRemoving(null);
        setError(humanLine(e));
      }
    } finally {
      if (alive.current) setBusy(null);
    }
  };

  const shown = useMemo(() => filterByQuery(index?.skills ?? [], query), [index, query]);
  const urlCheck = checkIndexUrl(customUrl);

  if (review) {
    return (
      <ReviewPane
        review={review}
        confirm={confirm}
        onConfirm={setConfirm}
        busy={busy !== null}
        error={error}
        onBack={() => {
          setReview(null);
          setError(null);
        }}
        onInstall={() => void doInstall()}
      />
    );
  }

  return (
    <div className={dialogStyles.root} data-testid="skill-market-dialog">
      <DialogHeader title={t("skillMarket.title")} subtitle={t("skillMarket.subtitle")} onClose={onClose} />
      <div className={dialogStyles.body} inert={busy !== null && removing === null}>
        <Segmented<Tab>
          label={t("skillMarket.title")}
          value={tab}
          onChange={(v) => {
            setTab(v);
            setError(null);
            setNotice(null);
          }}
          options={[
            { value: "installed", label: t("skillMarket.tabInstalled"), testId: "skill-market-tab-installed" },
            { value: "browse", label: t("skillMarket.tabBrowse"), testId: "skill-market-tab-browse" }
          ]}
        />
        {error ? (
          <Callout tone="danger" role="alert">
            {error}
          </Callout>
        ) : null}
        {notice ? (
          <Callout tone="success" role="status">
            {notice}
          </Callout>
        ) : null}

        {tab === "installed" ? (
          installedError && !installed ? (
            <Callout tone="danger" title={t("skillMarket.loadFailed")} role="alert">
              {installedError}
            </Callout>
          ) : !installed ? (
            <p className={dialogStyles.hint}>{t("skillMarket.loading")}</p>
          ) : installed.length === 0 ? (
            <p className={skillStyles.empty} data-testid="skill-market-installed-empty">
              <PackageOpen aria-hidden="true" />
              {t("skillMarket.installedEmpty")}
            </p>
          ) : (
            <ul className={skillStyles.list} data-testid="skill-market-installed">
              {installed.map((s) => (
                <li key={s.name} className={styles.row}>
                  <InstalledRow
                    skill={s}
                    busy={busy === s.name}
                    onUpdate={() => void openReview(s.name, true)}
                    onRemove={() => setRemoving({ name: s.name, usedBy: [] })}
                  />
                </li>
              ))}
            </ul>
          )
        ) : (
          <>
            <label className={skillStyles.find}>
              <Search aria-hidden="true" />
              <input
                type="search"
                value={query}
                placeholder={t("skillMarket.search")}
                aria-label={t("skillMarket.search")}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            {indexLoading ? <p className={dialogStyles.hint}>{t("skillMarket.loading")}</p> : null}
            {indexError ? (
              <Callout tone="danger" title={t("skillMarket.listFailed")} role="alert">
                {indexError}
                <div>
                  <Button size="sm" variant="secondary" onClick={() => void loadIndex(activeUrl)}>
                    <RefreshCw aria-hidden="true" />
                    {t("health.retry")}
                  </Button>
                </div>
              </Callout>
            ) : null}
            {index && index.source === "custom" ? (
              <Callout tone="warning" role="status">
                {t("skillMarket.customActive", { url: activeUrl ?? "" })}
              </Callout>
            ) : null}
            {index && !indexError ? (
              index.skills.length === 0 ? (
                <p className={skillStyles.empty}>
                  <PackageOpen aria-hidden="true" />
                  {t("skillMarket.browseEmpty")}
                </p>
              ) : shown.length === 0 ? (
                <p className={skillStyles.empty}>
                  <Search aria-hidden="true" />
                  {t("skillMarket.searchEmpty")}
                </p>
              ) : (
                <ul className={skillStyles.list} data-testid="skill-market-browse">
                  {shown.map((s) => (
                    <li key={s.name} className={styles.row}>
                      <BrowseRow
                        skill={s}
                        busy={busy === s.name}
                        onReview={() => void openReview(s.name, false)}
                      />
                    </li>
                  ))}
                </ul>
              )
            ) : null}
            <details className={styles.advanced}>
              <summary>{t("skillMarket.advanced")}</summary>
              <p className={dialogStyles.hint}>{t("skillMarket.advancedHint")}</p>
              <div className={styles.urlRow}>
                <input
                  type="url"
                  value={customUrl}
                  className={styles.urlInput}
                  placeholder="https://example.com/skills/index.json"
                  aria-label={t("skillMarket.urlLabel")}
                  aria-invalid={urlCheck !== "ok" && urlCheck !== "empty"}
                  aria-describedby="skill-market-url-note"
                  onChange={(e) => setCustomUrl(e.target.value)}
                />
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={urlCheck !== "ok" || indexLoading}
                  onClick={() => void loadIndex(customUrl.trim())}
                >
                  {t("skillMarket.useList")}
                </Button>
                {activeUrl ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => {
                      setCustomUrl("");
                      void loadIndex(null);
                    }}
                  >
                    {t("skillMarket.useDefault")}
                  </Button>
                ) : null}
              </div>
              <p id="skill-market-url-note" className={dialogStyles.hint} role="status">
                {urlCheck === "not_https"
                  ? t("skillMarket.urlNotHttps")
                  : urlCheck === "bad_host"
                    ? t("skillMarket.urlBadHost")
                    : t("skillMarket.urlTrust")}
              </p>
            </details>
          </>
        )}
        <p className={dialogStyles.hint}>{t("skillMarket.boundary")}</p>
      </div>
      <DialogFooter>
        <Button variant="secondary" onClick={onClose}>
          {t("common.close")}
        </Button>
      </DialogFooter>
      <ConfirmDialog
        open={removing !== null}
        title={t(removing && removing.usedBy.length > 0 ? "skillMarket.removeUsedTitle" : "skillMarket.removeTitle", {
          name: removing?.name ?? ""
        })}
        body={
          removing && removing.usedBy.length > 0
            ? t("skillMarket.removeUsedBody", { bots: removing.usedBy.join(", ") })
            : t("skillMarket.removeBody")
        }
        confirmLabel={t("skillMarket.remove")}
        cancelLabel={t("common.cancel")}
        tone="danger"
        busy={busy !== null}
        onCancel={() => setRemoving(null)}
        onConfirm={() => removing && void doRemove(removing.name, removing.usedBy.length > 0)}
      />
    </div>
  );
}

function NeedsList({ needs }: { needs: string[] }) {
  const { t } = useTranslation();
  const list = sortedNeeds(needs);
  if (list.length === 0) {
    return <p className={styles.noNeeds}>{t("skillMarket.needs.none")}</p>;
  }
  return (
    <ul className={styles.needs}>
      {list.map((c) => (
        <li key={c}>{t(needsKey(c))}</li>
      ))}
    </ul>
  );
}

function RiskBadge({ risk }: { risk: string }) {
  const { t } = useTranslation();
  const r = riskOf(risk);
  return (
    <span className={`${styles.badge} ${styles[`tone_${riskTone(r)}`]}`}>
      {t(`skillMarket.risk.${r}`)}
    </span>
  );
}

function InstalledRow({
  skill,
  busy,
  onUpdate,
  onRemove
}: {
  skill: MarketInstalled;
  busy: boolean;
  onUpdate: () => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.rowBody}>
      <div className={skillStyles.itemTop}>
        <span className={skillStyles.itemName}>{skill.name}</span>
        <span className={skillStyles.scope}>{t("skillMarket.version", { version: skill.version })}</span>
        <RiskBadge risk={skill.risk} />
        {skill.state !== "ok" ? (
          <span className={`${styles.badge} ${styles.tone_warn}`}>{t(`skillMarket.state.${skill.state}`)}</span>
        ) : null}
      </div>
      {skill.description ? <span className={skillStyles.itemDesc}>{skill.description}</span> : null}
      <span className={skillStyles.itemDesc}>
        {t(sourceKey(skill.source_kind), { host: skill.source_host })}
      </span>
      <NeedsList needs={skill.needs} />
      <div className={styles.actions}>
        <Button size="sm" variant="secondary" disabled={busy} onClick={onUpdate}>
          <RefreshCw aria-hidden="true" />
          {t("skillMarket.checkUpdate")}
        </Button>
        <Button size="sm" variant="ghost" disabled={busy} onClick={onRemove} aria-label={t("skillMarket.removeAria", { name: skill.name })}>
          <Trash2 aria-hidden="true" />
          {t("skillMarket.remove")}
        </Button>
      </div>
    </div>
  );
}

function BrowseRow({ skill, busy, onReview }: { skill: MarketSkill; busy: boolean; onReview: () => void }) {
  const { t } = useTranslation();
  const unavailable = skill.installed || skill.already_available;
  return (
    <div className={styles.rowBody}>
      <div className={skillStyles.itemTop}>
        <span className={skillStyles.itemName}>{skill.name}</span>
        {skill.tags.slice(0, 3).map((tag) => (
          <span key={tag} className={skillStyles.scope}>
            {tag}
          </span>
        ))}
      </div>
      {skill.description ? <span className={skillStyles.itemDesc}>{skill.description}</span> : null}
      {skill.author || skill.license ? (
        <span className={skillStyles.itemDesc}>
          {[skill.author, skill.license].filter(Boolean).join(" · ")}
        </span>
      ) : null}
      <div className={styles.actions}>
        {unavailable ? (
          <span className={skillStyles.itemDesc}>
            {t(skill.installed ? "skillMarket.alreadyInstalled" : "skillMarket.alreadyIncluded")}
          </span>
        ) : (
          <Button size="sm" variant="secondary" disabled={busy} onClick={onReview} aria-label={t("skillMarket.reviewAria", { name: skill.name })}>
            <Download aria-hidden="true" />
            {t("skillMarket.review")}
          </Button>
        )}
      </div>
    </div>
  );
}

function ReviewPane({
  review,
  confirm,
  onConfirm,
  busy,
  error,
  onBack,
  onInstall
}: {
  review: Review;
  confirm: ConfirmState;
  onConfirm: (c: ConfirmState) => void;
  busy: boolean;
  error: string | null;
  onBack: () => void;
  onInstall: () => void;
}) {
  const { t } = useTranslation();
  const p = review.preview;
  const risk = riskOf(p.risk);
  const ready = canConfirm(p, confirm);
  const flagged = p.findings.filter((f) => f.severity !== "info");
  return (
    <div className={dialogStyles.root} data-testid="skill-market-review">
      <DialogHeader
        title={t(review.update ? "skillMarket.reviewUpdateTitle" : "skillMarket.reviewTitle", { name: p.name })}
        subtitle={t("skillMarket.reviewSubtitle")}
        closeDisabled={busy}
        onClose={onBack}
      />
      <div className={dialogStyles.body} inert={busy}>
        <p className={skillStyles.itemDesc}>{p.description}</p>
        <div className={skillStyles.itemTop}>
          <span className={skillStyles.scope}>{t("skillMarket.version", { version: p.version })}</span>
          <RiskBadge risk={p.risk} />
          <span className={skillStyles.itemDesc}>{t("skillMarket.files", { count: p.files })}</span>
        </div>

        {review.update && p.installed ? (
          <section className={dialogStyles.block} data-testid="skill-market-diff">
            <h3 className={dialogStyles.blockTitle}>{t("skillMarket.diffTitle")}</h3>
            <p className={skillStyles.itemDesc}>
              {t("skillMarket.diffVersion", { from: p.installed.version, to: p.version })}
            </p>
            {p.added_needs.length > 0 ? (
              <Callout tone="warning" title={t("skillMarket.diffAdded")} role="status">
                <NeedsList needs={p.added_needs} />
              </Callout>
            ) : (
              <p className={skillStyles.itemDesc}>{t("skillMarket.diffSame")}</p>
            )}
            {p.removed_needs.length > 0 ? (
              <>
                <p className={skillStyles.itemDesc}>{t("skillMarket.diffRemoved")}</p>
                <NeedsList needs={p.removed_needs} />
              </>
            ) : null}
            {riskOf(p.installed.risk) !== risk ? (
              <p className={skillStyles.itemDesc}>
                {t("skillMarket.diffRisk", {
                  from: t(`skillMarket.risk.${riskOf(p.installed.risk)}`),
                  to: t(`skillMarket.risk.${risk}`)
                })}
              </p>
            ) : null}
          </section>
        ) : null}

        <section className={dialogStyles.block}>
          <h3 className={dialogStyles.blockTitle}>{t("skillMarket.needsTitle")}</h3>
          <p className={skillStyles.itemDesc}>{t("skillMarket.needsLead")}</p>
          <NeedsList needs={p.needs} />
        </section>

        <section className={dialogStyles.block}>
          <h3 className={dialogStyles.blockTitle}>{t("skillMarket.scanTitle")}</h3>
          {flagged.length === 0 ? (
            <p className={skillStyles.itemDesc}>{t("skillMarket.scanClean")}</p>
          ) : (
            <ul className={styles.findings}>
              {flagged.map((f, i) => (
                <li key={`${f.file}:${f.line ?? 0}:${i}`}>
                  <span className={`${styles.badge} ${f.severity === "high" ? styles.tone_danger : styles.tone_warn}`}>
                    {t(`skillMarket.severity.${f.severity === "high" ? "high" : "warn"}`)}
                  </span>{" "}
                  {f.message}{" "}
                  <code>
                    {f.file}
                    {f.line ? `:${f.line}` : ""}
                  </code>
                </li>
              ))}
            </ul>
          )}
          <p className={dialogStyles.hint}>{t("skillMarket.scanAdvisory")}</p>
        </section>

        {p.requires_risk_acceptance ? (
          <Callout tone={risk === "high" ? "danger" : "warning"} title={t("skillMarket.riskTitle", { risk: t(`skillMarket.risk.${risk}`) })}>
            <label className={styles.tick}>
              <input
                type="checkbox"
                checked={confirm.acceptRisk}
                onChange={(e) => onConfirm({ ...confirm, acceptRisk: e.target.checked })}
                data-testid="skill-market-accept-risk"
              />
              <span>{t("skillMarket.acceptRisk")}</span>
            </label>
          </Callout>
        ) : null}
        {p.requires_access_confirmation ? (
          <Callout tone="warning" title={t("skillMarket.moreTitle")}>
            <label className={styles.tick}>
              <input
                type="checkbox"
                checked={confirm.acceptMore}
                onChange={(e) => onConfirm({ ...confirm, acceptMore: e.target.checked })}
                data-testid="skill-market-accept-more"
              />
              <span>{t("skillMarket.acceptMore")}</span>
            </label>
          </Callout>
        ) : null}
        {error ? (
          <Callout tone="danger" role="alert">
            {error}
          </Callout>
        ) : null}
      </div>
      <DialogFooter note={ready ? undefined : t("skillMarket.tickFirst")}>
        <Button variant="ghost" disabled={busy} onClick={onBack}>
          {t("skillMarket.back")}
        </Button>
        <Button variant="primary" disabled={busy || !ready} data-testid="skill-market-install" onClick={onInstall}>
          {t(review.update ? "skillMarket.updateNow" : "skillMarket.installNow", { name: p.name })}
        </Button>
      </DialogFooter>
    </div>
  );
}
