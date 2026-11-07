/** Largest GIF kept as-is (animation would be lost by re-encoding). */
export const MAX_GIF_BYTES = 400 * 1024;
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const AVATAR_SIZE = 128;

export type AvatarError = "tooLarge" | "unreadable";

export class AvatarFileError extends Error {
  readonly reason: AvatarError;
  constructor(reason: AvatarError) {
    super(reason);
    this.reason = reason;
  }
}

function readAsDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(typeof r.result === "string" ? r.result : "");
    r.onerror = () => reject(new AvatarFileError("unreadable"));
    r.readAsDataURL(file);
  });
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new AvatarFileError("unreadable"));
    img.src = src;
  });
}

/**
 * Turn a picked image into a small data URL for the bot profile.  GIFs are
 * kept untouched (size limited); other images are cropped to a square and
 * downscaled.
 */
export async function avatarFromFile(file: File): Promise<string> {
  if (file.type === "image/gif") {
    if (file.size > MAX_GIF_BYTES) {
      throw new AvatarFileError("tooLarge");
    }
    return readAsDataUrl(file);
  }
  if (file.size > MAX_IMAGE_BYTES) {
    throw new AvatarFileError("tooLarge");
  }
  const img = await loadImage(await readAsDataUrl(file));
  const side = Math.min(img.naturalWidth, img.naturalHeight);
  if (side === 0) {
    throw new AvatarFileError("unreadable");
  }
  const canvas = document.createElement("canvas");
  canvas.width = AVATAR_SIZE;
  canvas.height = AVATAR_SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    throw new AvatarFileError("unreadable");
  }
  ctx.drawImage(
    img,
    (img.naturalWidth - side) / 2,
    (img.naturalHeight - side) / 2,
    side,
    side,
    0,
    0,
    AVATAR_SIZE,
    AVATAR_SIZE
  );
  return canvas.toDataURL("image/png");
}
