import { API_URL, summarizeRelease } from "./release.mjs";

const box = document.getElementById("download");
if (box) {
  const t = box.dataset;
  fetch(API_URL, { headers: { Accept: "application/vnd.github+json" }, referrerPolicy: "no-referrer", credentials: "omit" })
    .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
    .then((json) => {
      const s = summarizeRelease(json);
      if (!s || !s.installerUrl) return;
      const a = document.getElementById("dl-button");
      a.href = s.installerUrl;
      a.textContent = `${t.btn} ${s.tag}` + (s.sizeMb ? ` (${s.sizeMb} MB)` : "");
      document.getElementById("dl-release").href = s.releaseUrl;
      const hash = document.getElementById("dl-hash");
      if (s.sha256) {
        hash.querySelector("code").textContent = s.sha256;
        hash.hidden = false;
      }
    })
    .catch(() => {
      /* Offline or rate-limited: the static link to the releases page stays. */
    });
}
