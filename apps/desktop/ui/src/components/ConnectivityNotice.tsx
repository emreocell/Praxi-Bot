import { WifiOff } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useDaemon } from "../state/DaemonContext";
import { useConnectivity } from "../state/useConnectivity";
import styles from "./ConnectivityNotice.module.css";

export function ConnectivityNotice() {
  const { t } = useTranslation();
  const { state } = useConnectivity();
  const { activeHostName } = useDaemon();
  if (state !== "offline" && state !== "host_unreachable") return null;
  // Name the computer that is silent: "your selected computer" says neither which one nor what to do.
  const name = activeHostName || t("computerPicker.host");
  return <div className={styles.notice} role="status" data-testid="connectivity-notice">
    <WifiOff aria-hidden="true" />
    <span>{state === "offline" ? `${t("offline.title")} ${t("offline.detail")}` : `${t("offline.host", { name })} ${t("offline.hostDetail")}`}</span>
  </div>;
}
