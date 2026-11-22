import { Plus, Server, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { HostLinkInfo } from "../api/types";
import { isMockMode } from "../api/mockMode";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import styles from "./AddServerModal.module.css";
import dialog from "./Dialog.module.css";
import { Callout, DialogFooter, DialogHeader } from "./DialogParts";
import { Modal } from "./Modal";
import { useToast } from "./Toast";

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
                </span>
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
        {hosts.length > 0 ? <p className={styles.note}>{t("servers.updateHelp")}</p> : null}
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
    </div>
  );
}
