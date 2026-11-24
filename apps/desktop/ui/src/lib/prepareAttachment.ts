import { MAX_ATTACHMENT_BYTES, MAX_IMAGE_BYTES, MAX_IMAGE_EDGE, MAX_TEXT_BYTES } from "./attachments.ts";

export class AttachmentPreparationError extends Error {
  readonly key: string;
  constructor(key: string) { super(key); this.key = key; }
}

export interface PreparedAttachment {
  blob: Blob;
  kind: "image" | "document" | "text";
  resized?: boolean;
}

interface DecodedImage { width: number; height: number; close(): void }
export interface ImagePreparation {
  decode(blob: Blob): Promise<DecodedImage>;
  encode(image: DecodedImage, width: number, height: number, mime: string): Promise<Blob>;
}

const browserImages: ImagePreparation = {
  decode: (blob) => createImageBitmap(blob, { imageOrientation: "from-image" }),
  encode: (image, width, height, mime) => new Promise((resolve, reject) => {
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext("2d");
    if (!context) { reject(new Error("canvas unavailable")); return; }
    context.drawImage(image as ImageBitmap, 0, 0, width, height);
    canvas.toBlob((blob) => blob ? resolve(blob) : reject(new Error("image encoding failed")), mime, 0.85);
  })
};

/** Bound images before upload. Re-encoding also follows the photo's orientation. */
export async function downscaleImage(blob: Blob, images: ImagePreparation = browserImages): Promise<PreparedAttachment> {
  let image: DecodedImage | undefined;
  try {
    image = await images.decode(blob);
    if (!image.width || !image.height || image.width * image.height > 40_000_000) {
      throw new Error("image dimensions exceed the preparation budget");
    }
    const scale = Math.min(1, MAX_IMAGE_EDGE / Math.max(image.width, image.height));
    if (scale === 1 && blob.size <= MAX_IMAGE_BYTES) return { blob, kind: "image" };
    let width = Math.max(1, Math.round(image.width * scale));
    let height = Math.max(1, Math.round(image.height * scale));
    for (let attempt = 0; attempt < 4; attempt++) {
      const encoded = await images.encode(image, width, height, blob.type === "image/jpeg" ? "image/jpeg" : "image/webp");
      if (encoded.size <= MAX_IMAGE_BYTES) return { blob: encoded, kind: "image", resized: true };
      width = Math.max(1, Math.floor(width * 0.7));
      height = Math.max(1, Math.floor(height * 0.7));
    }
    throw new AttachmentPreparationError("chat.attach.imageTooLarge");
  } catch (error) {
    if (error instanceof AttachmentPreparationError) throw error;
    throw new AttachmentPreparationError("chat.attach.imageFailed");
  } finally {
    image?.close();
  }
}

/** Early feedback only: the daemon still independently sniffs and validates all bytes. */
export async function prepareAttachment(file: File, images?: ImagePreparation): Promise<PreparedAttachment> {
  if (!file.size) throw new AttachmentPreparationError("chat.attach.empty");
  if (file.size > MAX_ATTACHMENT_BYTES) throw new AttachmentPreparationError("chat.attach.tooLarge");
  const bytes = new Uint8Array(await file.slice(0, 512).arrayBuffer());
  const starts = (...prefix: number[]) => prefix.every((b, i) => bytes[i] === b);
  let mime: string | undefined;
  if (starts(137, 80, 78, 71, 13, 10, 26, 10)) mime = "image/png";
  else if (starts(255, 216, 255)) mime = "image/jpeg";
  else if (starts(71, 73, 70, 56, 55, 97) || starts(71, 73, 70, 56, 57, 97)) mime = "image/gif";
  else if (starts(82, 73, 70, 70) && String.fromCharCode(...bytes.slice(8, 12)) === "WEBP") mime = "image/webp";
  if (mime) return downscaleImage(file.slice(0, file.size, mime), images);
  if (starts(37, 80, 68, 70, 45)) return { blob: file, kind: "document" };
  if (starts(80, 75, 3, 4) && /\.(docx|xlsx|pptx)$/i.test(file.name)) return { blob: file, kind: "document" };
  if (/^(audio|video)\//.test(file.type) || /\.(mp3|mp4|wav|ogg|flac|mov|avi|webm|exe|dll|msi|zip|doc|xls|ppt)$/i.test(file.name)) {
    throw new AttachmentPreparationError("chat.attach.unsupported");
  }
  // Never preview SVG/HTML or execute a file. Only accepted raster bytes get an object URL.
  if (file.size > MAX_TEXT_BYTES && (file.type.startsWith("text/") || /\.(txt|md|csv|json|xml|ya?ml|log)$/i.test(file.name))) {
    throw new AttachmentPreparationError("chat.attach.textTooLarge");
  }
  if (file.size <= MAX_TEXT_BYTES) {
    const full = new Uint8Array(await file.arrayBuffer());
    try {
      new TextDecoder("utf-8", { fatal: true }).decode(full);
      if (!full.some((b) => b < 32 && b !== 9 && b !== 10 && b !== 13)) return { blob: file, kind: "text" };
    } catch { /* Binary content is refused below. */ }
  }
  throw new AttachmentPreparationError("chat.attach.unsupported");
}
