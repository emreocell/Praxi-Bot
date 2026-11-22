import type { LocalModelsDto } from "../api/types";

type Hardware = LocalModelsDto["hardware"];
type Advice = LocalModelsDto["advice"];
type Model = Advice["models"][number];

const GIB = 1024 ** 3;

/** "16 GB" from bytes; null when the value is unknown. Memory sizes are shown in whole GB. */
export function gbLabel(bytes: number | null | undefined): string | null {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes) || bytes <= 0) {
    return null;
  }
  const gb = bytes / GIB;
  return `${gb >= 10 ? Math.round(gb) : Math.round(gb * 10) / 10} GB`;
}

export type HardwareFacts = {
  ram: string | null;
  cores: number | null;
  gpu: string | null;
  vram: string | null;
  unified: boolean;
  disk: string | null;
};

/** The probe's facts, each null when unknown, so the view can say "unknown" honestly. */
export function hardwareFacts(h: Hardware): HardwareFacts {
  return {
    ram: gbLabel(h.ram_bytes),
    cores: h.logical_cores ?? null,
    gpu: h.gpu_name?.trim() ? h.gpu_name.trim() : null,
    vram: gbLabel(h.gpu_vram_bytes),
    unified: h.unified_memory,
    disk: gbLabel(h.free_disk_bytes)
  };
}

/** True when nothing useful was detected (the probe failed or the machine hides everything). */
export function probeIsEmpty(h: Hardware): boolean {
  const f = hardwareFacts(h);
  return f.ram === null && f.gpu === null && f.cores === null;
}

/** The models worth showing: what fits, best first; models that do not fit are left out. */
export function fittingModels(a: Advice): Model[] {
  return a.models.filter((m) => m.fit !== "too_large");
}

/** The model to highlight: the catalog id the daemon recommended, when it is in the list. */
export function recommendedModel(a: Advice): Model | null {
  return a.recommended ? (a.models.find((m) => m.model_id === a.recommended) ?? null) : null;
}

/** A command is only offered for copying when it is a plain `ollama pull <id>` of the listed id. */
export function copyablePull(m: Model): string | null {
  return m.pull_command === `ollama pull ${m.model_id}` && /^[A-Za-z0-9][A-Za-z0-9._:/@+-]*$/.test(m.model_id)
    ? m.pull_command
    : null;
}

export type RuntimeState = "notInstalled" | "notRunning" | "running" | "unknown";

/** Ollama status for the user's next step; `unknown` when the daemon did not report the runtime. */
export function runtimeState(d: LocalModelsDto, id: "ollama" | "lmstudio"): RuntimeState {
  const r = d.runtimes.find((x) => x.id === id);
  if (!r) {
    return "unknown";
  }
  if (r.running) {
    return "running";
  }
  return r.installed === false ? "notInstalled" : "notRunning";
}
