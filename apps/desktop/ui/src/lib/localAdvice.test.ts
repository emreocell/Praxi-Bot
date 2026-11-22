import assert from "node:assert/strict";
import { test } from "node:test";
import type { LocalModelsDto } from "../api/types.ts";
import { copyablePull, fittingModels, gbLabel, hardwareFacts, probeIsEmpty, runtimeState } from "./localAdvice.ts";

const base: LocalModelsDto = {
  hardware: { ram_bytes: 16 * 1024 ** 3, logical_cores: 8, gpu_name: " RTX ", gpu_vram_bytes: 8 * 1024 ** 3, unified_memory: false, free_disk_bytes: null, os: "linux", arch: "x86_64" },
  advice: {
    basis: "vram",
    recommended: "a:8b",
    models: [
      { model_id: "a:8b", display_name: "A", params_billions: 8, fit: "comfortable", needs_gib: 6.7, pull_command: "ollama pull a:8b", disk_ok: true },
      { model_id: "a:70b", display_name: "A70", params_billions: 70, fit: "too_large", needs_gib: 47, pull_command: "ollama pull a:70b", disk_ok: true }
    ]
  },
  runtimes: [{ id: "ollama", installed: false, running: false, installed_models: [] }]
};

test("hardware facts are honest about unknown values", () => {
  assert.equal(gbLabel(null), null);
  assert.equal(gbLabel(16 * 1024 ** 3), "16 GB");
  const f = hardwareFacts(base.hardware);
  assert.deepEqual([f.ram, f.gpu, f.vram, f.disk], ["16 GB", "RTX", "8 GB", null]);
  assert.equal(probeIsEmpty({ ...base.hardware, ram_bytes: null, logical_cores: null, gpu_name: null }), true);
});

test("only fitting models are listed and only plain pull commands are copyable", () => {
  assert.deepEqual(fittingModels(base.advice).map((m) => m.model_id), ["a:8b"]);
  const m = base.advice.models[0]!;
  assert.equal(copyablePull(m), "ollama pull a:8b");
  assert.equal(copyablePull({ ...m, pull_command: "ollama pull a:8b; rm -rf ~" }), null);
  assert.equal(copyablePull({ ...m, model_id: "-x", pull_command: "ollama pull -x" }), null);
});

test("runtime state tells not installed from not running", () => {
  assert.equal(runtimeState(base, "ollama"), "notInstalled");
  assert.equal(runtimeState(base, "lmstudio"), "unknown");
  assert.equal(runtimeState({ ...base, runtimes: [{ id: "ollama", installed: true, running: false, installed_models: [] }] }, "ollama"), "notRunning");
});
