import { ApiError } from "./client.ts";
import type { HostSetupJob, HostSetupStep, SshKeyHint, StartHostSetupInput } from "./types.ts";

const pass = (id: string, title: string, detail: string): HostSetupStep => ({
  id,
  title,
  status: "pass",
  detail,
  hint: null
});

/** Steps reported after each poll once the host key is accepted (mock mode only). */
const TIMELINE: HostSetupStep[][] = [
  [
    pass("hostkey", "Server identity (host key)", "first contact, pinned SHA256:mock"),
    pass("connect", "SSH login", "logged in as ubuntu")
  ],
  [
    pass("preflight.os", "Operating system", "Ubuntu 24.04"),
    pass("preflight.arch", "CPU", "aarch64"),
    pass("preflight.sudo", "Administrator rights", "sudo works without a password"),
    pass("preflight.disk", "Disk space", "38 GB free")
  ],
  [
    {
      id: "install",
      title: "Install Praxi",
      status: "skip",
      detail: "Praxi 0.1.0 already installed in host mode",
      hint: null
    }
  ],
  [pass("pair", "Pair this computer", "paired with oracle and granted screen view + input")],
  [
    pass("health.service", "Service running in host mode", "praxi.service active"),
    pass("health.channel", "Paired channel", "answers over the encrypted peer link"),
    pass("health.live_view", "Live view", "1 target")
  ]
];

/** Update of a server that is already added (`?update=fail` style hosts fail at the install row). */
const UPDATE_TIMELINE: HostSetupStep[][] = [
  [pass("hostkey", "Server identity (host key)", "pinned"), pass("connect", "SSH login", "logged in as ubuntu")],
  [pass("preflight.os", "Operating system", "Ubuntu 24.04"), pass("preflight.disk", "Disk space", "38 GB free")],
  [pass("install.stage", "Upload install scripts", "2 scripts staged"), pass("install.snapshot", "Snapshot existing data", "snapshot: /var/backups/praxi/praxi-data.tgz")],
  [pass("install.run", "Install Praxi", "Praxi v0.1.0 installed in host mode; previous version kept at /opt/praxi.backup-20261123T100000Z")],
  [pass("pair", "Pair this computer", "already connected")],
  [pass("health.service", "Service running in host mode", "praxi.service active"), pass("health.version", "Version", "0.1.0")]
];

const FAILURES: Record<string, { at: number; step: HostSetupStep }> = {
  fail: {
    at: 0,
    step: {
      id: "connect",
      title: "SSH login",
      status: "fail",
      detail: "the server rejected the login (key or user)",
      hint: "Check the user name and the key file."
    }
  },
  fresh: {
    at: 2,
    step: {
      id: "install",
      title: "Install Praxi",
      status: "fail",
      detail: "Praxi is not installed on this server, and automatic install needs a published release",
      hint: "No release has been published yet."
    }
  }
};

/** A scripted wizard backend: one poll moves one step, so screenshots can freeze any state. */
export class MockHostSetup {
  private job: HostSetupJob | null = null;
  private scenario = "ok";
  private polls = 0;
  private timeline = TIMELINE;

  /** Update an already added server: one poll moves one step. */
  startUpdate(hostId: string): HostSetupJob {
    this.scenario = hostId.includes("fail") ? "update-fail" : "update";
    this.timeline = UPDATE_TIMELINE;
    this.polls = 0;
    this.job = {
      id: "mock-update",
      kind: "update",
      state: "running",
      target: "ubuntu@203.0.113.7:22",
      steps: [],
      notes: [],
      host_key: null,
      host_id: hostId,
      host_name: null,
      view_targets: null,
      install_available: true
    };
    return structuredClone(this.job);
  }

  start(input: StartHostSetupInput): HostSetupJob {
    const host = input.host.toLowerCase();
    this.scenario = host.includes("fail") ? "fail" : host.includes("fresh") ? "fresh" : "ok";
    this.polls = 0;
    this.timeline = TIMELINE;
    this.job = {
      id: "mock-job",
      state: "awaiting_host_key",
      target: `${input.user ?? "ubuntu"}@${input.host}:${input.port ?? 22}`,
      steps: [],
      notes: [],
      host_key: {
        host: input.host,
        port: input.port ?? 22,
        fingerprint: "SHA256:uJ3Zr0m1cK2mQe8y0aVfWc7rPq9XhT4nLd5oBsE6gYk"
      },
      host_id: null,
      host_name: null,
      view_targets: null,
      install_available: false
    };
    return structuredClone(this.job);
  }

  private get hold(): number {
    const w = globalThis as { __praxiMockSetup?: { until?: number } };
    return w.__praxiMockSetup?.until ?? Number.POSITIVE_INFINITY;
  }

  get(id: string): HostSetupJob {
    const job = this.require(id);
    if (job.state === "running" && this.polls < this.hold) {
      this.advance(job);
    }
    return structuredClone(job);
  }

  private advance(job: HostSetupJob): void {
    if (this.scenario === "update-fail" && this.polls === 3) {
      this.polls += 1;
      job.steps.push({
        id: "install.run",
        title: "Install Praxi",
        status: "fail",
        detail: "exit 1: attestation verification failed | [praxi-install] rollback complete; previously active services restarted and active",
        hint: null
      });
      job.state = "failed";
      return;
    }
    const failure = FAILURES[this.scenario];
    const index = this.polls;
    this.polls += 1;
    if (failure && index === failure.at) {
      if (failure.at > 0) job.steps.push(...this.timeline[index]);
      else job.steps.push(pass("hostkey", "Server identity (host key)", "first contact, pinned"));
      job.steps.push(failure.step);
      job.state = "failed";
      return;
    }
    job.steps.push(...(this.timeline[index] ?? []));
    if (index >= this.timeline.length - 1) {
      job.state = "succeeded";
      job.host_id = "mockhost1";
      job.host_name = "Oracle";
      job.view_targets = this.scenario === "ok" ? 1 : null;
    }
  }

  answerKey(id: string, accept: boolean): HostSetupJob {
    const job = this.require(id);
    job.host_key = null;
    if (accept) {
      job.state = "running";
    } else {
      job.state = "failed";
      job.steps.push({
        id: "hostkey",
        title: "Server identity (host key)",
        status: "fail",
        detail: "not accepted (SHA256:mock)",
        hint: null
      });
    }
    return structuredClone(job);
  }

  cancel(id: string): HostSetupJob {
    const job = this.require(id);
    if (job.state === "running" || job.state === "awaiting_host_key") {
      job.state = "cancelled";
      job.host_key = null;
    }
    return structuredClone(job);
  }

  keys(): SshKeyHint[] {
    return [
      { name: "id_ed25519", path: String.raw`C:\Users\you\.ssh\id_ed25519` },
      { name: "oracle.key", path: String.raw`C:\Users\you\.ssh\oracle.key` }
    ];
  }

  private require(id: string): HostSetupJob {
    if (!this.job || this.job.id !== id) throw new ApiError(404, "not_found", "setup job");
    return this.job;
  }
}
