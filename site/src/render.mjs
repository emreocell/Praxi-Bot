import { DOCS, REPO, SITE, locales, shots } from "./content.mjs";

const esc = (s) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

// Links that differ per language (the translated guides exist for these).
const links = {
  en: {
    security: `${DOCS}/guide/security.md`,
    plainSafe: `${DOCS}/guide/keep-your-bot-safe.md`,
    linux: `${DOCS}/guide/linux-host.md`,
    docs: `${DOCS}/guide/index.md`,
  },
  tr: {
    security: `${DOCS}/guide/security.md`,
    plainSafe: `${DOCS}/guide/tr/keep-your-bot-safe.md`,
    linux: `${DOCS}/guide/tr/linux-host.md`,
    docs: `${DOCS}/guide/index.md`,
  },
};
const common = {
  releases: `${REPO}/releases`,
  gfi: `${REPO}/labels/good%20first%20issue`,
  contributing: `${REPO}/blob/main/CONTRIBUTING.md`,
};

/** Relative prefix from a page to the site root. */
const root = (code) => (code === "en" ? "" : "../");

export function renderPage(code) {
  const t = locales[code];
  const l = links[code];
  const r = root(code);
  const other = code === "en" ? "tr" : "en";
  const canonical = code === "en" ? SITE : `${SITE}tr/`;
  const features = t.features
    .map((f, i) => {
      const s = shots[i];
      return `<article class="card feature">
        <img src="${r}img/${s.file}" width="${s.w}" height="${s.h}" alt="${esc(f.alt)}" loading="${i === 0 ? "eager" : "lazy"}" decoding="async">
        <h3>${esc(f.title)}</h3>
        <p>${esc(f.text)}</p>
      </article>`;
    })
    .join("\n");
  const safe = t.safe.map((s) => `<li class="card"><h3>${esc(s.title)}</h3><p>${esc(s.text)}</p></li>`).join("\n");
  const who = t.who.map((w) => `<li>${esc(w)}</li>`).join("\n");
  return `<!doctype html>
<html lang="${t.lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(t.title)}</title>
<meta name="description" content="${esc(t.description)}">
<meta name="color-scheme" content="dark">
<meta name="theme-color" content="#111217">
<link rel="canonical" href="${canonical}">
<link rel="alternate" hreflang="en" href="${SITE}">
<link rel="alternate" hreflang="tr" href="${SITE}tr/">
<link rel="alternate" hreflang="x-default" href="${SITE}">
<link rel="icon" type="image/webp" href="${r}img/ghost-sm.webp">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Praxi-Bot">
<meta property="og:title" content="${esc(t.title)}">
<meta property="og:description" content="${esc(t.description)}">
<meta property="og:url" content="${canonical}">
<meta property="og:image" content="${SITE}og.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="${esc(t.ogAlt)}">
<meta property="og:locale" content="${code === "en" ? "en_US" : "tr_TR"}">
<meta name="twitter:card" content="summary_large_image">
<link rel="stylesheet" href="${r}style.css">
<script type="module" src="${r}app.js"></script>
</head>
<body>
<a class="skip" href="#main">${esc(t.skip)}</a>
<header class="top">
  <div class="wrap bar">
    <a class="brand" href="${code === "en" ? "./" : "./"}"><img src="${r}img/ghost-sm.webp" width="32" height="32" alt="" decoding="async"><span>Praxi-Bot</span></a>
    <nav aria-label="Main">
      <a href="#features">${esc(t.nav.features)}</a>
      <a href="#download">${esc(t.nav.download)}</a>
      <a href="#safe">${esc(t.nav.safe)}</a>
      <a href="#who">${esc(t.nav.who)}</a>
      <a href="#help">${esc(t.nav.help)}</a>
      <a class="lang" href="${t.switchHref}" hreflang="${other}" lang="${other}">${esc(t.switchLabel)}</a>
    </nav>
  </div>
</header>
<main id="main">
<section class="hero wrap">
  <div class="hero-text">
    <p class="badge" role="status">${esc(t.badge)}</p>
    <h1>${esc(t.heroTitle)}</h1>
    <p class="lead">${esc(t.heroLead)}</p>
    <p class="cta">
      <a class="btn primary" href="#download">${esc(t.ctaDownload)}</a>
      <a class="btn" href="${REPO}">${esc(t.ctaGithub)}</a>
    </p>
    <p class="note">${esc(t.heroNote)}</p>
  </div>
  <img class="ghost" src="${r}img/ghost.webp" width="512" height="512" alt="" decoding="async" fetchpriority="high">
</section>

<section id="features" class="wrap">
  <h2>${esc(t.featuresTitle)}</h2>
  <div class="grid">
${features}
  </div>
  <p class="note">${esc(t.shotsNote)}</p>
</section>

<section id="download" class="wrap" data-btn="${esc(t.downloadBtn)}">
  <div class="card dl">
    <h2>${esc(t.downloadTitle)}</h2>
    <p>${esc(t.downloadLead)}</p>
    <p><a id="dl-button" class="btn primary" href="${common.releases}">${esc(t.downloadFallback)}</a></p>
    <p><a id="dl-release" href="${common.releases}">${esc(t.releaseLink)}</a></p>
    <p id="dl-hash" hidden>${esc(t.shaLabel)} <code></code><br><span class="note">${esc(t.shaHow)}</span></p>
    <p class="warn">${esc(t.smartscreen)}</p>
    <p>${esc(t.linux)} <a href="${l.linux}">${esc(t.linuxLink)}</a>.</p>
    <p class="note">${esc(t.privacy)}</p>
  </div>
</section>

<section id="who" class="wrap">
  <h2>${esc(t.whoTitle)}</h2>
  <ul class="plain">
${who}
  </ul>
  <p class="note">${esc(t.notFor)}</p>
</section>

<section id="safe" class="wrap">
  <h2>${esc(t.safeTitle)}</h2>
  <ul class="grid3">
${safe}
  </ul>
  <p><a class="btn" href="${l.security}">${esc(t.safeLink)}</a> <a href="${l.plainSafe}">${code === "en" ? "Plain-words version" : "Sade anlatım"}</a></p>
  <p class="note">${esc(t.safeHonest)}</p>
</section>

<section id="help" class="wrap">
  <div class="card">
    <h2>${esc(t.helpTitle)}</h2>
    <p>${esc(t.helpText)}</p>
    <p><a class="btn primary" href="${common.gfi}">${esc(t.helpGfi)}</a> <a class="btn" href="${common.contributing}">${esc(t.helpContrib)}</a> <a href="${l.docs}">${esc(t.docsLink)}</a></p>
  </div>
</section>
</main>
<footer class="wrap foot">
  <p>Praxi-Bot · ${esc(t.footerLicense)} ${esc(t.footerNoTrack)}</p>
</footer>
</body>
</html>
`;
}
