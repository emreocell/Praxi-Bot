import { ChevronLeft, ChevronRight, ClipboardList, Download, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { AuditEvent, AuditPage, AuditVerify } from "../../api/types";
import { Modal } from "../../components/Modal";
import { useToast } from "../../components/Toast";
import {
  AUDIT_KINDS,
  auditActorKey,
  auditEventKey,
  auditQuery,
  type AuditFilters
} from "../../lib/auditLog";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./AuditLogCard.module.css";

const PAGE = 50;
const DAY = 86_400_000;
const WINDOWS: Array<[string, number | null]> = [
  ["anyTime", null],
  ["last24h", DAY],
  ["last7d", 7 * DAY],
  ["last30d", 30 * DAY]
];
const NO_FILTERS: AuditFilters = { windowMs: null, actor: "", kind: "", bot: "", q: "" };

/**
 * Settings > Safety: the owner's read-only activity log (approvals, permission changes, takeovers,
 * imports, updates, key use) with filters, search, a details drawer, redacted export and an
 * integrity check. The daemon redacts everything; this view only displays and saves what it got.
 */
export function AuditLogCard() {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [filters, setFilters] = useState<AuditFilters>(NO_FILTERS);
  const [search, setSearch] = useState("");
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<AuditPage | null>(null);
  const [bots, setBots] = useState<Array<[string, string]>>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState<AuditEvent | null>(null);
  const [verify, setVerify] = useState<AuditVerify | "failed" | null>(null);
  const generation = useRef(0);

  const load = useCallback(async () => {
    if (!client) return;
    const mine = ++generation.current;
    setBusy(true);
    setError(null);
    try {
      const next = await client.auditLog(auditQuery(filters, Date.now(), { offset, limit: PAGE }));
      if (mine !== generation.current) return;
      setPage(next);
      // Remember every bot ever seen so the filter list does not shrink while filtering.
      setBots((old) => {
        const map = new Map(old);
        for (const e of next.items) if (e.bot_id) map.set(e.bot_id, e.bot_name ?? e.bot_id);
        return [...map];
      });
    } catch (e) {
      if (mine === generation.current) setError(`${t("audit.failed")} (${humanLine(e)})`);
    } finally {
      if (mine === generation.current) setBusy(false);
    }
  }, [client, filters, offset, t]);

  useEffect(() => {
    if (open) void load();
  }, [open, load]);

  const close = () => {
    generation.current++;
    setOpen(false);
    setSelected(null);
    setPage(null);
    setVerify(null);
    setFilters(NO_FILTERS);
    setSearch("");
    setOffset(0);
  };
  const change = (patch: Partial<AuditFilters>) => {
    setFilters((f) => ({ ...f, ...patch }));
    setOffset(0);
  };

  const exportAs = async (format: "csv" | "json") => {
    if (!client) return;
    try {
      const text = await client.auditExport(auditQuery(filters, Date.now()), format);
      const url = URL.createObjectURL(
        new Blob([text], { type: format === "csv" ? "text/csv;charset=utf-8" : "application/json" })
      );
      const a = document.createElement("a");
      a.href = url;
      a.download = `praxi-audit.${format}`;
      a.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      toast.show(t("audit.exportDone"), "success");
    } catch (e) {
      toast.show(`${t("audit.exportFailed")} (${humanLine(e)})`, "danger");
    }
  };

  const runVerify = async () => {
    if (!client) return;
    setVerify(null);
    try {
      setVerify(await client.auditVerify());
    } catch {
      setVerify("failed");
    }
  };

  const copy = async (e: AuditEvent) => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(e.detail, null, 2));
      toast.show(t("audit.copied"), "success");
    } catch {
      toast.show(t("audit.copyFailed"), "danger");
    }
  };

  const actorName = (actor: string) => {
    const key = auditActorKey(actor);
    return key ? t(key) : actor;
  };
  const total = page?.total ?? 0;
  const first = total === 0 ? 0 : offset + 1;
  const last = Math.min(offset + PAGE, total);

  return (
    <>
      <Button variant="secondary" size="sm" disabled={!client} onClick={() => setOpen(true)}>
        <ClipboardList aria-hidden="true" />
        {t("audit.open")}
      </Button>
      <Modal open={open} onClose={close} label={t("audit.title")} size="lg">
        <div className={styles.root}>
          <h2>{t("audit.title")}</h2>
          <p>{t("audit.privacy")}</p>
          <form
            className={styles.controls}
            onSubmit={(e) => {
              e.preventDefault();
              change({ q: search });
            }}
          >
            <label>
              {t("audit.search")}
              <input type="search" value={search} onChange={(e) => setSearch(e.target.value)} />
            </label>
            <label>
              {t("audit.filterTime")}
              <select
                value={filters.windowMs ?? ""}
                onChange={(e) => change({ windowMs: e.target.value ? Number(e.target.value) : null })}
              >
                {WINDOWS.map(([key, ms]) => (
                  <option key={key} value={ms ?? ""}>{t(`audit.${key}`)}</option>
                ))}
              </select>
            </label>
            <label>
              {t("audit.filterKind")}
              <select value={filters.kind} onChange={(e) => change({ kind: e.target.value })}>
                <option value="">{t("audit.all")}</option>
                {AUDIT_KINDS.map((k) => (
                  <option key={k} value={k}>{t(`audit.kind.${k}`)}</option>
                ))}
              </select>
            </label>
            <label>
              {t("audit.filterActor")}
              <select value={filters.actor} onChange={(e) => change({ actor: e.target.value })}>
                <option value="">{t("audit.all")}</option>
                {(page?.actors ?? []).map((a) => (
                  <option key={a} value={a}>{actorName(a)}</option>
                ))}
              </select>
            </label>
            <label>
              {t("audit.filterBot")}
              <select value={filters.bot} onChange={(e) => change({ bot: e.target.value })}>
                <option value="">{t("audit.all")}</option>
                {bots.map(([id, name]) => (
                  <option key={id} value={id}>{name}</option>
                ))}
              </select>
            </label>
            <Button type="submit" variant="secondary" size="sm">{t("audit.search")}</Button>
          </form>
          {busy ? <p role="status">{t("audit.loading")}</p> : null}
          {error ? (
            <p role="alert">
              {error} <Button variant="ghost" size="sm" onClick={() => void load()}>{t("audit.retry")}</Button>
            </p>
          ) : null}
          {page && page.items.length === 0 && !busy ? <p>{t("audit.empty")}</p> : null}
          {page && page.items.length > 0 ? (
            <div className={styles.tableWrap}>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th scope="col">{t("audit.when")}</th>
                    <th scope="col">{t("audit.what")}</th>
                    <th scope="col">{t("audit.who")}</th>
                    <th scope="col">{t("audit.bot")}</th>
                    <th scope="col">{t("audit.type")}</th>
                  </tr>
                </thead>
                <tbody>
                  {page.items.map((e) => (
                    <tr key={e.id} className={selected?.id === e.id ? styles.selected : undefined}>
                      <td>{new Date(e.ts_ms).toLocaleString()}</td>
                      <td>
                        <button type="button" className={styles.link} onClick={() => setSelected(e)}>
                          {t(auditEventKey(e.action))}
                        </button>
                      </td>
                      <td>{actorName(e.actor)}</td>
                      <td>{e.bot_name ?? ""}</td>
                      <td>{t(`audit.kind.${e.kind}`)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : null}
          {page ? (
            <div className={styles.actions}>
              <Button variant="ghost" size="sm" disabled={offset === 0 || busy} onClick={() => setOffset(Math.max(0, offset - PAGE))}>
                <ChevronLeft aria-hidden="true" />{t("audit.prev")}
              </Button>
              <span role="status">{t("audit.range", { from: first, to: last, total })}</span>
              <Button variant="ghost" size="sm" disabled={last >= total || busy} onClick={() => setOffset(offset + PAGE)}>
                {t("audit.next")}<ChevronRight aria-hidden="true" />
              </Button>
            </div>
          ) : null}
          {selected ? (
            <section className={styles.drawer} aria-label={t("audit.details")}>
              <h3>{t(auditEventKey(selected.action))}</h3>
              <p>{t("audit.detailsNote")}</p>
              <p>{t("audit.action")}: <code>{selected.action}</code></p>
              <pre className={styles.preview} tabIndex={0}>{JSON.stringify(selected.detail, null, 2)}</pre>
              <div className={styles.actions}>
                <Button variant="secondary" size="sm" onClick={() => void copy(selected)}>{t("audit.copy")}</Button>
                <Button variant="ghost" size="sm" onClick={() => setSelected(null)}>{t("audit.close")}</Button>
              </div>
            </section>
          ) : null}
          <div className={styles.actions}>
            <Button variant="secondary" size="sm" disabled={!client} onClick={() => void exportAs("csv")}>
              <Download aria-hidden="true" />{t("audit.exportCsv")}
            </Button>
            <Button variant="secondary" size="sm" disabled={!client} onClick={() => void exportAs("json")}>
              <Download aria-hidden="true" />{t("audit.exportJson")}
            </Button>
            <Button variant="secondary" size="sm" disabled={!client} onClick={() => void runVerify()}>
              <ShieldCheck aria-hidden="true" />{t("audit.verify")}
            </Button>
            <Button variant="ghost" size="sm" onClick={close}>{t("audit.close")}</Button>
          </div>
          {verify === "failed" ? <p role="alert">{t("audit.verifyFailed")}</p> : null}
          {verify && verify !== "failed" ? (
            <p role="status">
              {verify.status === "ok"
                ? t("audit.verifyOk", { count: verify.entries })
                : t("audit.verifyProblem", { missing: verify.missing })}{" "}
              {t("audit.verifyNote")}
            </p>
          ) : null}
        </div>
      </Modal>
    </>
  );
}
