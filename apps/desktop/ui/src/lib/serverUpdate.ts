import type { HostSetupJob } from "../api/types";

/**
 * "Update server": pure helpers for the version badge, the progress rows, the
 * plain-language failure and the copy-paste fallback. No screen code here, so
 * the flow is testable without rendering anything.
 */

const TAG = /^v\d+\.\d+\.\d+(?:[.+-][0-9A-Za-z.+-]+)?$/;

function parse(v: string): { core: number[]; pre: string } | null {
  const m = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$/.exec(v.trim());
  return m ? { core: [Number(m[1]), Number(m[2]), Number(m[3])], pre: m[4] ?? "" } : null;
}

function comparePre(a: string, b: string): number {
  if (a === b) return 0;
  if (a === "") return 1; // a release is newer than its own pre-release
  if (b === "") return -1;
  const x = a.split(".");
  const y = b.split(".");
  for (let i = 0; i < Math.max(x.length, y.length); i += 1) {
    if (x[i] === undefined) return -1;
    if (y[i] === undefined) return 1;
    const nx = /^\d+$/.test(x[i]) ? Number(x[i]) : null;
    const ny = /^\d+$/.test(y[i]) ? Number(y[i]) : null;
    if (nx !== null && ny !== null) {
      if (nx !== ny) return nx < ny ? -1 : 1;
    } else if ((nx === null) !== (ny === null)) {
      return nx === null ? 1 : -1;
    } else if (x[i] !== y[i]) {
      return x[i] < y[i] ? -1 : 1;
    }
  }
  return 0;
}

/** Older servers may report the published tag only as their build ID. */
export function reportedRelease(releaseTag?: string | null, buildId?: string | null): string | null {
  return [releaseTag, buildId].find((tag): tag is string => Boolean(tag && TAG.test(tag))) ?? null;
}

/** True only when both versions are understood and the server is strictly older than this app. */
export function serverIsOlder(server: string | null | undefined, app: string | null | undefined): boolean {
  const s = server ? parse(server) : null;
  const a = app ? parse(app) : null;
  return s !== null && a !== null && compareParsed(s, a) < 0;
}

function compareParsed(s: { core: number[]; pre: string }, a: { core: number[]; pre: string }): number {
  for (let i = 0; i < 3; i += 1) {
    if (s.core[i] !== a.core[i]) return s.core[i] < a.core[i] ? -1 : 1;
  }
  return comparePre(s.pre, a.pre);
}

export type UpdateState = "older" | "current" | "newer" | "unknown";

/**
 * Where a server stands against this app. Every published build reports the same cargo version
 * (`0.1.0`), so once the app knows its release tag the SERVER's tag decides: a different older tag
 * is "older", the same tag is "current", and a server that reports no tag is "unknown", never
 * "current". Without an app tag (local build) only the version numbers can be compared.
 */
export function updateState(
  server: { version: string; releaseTag?: string | null },
  app: { version: string | null | undefined; releaseTag?: string | null }
): UpdateState {
  const st = server.releaseTag ? parse(server.releaseTag) : null;
  const at = app.releaseTag ? parse(app.releaseTag) : null;
  if (st && at) {
    const order = compareParsed(st, at);
    return order < 0 ? "older" : order > 0 ? "newer" : "current";
  }
  if (serverIsOlder(server.version, app.version)) return "older";
  const sv = server.version ? parse(server.version) : null;
  const av = app.version ? parse(app.version) : null;
  if (sv && av && compareParsed(sv, av) > 0) return "newer";
  if (!app.releaseTag) return sv && av ? "current" : "unknown";
  return "unknown";
}

/** The progress rows, in order. The installer does download, check, install and restart as one verified run. */
export const UPDATE_ROWS = ["connect", "backup", "install", "check"] as const;
export type UpdateRow = (typeof UPDATE_ROWS)[number];
export type RowState = "pending" | "running" | "done" | "failed";

export function rowOf(stepId: string): UpdateRow | null {
  if (stepId === "hostkey" || stepId === "connect" || stepId === "prepare" || stepId.startsWith("preflight")) return "connect";
  if (stepId === "install.stage" || stepId === "install.snapshot" || stepId === "install.gh") return "backup";
  if (stepId === "install" || stepId === "install.run") return "install";
  if (stepId === "pair" || stepId.startsWith("health")) return "check";
  return null;
}

/** The step whose success closes a row (rows without one close when the next row starts). */
const CLOSER: Partial<Record<UpdateRow, (id: string) => boolean>> = {
  connect: (id) => id === "connect",
  install: (id) => id === "install" || id === "install.run"
};

export function rowStates(job: Pick<HostSetupJob, "state" | "steps">): Record<UpdateRow, RowState> {
  const out = {} as Record<UpdateRow, RowState>;
  const failedAt = UPDATE_ROWS.findIndex((r) => job.steps.some((s) => s.status === "fail" && rowOf(s.id) === r));
  const lastReached = UPDATE_ROWS.reduce((n, r, i) => (job.steps.some((s) => rowOf(s.id) === r) ? i : n), -1);
  const active = job.state === "running" || job.state === "awaiting_host_key";
  const lastClosed =
    lastReached >= 0 &&
    (CLOSER[UPDATE_ROWS[lastReached]] ? job.steps.some((s) => CLOSER[UPDATE_ROWS[lastReached]]?.(s.id) && s.status !== "fail") : false);
  // The row in progress: the last reported one, or the one after it once it closed.
  const current = lastReached < 0 ? 0 : lastClosed ? lastReached + 1 : lastReached;
  UPDATE_ROWS.forEach((r, i) => {
    if (failedAt >= 0) out[r] = i < failedAt ? "done" : i === failedAt ? "failed" : "pending";
    else if (job.state === "succeeded") out[r] = "done";
    else if (active) out[r] = i < current ? "done" : i === current ? "running" : "pending";
    else out[r] = i < lastReached || (i === lastReached && lastClosed) ? "done" : "pending";
  });
  return out;
}

export type UpdateFailure = "verify" | "rolled_back" | "unreachable" | "host_key" | "no_ssh" | "generic";

/** Which plain message fits what went wrong (the engine's own text is only in the log). */
export function updateFailure(job: Pick<HostSetupJob, "state" | "steps">): UpdateFailure | null {
  if (job.state !== "failed") return null;
  const bad = job.steps.find((s) => s.status === "fail");
  if (!bad) return "generic";
  const text = `${bad.detail} ${bad.hint ?? ""}`.toLowerCase();
  if (bad.id === "hostkey") return "host_key";
  if (bad.id === "connect" || bad.id === "prepare") return bad.id === "prepare" ? "no_ssh" : "unreachable";
  if (/attestation|verification|digest|checksum|sha256|signature|hash/.test(text)) return "verify";
  if (/rollback complete|rolled back|previous version/.test(text)) return "rolled_back";
  return "generic";
}

/**
 * The verified bootstrap from docs/guide/release-linux.md, pinned to the release this app was built
 * from, for a server Praxi cannot reach by itself. `null` when the app does not know its release.
 */
export function updateCommands(tag: string | null | undefined): string | null {
  if (!tag || !TAG.test(tag)) return null;
  return `(
  set -euo pipefail
  version=${tag}
  work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
  base="https://github.com/emreocell/Praxi-Bot/releases/download/$version"
  for asset in install-linux.sh praxi-linux.sigstore.json; do
    curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 "$base/$asset" -o "$work/$asset"
  done
  gh attestation verify "$work/install-linux.sh" --bundle "$work/praxi-linux.sigstore.json" \
    --repo emreocell/Praxi-Bot --hostname github.com \
    --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
    --source-ref "refs/tags/$version" --deny-self-hosted-runners \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256
  sudo bash "$work/install-linux.sh" --update --version "$version"
)`;
}
