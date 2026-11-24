// Pure helpers for the Download block. Used by the browser (app.js) and by the
// node tests, so they must not touch the DOM or the network.

export const RELEASES_URL = "https://github.com/emreocell/Praxi-Bot/releases";
export const API_URL = "https://api.github.com/repos/emreocell/Praxi-Bot/releases?per_page=5";

const SHA_RE = /\b[0-9a-f]{64}\b/i;
const INSTALLER_RE = /-setup\.exe$/i;

/** Only GitHub release downloads of this repository are ever linked. */
export function isTrustedDownloadUrl(url) {
  try {
    const u = new URL(url);
    return (
      u.protocol === "https:" &&
      u.hostname === "github.com" &&
      u.pathname.startsWith("/emreocell/Praxi-Bot/releases/download/")
    );
  } catch {
    return false;
  }
}

/**
 * Take the GitHub "list releases" answer and return what the page needs, or
 * null when there is nothing usable (the page then keeps its static link).
 * The SHA-256 comes from the release body line that names the installer, else
 * from the asset digest GitHub computed itself. Nothing is invented.
 */
export function summarizeRelease(releases) {
  if (!Array.isArray(releases)) return null;
  const rel = releases.find((r) => r && !r.draft && typeof r.tag_name === "string");
  if (!rel) return null;
  const assets = Array.isArray(rel.assets) ? rel.assets : [];
  const installer = assets.find(
    (a) => a && typeof a.name === "string" && INSTALLER_RE.test(a.name) && isTrustedDownloadUrl(a.browser_download_url),
  );
  const htmlUrl = typeof rel.html_url === "string" && rel.html_url.startsWith(RELEASES_URL) ? rel.html_url : RELEASES_URL;
  let sha = null;
  if (installer) {
    const body = typeof rel.body === "string" ? rel.body : "";
    const line = body.split(/\r?\n/).find((l) => l.includes(installer.name) && SHA_RE.test(l));
    if (line) sha = line.match(SHA_RE)[0].toLowerCase();
    else if (typeof installer.digest === "string" && installer.digest.startsWith("sha256:")) {
      const d = installer.digest.slice(7);
      if (d.length === 64 && SHA_RE.test(d)) sha = d.toLowerCase();
    }
  }
  return {
    tag: rel.tag_name,
    prerelease: !!rel.prerelease,
    releaseUrl: htmlUrl,
    installerName: installer ? installer.name : null,
    installerUrl: installer ? installer.browser_download_url : null,
    sizeMb: installer && Number.isFinite(installer.size) ? Math.round(installer.size / 1048576) : null,
    sha256: sha,
  };
}
