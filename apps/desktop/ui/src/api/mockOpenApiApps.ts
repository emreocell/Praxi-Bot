import { ApiError } from "./client.ts";
import type { OpenApiApp, OpenApiAppInput, OpenApiAppUpdate, OpenApiOperation } from "./types.ts";

const OPS: Array<Pick<OpenApiOperation, "method" | "path" | "summary" | "tags" | "effect" | "always_ask"> & { id: string }> = [
  { id: "listProducts", method: "GET", path: "/products", summary: "List products", tags: ["products"], effect: "read", always_ask: false },
  { id: "getProduct", method: "GET", path: "/products/{id}", summary: "Get one product", tags: ["products"], effect: "read", always_ask: false },
  { id: "createProduct", method: "POST", path: "/products", summary: "Add a product", tags: ["products"], effect: "write", always_ask: false },
  { id: "deleteProduct", method: "DELETE", path: "/products/{id}", summary: "Remove a product", tags: ["products"], effect: "destructive", always_ask: false },
  { id: "createCheckoutSession", method: "POST", path: "/checkout/sessions", summary: "Start a checkout", tags: ["payments"], effect: "write", always_ask: true },
];

/** Review-state stand-in for `/v1/openapi-apps/*`: one demo API, no network, no spec parsing. */
export class MockOpenApiApps {
  private apps: OpenApiApp[] = [];

  route(method: string, seg: string[], body: unknown): unknown {
    const [, id, action] = seg;
    if (!id) {
      if (method === "GET") return structuredClone(this.apps);
      return this.add(body as OpenApiAppInput);
    }
    const i = this.apps.findIndex((a) => a.id === id);
    if (i < 0) throw new ApiError(404, "not_found", `app ${id}`);
    if (method === "DELETE") {
      this.apps.splice(i, 1);
      return undefined;
    }
    if (method === "PUT") return this.update(i, body as OpenApiAppUpdate);
    if (action === "refresh") return structuredClone(this.apps[i]);
    return structuredClone(this.apps[i]);
  }

  private add(input: OpenApiAppInput): OpenApiApp {
    if (!input.spec_url && !input.spec_text) throw new ApiError(400, "bad_request", "give exactly one of `spec_url` and `spec_text`");
    const id = input.name.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "").slice(0, 20) || "api";
    const app: OpenApiApp = {
      id,
      name: input.name,
      title: "Demo shop",
      version: "1.0.0",
      base_url: input.base_url || "https://shop.example.test/api",
      spec_url: input.spec_url ?? null,
      auth: { kind: input.auth?.kind ?? "none", name: input.auth?.name ?? null, username: input.auth?.username ?? null, secret_set: !!input.auth?.secret },
      operations: OPS.map((o) => ({
        name: `api_${id}_${o.id}`, method: o.method, path: o.path, summary: o.summary, tags: o.tags,
        effect: o.effect, always_ask: o.always_ask, enabled: false, bots: null,
      })),
      skipped: [],
      warnings: [],
      security_hints: ["bearerAuth: http bearer"],
      enabled_count: 0,
      max_enabled: 50,
    };
    this.apps.push(app);
    return structuredClone(app);
  }

  private update(i: number, input: OpenApiAppUpdate): OpenApiApp {
    const app = this.apps[i];
    if (input.name) app.name = input.name;
    if (input.base_url) app.base_url = input.base_url;
    if (input.auth) app.auth = { kind: input.auth.kind, name: input.auth.name ?? null, username: input.auth.username ?? null, secret_set: !!input.auth.secret || app.auth.secret_set };
    if (input.operations) {
      const on = new Map(input.operations.map((o) => [o.name, o.bots ?? null]));
      for (const op of app.operations) {
        op.enabled = on.has(op.name);
        op.bots = on.get(op.name) ?? null;
      }
      app.enabled_count = on.size;
    }
    return structuredClone(app);
  }
}
