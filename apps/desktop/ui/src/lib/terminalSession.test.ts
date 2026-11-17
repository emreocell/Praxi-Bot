import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type TerminalApiLike,
  type TerminalOutputDto,
  TerminalLink,
  base64ToBytes,
  bytesToBase64,
  inputChunks
} from "./terminalSession.ts";

const enc = (s: string) => bytesToBase64(new TextEncoder().encode(s));

function fake(outputs: Array<TerminalOutputDto | Error>, hangAtEnd = false) {
  const calls: string[] = [];
  const typed: string[] = [];
  const api: TerminalApiLike = {
    open: async (cols, rows) => {
      calls.push(`open ${cols}x${rows}`);
      return { id: "t1" };
    },
    output: async (_id, after, _wait, signal) => {
      calls.push(`output ${after}`);
      const next = outputs.shift();
      if (!next && hangAtEnd) {
        // A long poll with nothing to say: ends when the view closes.
        await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve()));
        throw new Error("aborted");
      }
      if (!next) {
        throw Object.assign(new Error("gone"), { status: 404, code: "terminal_gone" });
      }
      if (next instanceof Error) {
        throw next;
      }
      return next;
    },
    input: async (_id, b64) => {
      typed.push(new TextDecoder().decode(base64ToBytes(b64)));
    },
    resize: async (_id, c, r) => {
      calls.push(`resize ${c}x${r}`);
    },
    close: async () => {
      calls.push("close");
    }
  };
  return { api, calls, typed };
}

test("output is read by absolute offset, in order, until the shell exits", async () => {
  const { api, calls } = fake([
    { data: enc("hel"), next: 3, dropped: false },
    { data: enc("lo"), next: 5, dropped: false },
    { data: "", next: 5, dropped: false, exited: 0 }
  ]);
  const got: string[] = [];
  let ended: number | null | undefined;
  const link = new TerminalLink(
    api,
    {
      write: (b) => got.push(new TextDecoder().decode(b)),
      ended: (c) => {
        ended = c;
      },
      failed: () => assert.fail("no failure expected")
    },
    async () => undefined
  );
  await link.start(80, 24);
  await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(got, ["hel", "lo"]);
  assert.equal(ended, 0);
  assert.deepEqual(calls.slice(0, 4), ["open 80x24", "output 0", "output 3", "output 5"]);
});

test("typed text is sent in order and a pasted command gets no newline from the link", async () => {
  const { api, typed } = fake([{ data: "", next: 0, dropped: false }], true);
  const link = new TerminalLink(api, { write() {}, ended() {}, failed() {} }, async () => undefined);
  // Not started: nothing is sent.
  link.send("ignored");
  await link.start(80, 24);
  link.send("ls -la");
  link.send(" /var");
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual(typed, ["ls -la", " /var"]);
  assert.equal(inputChunks("x".repeat(20000)).length, 3);
  link.dispose();
});

test("a link that keeps failing gives up with the error; closing the view closes the shell", async () => {
  const boom = Object.assign(new Error("net"), { status: 0, code: "network" });
  const { api, calls } = fake([boom, boom, boom, boom, boom]);
  let failed: unknown;
  const link = new TerminalLink(api, { write() {}, ended() {}, failed: (e) => (failed = e) }, async () => undefined);
  await link.start(80, 24);
  await new Promise((r) => setTimeout(r, 20));
  assert.equal(failed, boom);
  assert.equal(calls.filter((c) => c.startsWith("output")).length, 5);
  const second = fake([{ data: "", next: 0, dropped: false }], true);
  const l2 = new TerminalLink(second.api, { write() {}, ended() {}, failed() {} }, async () => undefined);
  await l2.start(80, 24);
  l2.dispose();
  assert.ok(second.calls.includes("close"));
});
