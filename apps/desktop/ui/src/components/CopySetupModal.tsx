import { writeLastSync } from "../lib/lastSync";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { CopyPlan, CopyReport } from "../api/types";
import {
  buildCopyItems,
  defaultChecked,
  resultKey,
  summarizeReport,
  toRequest,
  unanswered,
  type LinkChoice
} from "../lib/copySetup";
import { MOCK_BASE_URL } from "../api/mockMode";
import { setHostComputerAccess } from "./ComputerAccess";
import { useDaemon } from "../state/DaemonContext";
import styles from "./CopySetupModal.module.css";
import { Modal } from "./Modal";
import { RunOnServer } from "./RunOnServer";

/**
 * "Make this server the home of my bots": one confirmation that lists what
 * moves, then a per-item report. The local daemon reads the keys and sends
 * them; this dialog only ever holds names. Nothing is deleted on this computer
 * and the dialog can be run again later (it skips what the server already has).
 */
export function CopySetupModal({
  open,
  hostId,
  onClose
}: {
  open: boolean;
  hostId: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="sm" label={t("copySetup.title")}>
      {open ? <Content hostId={hostId} onClose={onClose} /> : null}
    </Modal>
  );
}

function Content({ hostId, onClose }: { hostId: string; onClose: () => void }) {
  const { t } = useTranslation();
  const { localApi: local, client, refreshHome, hostList } = useDaemon();
  // Mock mode (design review) has no separate local daemon: the mock client answers.
  const localApi = local ?? (client?.baseUrl === MOCK_BASE_URL ? client : null);
  const [plan, setPlan] = useState<CopyPlan | null>(null);
  const [failed, setFailed] = useState(false);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  // Answers to "a bot with this name already exists on the server" (never defaulted).
  const [choices, setChoices] = useState<Record<string, LinkChoice>>({});
  // Optional last step, off by default: let this server's bots use THIS computer too.
  const [allowPc, setAllowPc] = useState(false);
  const [report, setReport] = useState<CopyReport | null>(null);
  const items = useMemo(() => (plan ? buildCopyItems(plan) : []), [plan]);

  useEffect(() => {
    let live = true;
    localApi
      ?.copyPlan(hostId)
      .then((p) => {
        if (live) {
          setPlan(p);
          setChecked(defaultChecked(buildCopyItems(p)));
        }
      })
      .catch(() => live && setFailed(true));
    return () => {
      live = false;
    };
  }, [localApi, hostId]);

  const send = async () => {
    if (!localApi || busy) {
      return;
    }
    setBusy(true);
    try {
      const ownerKey = client?.baseUrl === MOCK_BASE_URL ? "mock" : await invoke<string>("owner_key");
      setReport(await localApi.copySetup(hostId, toRequest(items, checked, choices), ownerKey));
      writeLastSync(hostId, Date.now());
      // The home is recorded on both sides: the app follows it right away (no picking).
      void refreshHome();
      const link = hostList?.hosts.find((h) => h.id === hostId);
      if (allowPc && link) {
        await setHostComputerAccess(localApi, link, true).catch(() => {});
        void refreshHome();
      }
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };

  const refused = plan?.host_accepts === false && plan.host_code;
  const summary = report ? summarizeReport(report) : null;
  const hostName = hostList?.hosts.find((h) => h.id === hostId)?.name ?? t("computerPicker.host");
  const pending = unanswered(items, checked, choices);
  const adopted = Boolean(summary && !summary.homeFailed && summary.done > 0);
  return (
    <div className={styles.root} data-testid="copy-setup">
      <header>
        <h2 className={styles.title}>{t("copySetup.title")}</h2>
        <p className={styles.lead}>{t("copySetup.lead")}</p>
      </header>
      {failed ? <p role="alert">{t("copySetup.failed")}</p> : null}
      {refused ? (
        <div role="alert" className={styles.notice}>
          <p>{t(`copySetup.host.${plan.host_code}`)}</p>
          {plan.host_command ? <code className={styles.code}>{plan.host_command}</code> : null}
          {plan.host_command ? <RunOnServer command={plan.host_command} beforeOpen={onClose} /> : null}
        </div>
      ) : null}
      {report ? (
        <>
          {adopted ? (
            <div className={styles.notice} data-testid="home-adopted" role="status">
              <b>{t("home.adopted.title", { name: hostName })}</b>
              <p>{t("home.adopted.moved", { name: hostName })}</p>
            </div>
          ) : null}
          <ul className={styles.list} data-testid="copy-report">
            {report.items
              .filter((i) => i.kind !== "home")
              .map((i) => (
                <li key={`${i.kind}:${i.id}`} data-status={i.status}>
                  <b>{i.kind === "schedules" || i.kind === "chats" ? t(`copySetup.kind.${i.kind}Name`) : i.name}</b>
                  <span>
                    {i.count !== undefined && i.count !== null
                      ? t(`copySetup.counts.${i.kind}`, {
                          count: i.count,
                          existing: i.existing ?? 0,
                          skipped: i.skipped ?? 0
                        })
                      : t(`copySetup.result.${resultKey(i)}`, { defaultValue: i.code ?? i.status })}
                    {i.count !== undefined && i.code
                      ? ` ${t(`copySetup.result.${i.code}`, { defaultValue: i.code })}`
                      : ""}
                  </span>
                </li>
              ))}
          </ul>
          <p>
            {t("copySetup.summary", {
              done: summary?.done,
              skipped: summary?.skipped,
              failed: summary?.failed
            })}
          </p>
          {summary?.homeFailed ? <p role="alert">{t("copySetup.homeFailed")}</p> : null}
          {adopted ? (
            <>
              <p className={styles.note}><b>{t("home.adopted.stayed")}.</b> {t("home.adopted.stayedBody")}</p>
            </>
          ) : null}
        </>
      ) : (
        <ul className={styles.list}>
          {items.map((i) => (
            <li key={i.key}>
              <label>
                <input
                  type="checkbox"
                  checked={checked.has(i.key)}
                  disabled={Boolean(i.blocked) || Boolean(refused)}
                  onChange={(e) => {
                    const next = new Set(checked);
                    if (e.target.checked) {
                      next.add(i.key);
                    } else {
                      next.delete(i.key);
                    }
                    setChecked(next);
                  }}
                />
                <b>{i.detail === "extra" ? t(`copySetup.kind.${i.kind}Name`) : i.label}</b>
                <span>
                  {i.hostState === "separate"
                    ? t("copySetup.keptSeparate", { server: hostName })
                    : i.blocked
                    ? t(`copySetup.blocked.${i.blocked}`, { defaultValue: i.blocked })
                    : i.detail === "extra"
                      ? t(`copySetup.kind.${i.kind}`, { count: i.count ?? 0, messages: i.messages ?? 0 })
                      : t(`copySetup.kind.${i.kind}${i.detail === "key" ? "Key" : ""}`)}
                </span>
              </label>
              {i.kind === "bot" && i.hostState === "name_taken" && checked.has(i.key) ? (
                <div className={styles.question} role="group" data-testid={`adopt-question-${i.id}`}>
                  <p>{t("copySetup.nameTaken", { name: i.label, server: hostName })}</p>
                  {(["adopt", "separate"] as const).map((c) => (
                    <label key={c}>
                      <input
                        type="radio"
                        name={`link-${i.id}`}
                        checked={choices[i.id] === c}
                        data-testid={`${c}-${i.id}`}
                        onChange={() => setChoices({ ...choices, [i.id]: c })}
                      />
                      <span>
                        <b>{t(`copySetup.choice.${c}`)}</b>
                        <br />
                        {t(`copySetup.choice.${c}Why`, { server: hostName })}
                      </span>
                    </label>
                  ))}
                </div>
              ) : null}
            </li>
          ))}
        </ul>
      )}
      {report || refused ? null : (
        <label className={styles.note} data-testid="copy-setup-allow-pc">
          <input type="checkbox" checked={allowPc} onChange={(e) => setAllowPc(e.target.checked)} />{" "}
          <b>{t("computerAccess.title")}.</b> {t("home.adopted.allowPc")}
        </label>
      )}
      {!report && pending.length > 0 ? (
        <p className={styles.note} role="status">{t("copySetup.answerFirst")}</p>
      ) : null}
      {/* A disabled button says why: nothing ticked, or nothing there to tick. */}
      {!report && !refused && !busy && pending.length === 0 && checked.size === 0 && plan ? (
        <p className={styles.note} role="status" data-testid="copy-setup-why">
          {t(items.length === 0 ? "copySetup.nothingToMove" : "copySetup.pickFirst")}
        </p>
      ) : null}
      <p className={styles.note}>{t("copySetup.cannotMove")}</p>
      <p className={styles.note}>{t("copySetup.keepLocal")}</p>
      <div className={styles.actions}>
        <Button variant="ghost" onClick={onClose}>
          {report ? t("common.close") : t("common.cancel")}
        </Button>
        {report ? null : (
          <Button
            variant="primary"
            disabled={busy || checked.size === 0 || Boolean(refused) || pending.length > 0}
            onClick={() => void send()}
            data-testid="copy-setup-send"
          >
            {busy ? t("copySetup.sending") : t("copySetup.send")}
          </Button>
        )}
      </div>
    </div>
  );
}
