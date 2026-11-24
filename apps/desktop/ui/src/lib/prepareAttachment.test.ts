import assert from "node:assert/strict";
import { test } from "node:test";
import { MAX_ATTACHMENT_BYTES, MAX_IMAGE_BYTES, MAX_TEXT_BYTES } from "./attachments.ts";
import { downscaleImage, prepareAttachment, type ImagePreparation } from "./prepareAttachment.ts";

test("preparation rejects size, empty, unsupported and malformed text locally", async () => {
  await assert.rejects(prepareAttachment({ size: MAX_ATTACHMENT_BYTES + 1 } as File), { key: "chat.attach.tooLarge" });
  await assert.rejects(prepareAttachment(new File([], "empty.txt")), { key: "chat.attach.empty" });
  await assert.rejects(prepareAttachment(new File([new Uint8Array([255, 0])], "fake.txt")), { key: "chat.attach.unsupported" });
  await assert.rejects(prepareAttachment(new File(["ID3"], "voice.mp3", { type: "audio/mpeg" })), { key: "chat.attach.unsupported" });
  await assert.rejects(prepareAttachment(new File([new Uint8Array(MAX_TEXT_BYTES + 1)], "large.txt")), { key: "chat.attach.textTooLarge" });
  assert.equal((await prepareAttachment(new File(["%PDF-1.7"], "invoice.pdf"))).kind, "document");
  assert.equal((await prepareAttachment(new File(["<svg>untrusted text</svg>"], "image.svg", { type: "image/svg+xml" }))).kind, "text");
});

test("raster sniffing ignores the claimed MIME; resized bytes fit the provider limit and keep aspect ratio", async () => {
  const dimensions: number[][] = [];
  let closed = false;
  const images: ImagePreparation = {
    decode: async (blob) => {
      assert.equal(blob.type, "image/png");
      return { width: 4000, height: 2000, close: () => { closed = true; } };
    },
    encode: async (_image, width, height, mime) => {
      dimensions.push([width, height]);
      assert.equal(mime, "image/webp");
      return new Blob(["prepared"], { type: mime });
    }
  };
  const prepared = await prepareAttachment(new File([new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])], "fake.txt", { type: "text/plain" }), images);
  assert.deepEqual(dimensions, [[2048, 1024]]);
  assert.equal(prepared.resized, true);
  assert.equal(await prepared.blob.text(), "prepared");
  assert.equal(closed, true);
});

test("oversize encodings are reduced again, decode failures are plain errors, and bitmaps are closed", async () => {
  let attempts = 0;
  let closed = false;
  const images: ImagePreparation = {
    decode: async () => ({ width: 1000, height: 500, close: () => { closed = true; } }),
    encode: async () => new Blob([new Uint8Array(++attempts === 1 ? MAX_IMAGE_BYTES + 1 : 10)])
  };
  const prepared = await downscaleImage(new Blob([new Uint8Array(MAX_IMAGE_BYTES + 1)]), images);
  assert.equal(prepared.blob.size, 10);
  assert.equal(attempts, 2);
  assert.equal(closed, true);
  await assert.rejects(downscaleImage(new Blob(), { ...images, decode: async () => { throw new Error("decode failed"); } }), { key: "chat.attach.imageFailed" });
});
