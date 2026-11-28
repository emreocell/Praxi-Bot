import assert from "node:assert/strict";
import { test } from "node:test";
import { splitExternalLinks } from "./externalLinks.ts";

const urls = (text: string) => splitExternalLinks(text).filter((s) => s.kind === "url").map((s) => s.text);
const joined = (text: string) => splitExternalLinks(text).map((s) => s.text).join("");

test("text without a link is one plain segment", () => {
  assert.deepEqual(splitExternalLinks("Open a trial account please."), [{ kind: "text", text: "Open a trial account please." }]);
  assert.deepEqual(splitExternalLinks(""), []);
});

test("finds http, https, ftp-like and bare www addresses", () => {
  assert.deepEqual(urls("a http://x.io/p b https://y.io?q=1#z c ftp://f.io/a d sftp://s.io e www.z.io/p"), [
    "http://x.io/p",
    "https://y.io?q=1#z",
    "ftp://f.io/a",
    "sftp://s.io",
    "www.z.io/p"
  ]);
  assert.deepEqual(urls("HTTPS://EXAMPLE.COM/A"), ["HTTPS://EXAMPLE.COM/A"]);
});

test("trailing punctuation stays out of the address", () => {
  assert.deepEqual(urls("See https://x.io/a."), ["https://x.io/a"]);
  assert.deepEqual(urls("Is it https://x.io/a?"), ["https://x.io/a"]);
  assert.deepEqual(urls("(https://x.io/a)"), ["https://x.io/a"]);
  assert.deepEqual(urls("https://x.io/a, then www.y.io;"), ["https://x.io/a", "www.y.io"]);
  assert.deepEqual(urls("https://en.wiki.org/Foo_(bar)"), ["https://en.wiki.org/Foo_(bar)"]);
  assert.deepEqual(urls('say "https://x.io/a".'), ["https://x.io/a"]);
});

test("segments keep order and join back to the input", () => {
  const text = "Go to https://a.io/x. Then www.b.io, done";
  assert.deepEqual(splitExternalLinks(text), [
    { kind: "text", text: "Go to " },
    { kind: "url", text: "https://a.io/x" },
    { kind: "text", text: ". Then " },
    { kind: "url", text: "www.b.io" },
    { kind: "text", text: ", done" }
  ]);
  assert.equal(joined(text), text);
  assert.equal(joined("a\nhttp://x.io\nb"), "a\nhttp://x.io\nb");
});

test("lookalike text is not an address", () => {
  for (const text of [
    "http:/x.io",
    "hxxp://x.io",
    "http://",
    "http://.",
    "www.",
    "www. x",
    "awww.example.com",
    "sub.www.example.com",
    "xhttp://x.io",
    "user@www.example.com",
    "javascript:alert(1)",
    "example.com",
    "file:///etc/passwd"
  ]) {
    assert.deepEqual(urls(text), [], text);
    assert.equal(joined(text), text);
  }
});

test("very long input is split in linear time", () => {
  const started = Date.now();
  const long = "http://a.io/".repeat(50_000);
  assert.equal(splitExternalLinks(long).length, 1);
  assert.equal(joined(long), long);
  const noSpace = "www." + "a".repeat(1_000_000);
  assert.deepEqual(urls(noSpace), [noSpace]);
  const dots = "http://" + ".".repeat(500_000);
  assert.deepEqual(urls(dots), []);
  const many = "x http://a.io ".repeat(50_000);
  assert.equal(urls(many).length, 50_000);
  assert.ok(Date.now() - started < 5000);
});
