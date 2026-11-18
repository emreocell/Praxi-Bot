import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { AvatarFileError, MAX_GIF_BYTES, avatarFromFile } from "./avatar.ts";

type G = Record<string, unknown>;
const g = globalThis as unknown as G;
const saved = { FileReader: g.FileReader, Image: g.Image, document: g.document };

afterEach(() => {
  g.FileReader = saved.FileReader;
  g.Image = saved.Image;
  g.document = saved.document;
});

const fakeFile = (type: string, size: number): File => ({ type, size }) as unknown as File;

function installReader(result: unknown, fail = false): void {
  g.FileReader = class {
    result: unknown = null;
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    readAsDataURL(): void {
      queueMicrotask(() => {
        if (fail) this.onerror?.();
        else {
          this.result = result;
          this.onload?.();
        }
      });
    }
  };
}

function installImage(width: number, height: number, fail = false): void {
  g.Image = class {
    naturalWidth = width;
    naturalHeight = height;
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    set src(_v: string) {
      queueMicrotask(() => (fail ? this.onerror?.() : this.onload?.()));
    }
  };
}

function installCanvas(draws: unknown[][], ctxAvailable = true): void {
  const canvas = {
    width: 0,
    height: 0,
    getContext: () => (ctxAvailable ? { drawImage: (...a: unknown[]) => draws.push(a) } : null),
    toDataURL: (t: string) => `data:${t};base64,OUT`
  };
  g.document = { createElement: () => canvas };
}

async function reason(p: Promise<unknown>): Promise<string> {
  try {
    await p;
  } catch (e) {
    assert.ok(e instanceof AvatarFileError);
    return e.reason;
  }
  return "no error";
}

test("a GIF within the limit is kept untouched as a data URL", async () => {
  installReader("data:image/gif;base64,AAA");
  assert.equal(await avatarFromFile(fakeFile("image/gif", MAX_GIF_BYTES)), "data:image/gif;base64,AAA");
});

test("a GIF over the limit is refused as too large", async () => {
  installReader("unused");
  assert.equal(await reason(avatarFromFile(fakeFile("image/gif", MAX_GIF_BYTES + 1))), "tooLarge");
});

test("an image over 5 MiB is refused as too large", async () => {
  assert.equal(await reason(avatarFromFile(fakeFile("image/png", 5 * 1024 * 1024 + 1))), "tooLarge");
});

test("a read error surfaces as unreadable", async () => {
  installReader(null, true);
  assert.equal(await reason(avatarFromFile(fakeFile("image/gif", 10))), "unreadable");
});

test("an image that fails to decode, or has no pixels, is unreadable", async () => {
  installReader("data:image/png;base64,AAA");
  installImage(10, 10, true);
  assert.equal(await reason(avatarFromFile(fakeFile("image/png", 10))), "unreadable");
  installImage(0, 50);
  assert.equal(await reason(avatarFromFile(fakeFile("image/png", 10))), "unreadable");
});

test("a missing 2d context is unreadable", async () => {
  installReader("data:image/png;base64,AAA");
  installImage(10, 10);
  installCanvas([], false);
  assert.equal(await reason(avatarFromFile(fakeFile("image/png", 10))), "unreadable");
});

test("a wide image is cropped to a centred square and scaled to 128 px PNG", async () => {
  installReader("data:image/jpeg;base64,AAA");
  installImage(300, 100);
  const draws: unknown[][] = [];
  installCanvas(draws);
  const out = await avatarFromFile(fakeFile("image/jpeg", 1000));
  assert.equal(out, "data:image/png;base64,OUT");
  assert.equal(draws.length, 1);
  assert.deepEqual(draws[0].slice(1), [100, 0, 100, 100, 0, 0, 128, 128]);
});
