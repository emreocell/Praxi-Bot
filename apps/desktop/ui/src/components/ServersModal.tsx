import { Plus, RefreshCw, Server, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { HostLinkInfo } from "../api/types";
import { isMockMode } from "../api/mockMode";
import { humanLine } from "../lib/humanError";
import { updateState, type UpdateState } from "../lib/serverUpdate";
import { type ServerVersion, useServerVersions } from "../state/useServerVersions";
import { useDaemon } from "../state/DaemonContext";
import styles from "./AddServerModal.module.css";
import dialog from "./Dialog.module.css";
import { Callout, DialogFooter, DialogHeader } from "./DialogParts";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import { UpdateServerModal } from "./UpdateServerModal";
import updateStyles from "./UpdateServerModal.module.css";

/**
 * "Your servers": every paired server with its state, and Remove with a plain
 * explanation of what is (and is not) deleted. Remove only forgets the pairing on
 * this computer; nothing on the server is touched.
 */
export function ServersModal({ open, onClose, onAdd }: { open: boolean; onClose: () => void; onAdd: () => void }) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="md" label={t("servers.title")}>
      {open ? <Content onClose={onClose} onAdd={onAdd} /> : null}
    </Modal>
  );
}

function Content({ onClose, onAdd }: { onClose: () => void; onAdd: () => void }) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, localApi, hostList, refreshHosts, selectComputer } = useDaemon();
  const api = isMockMode() ? client : localApi;
  const [asking, setAsking] = useState<HostLinkInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const hosts = hostList?.hosts ?? [];
  const [updating, setUpdating] = useState<string | null>(null);
  const { versions, recheck } = useServerVersions(api, hosts.map((h) => h.id), true);
  const app = { version: hostList?.local_version ?? "", releaseTag: hostList?.local_release_tag ?? null };

  const remove = async (h: HostLinkInfo) => {
    if (!api) return;
    setBusy(true);
    try {
      await api.forgetHost(h.id);
      toast.show(t("servers.removed", { name: h.name }), "success");
      setAsking(null);
      await refreshHosts();
    } catch (e) {
      toast.show(`${t("servers.removeFailed")} ${humanLine(e)}`, "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={dialog.root} data-testid="servers">
      <DialogHeader
        title={t("servers.title")}
        subtitle={t("servers.lead")}
        leading={<Server aria-hidden="true" className={styles.headIcon} />}
        onClose={onClose}
      />
      <div className={dialog.body}>
        {hosts.length === 0 ? <p className={styles.para}>{t("servers.empty")}</p> : null}
        <ul className={styles.guide}>
          {hosts.map((h) => (
            <li key={h.id} className={styles.guideItem} data-testid={`server-${h.id}`}>
              <div className={styles.serverRow}>
                <span className={styles.stageText}>
                  <b>{h.name}</b>
                  <small data-state={h.state}>{t(`servers.status.${h.state}`)}</small>
                  <VersionLine v={versions[h.id]} app={app} />
                </span>
                {stateOf(versions[h.id], app) === "older" || stateOf(versions[h.id], app) === "unknown" ? (
                  <Button
                    variant={stateOf(versions[h.id], app) === "older" ? "primary" : "ghost"}
                    onClick={() => setUpdating(h.id)}
                    data-testid={`update-${h.id}`}
                  >
                    <RefreshCw aria-hidden="true" />
                    {t(stateOf(versions[h.id], app) === "older" ? "updateServer.button" : "updateServer.buttonAnyway")}
                  </Button>
                ) : null}
                <Button
                  variant="ghost"
                  onClick={() => {
                    void selectComputer(h.id);
                    onClose();
                  }}
                >
                  {t("servers.checkNow")}
                </Button>
                <Button variant="ghost" onClick={() => setAsking(h)} data-testid={`remove-${h.id}`}>
                  <Trash2 aria-hidden="true" />
                  {t("servers.remove")}
                </Button>
              </div>
              {asking?.id === h.id ? (
                <Callout tone="warning" title={t("servers.removeTitle", { name: h.name })} role="alert">
                  <p className={styles.para}>{t("servers.removeBody")}</p>
                  <div className={styles.keyActions}>
                    <Button variant="ghost" onClick={() => setAsking(null)}>
                      {t("common.cancel")}
                    </Button>
                    <Button variant="primary" disabled={busy} onClick={() => void remove(h)} data-testid="remove-confirm">
                      {t("servers.removeConfirm")}
                    </Button>
                  </div>
                </Callout>
              ) : null}
            </li>
          ))}
        </ul>
        {hosts.length > 0 ? (
          <p className={styles.note}>
            {t("servers.updateHelp")}{" "}
            <Button variant="ghost" onClick={recheck} data-testid="servers-recheck">
              {t("updateServer.checkAgain")}
            </Button>
          </p>
        ) : null}
      </div>
      <DialogFooter>
        <Button variant="ghost" onClick={onClose}>
          {t("common.close")}
        </Button>
        <Button variant="primary" onClick={onAdd} data-testid="servers-add">
          <Plus aria-hidden="true" />
          {t("servers.add")}
        </Button>
      </DialogFooter>
      <UpdateServerModal
        open={updating !== null}
        hostId={updating ?? ""}
        onClose={() => {
          setUpdating(null);
          recheck();
        }}
      />
    </div>
  );
}

type AppBuild = { version: string; releaseTag: string | null };

/** `null` while the server's version is not known (loading, or it could not be asked). */
export function stateOf(v: ServerVersion | undefined, app: AppBuild): UpdateState | null {
  return v?.state === "known" ? updateState({ version: v.version, releaseTag: v.releaseTag }, app) : null;
}

/** "Version 0.0.9 (this app: 0.1.0)" and a plain badge; nothing alarming when it cannot ask. */
export function VersionLine({ v, app }: { v: ServerVersion | undefined; app: AppBuild }) {
  const { t } = useTranslation();
  if (!v || v.state === "loading") return <small>{t("updateServer.checking")}</small>;
  if (v.state === "unknown") return <small>{t("updateServer.unknown")}</small>;
  const state = stateOf(v, app);
  const shown = (x: string, tag: string | null) => (tag ?? x);
  return (
    <small className={updateStyles.versionLine}>
      {t("updateServer.versionLine", { server: shown(v.version, v.releaseTag), app: shown(app.version, app.releaseTag) })}
      {state === "older" ? (
        <span className={updateStyles.badge}>{t("updateServer.available")}</span>
      ) : state === "unknown" ? (
        <span>{t("updateServer.versionUnknown")}</span>
      ) : state === "newer" ? (
        <span>{t("updateServer.newer")}</span>
      ) : (
        <span>{t("updateServer.upToDate")}</span>
      )}
    </small>
  );
}
