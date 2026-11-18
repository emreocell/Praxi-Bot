import { useCallback, useEffect, useRef, useState } from "react";
import { createScopedLoader } from "../lib/scopedLoader";
import type { CliEngineDto } from "../api/types";
import { engineRows, type EngineRow } from "../lib/modelPicker";
import { type EngineBlock, engineBlock } from "../lib/engineConsent";
import { configuredCliIds } from "../lib/detectedAccounts";
import { useDaemon } from "./DaemonContext";
import { notifyProvidersChanged, useProviders } from "./useProviders";

/** Window event the model picker raises to open Settings > AI (where installing a CLI lives). */
export const OPEN_AI_SETTINGS_EVENT = "praxi:open-ai-settings";

/**
 * Coding CLIs the model picker offers besides the enabled ones (`GET /v1/providers/cli`):
 * "Connect" rows switch an installed CLI on, "Install" rows open the existing install flow.
 */
export function useCliEngineRows(enabled: boolean): {
  rows: EngineRow[];
  run: (row: EngineRow) => Promise<void>;
  /** Engines (by registry id) whose models cannot answer right now, and why. */
  blocks: Record<string, EngineBlock>;
} {
  const { client } = useDaemon();
  const { dto } = useProviders();
  const [engines, setEngines] = useState<CliEngineDto[]>([]);

  const loader = useRef(createScopedLoader<CliEngineDto[]>((v) => setEngines(v ?? [])));
  useEffect(() => {
    if (!enabled || !client) {
      loader.current.switchTo(null);
      return;
    }
    void loader.current.load(client, () => client.cliEngines());
  }, [client, enabled, dto]);

  const run = useCallback(
    async (row: EngineRow) => {
      if (row.action === "install") {
        window.dispatchEvent(new CustomEvent(OPEN_AI_SETTINGS_EVENT));
        return;
      }
      if (client) {
        await client.enableCliProvider(row.id);
        notifyProvidersChanged();
      }
    },
    [client]
  );

  const ids = new Set([...configuredCliIds(dto)].map((id) => id.slice(4)));
  const blocks: Record<string, EngineBlock> = {};
  if (enabled) {
    for (const e of engines) {
      const reason = ids.has(e.detected.id) ? engineBlock(e) : null;
      if (reason) {
        blocks[e.detected.id] = reason;
      }
    }
  }
  return { rows: enabled ? engineRows(engines, ids) : [], run, blocks };
}
