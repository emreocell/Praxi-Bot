import type { InstallStateDto, LocalInstallDto } from "../api/types";

type Model = LocalInstallDto["models"][number];

/** A download, unpack or start is running (poll while true). */
export function isBusy(state: InstallStateDto): boolean {
  return (
    state.phase === "downloading_runtime" ||
    state.phase === "unpacking" ||
    state.phase === "downloading_model" ||
    state.phase === "starting"
  );
}

/** Whole percent of the combined download, 0..100. */
export function percent(state: InstallStateDto): number {
  if (!state.bytes_total) {
    return 0;
  }
  return Math.max(0, Math.min(100, Math.floor((state.bytes_done / state.bytes_total) * 100)));
}

function errorKey(state: InstallStateDto): string {
  switch (state.error) {
    case "unsupported":
      return "errorUnsupported";
    case "disk":
      return "errorDisk";
    case "network":
      return "errorNetwork";
    case "checksum":
      return "errorChecksum";
    case "port_busy":
      return "errorPortBusy";
    case "start_failed":
      return "errorStartFailed";
    default:
      return "errorOther";
  }
}

/** i18n key (under `localInstall.`) that describes the state, or null when nothing is going on. */
export function phaseKey(state: InstallStateDto): string | null {
  switch (state.phase) {
    case "downloading_runtime":
      return "phaseDownloadingRuntime";
    case "unpacking":
      return "phaseUnpacking";
    case "downloading_model":
      return "phaseDownloadingModel";
    case "starting":
      return "phaseStarting";
    case "ready":
      return "phaseReady";
    case "cancelled":
      return "phaseCancelled";
    case "failed":
      return errorKey(state);
    default:
      return null;
  }
}

/** Only models that fit this computer and its disk can be installed. */
export function canInstall(m: Model): boolean {
  return (m.fit === "comfortable" || m.fit === "slow") && m.disk_ok !== false;
}

/** "2.3 GB" / "850 MB" from bytes. */
export function sizeLabel(bytes: number): string {
  const gb = bytes / 1024 ** 3;
  return gb >= 1 ? `${Math.round(gb * 10) / 10} GB` : `${Math.round(bytes / 1024 ** 2)} MB`;
}
