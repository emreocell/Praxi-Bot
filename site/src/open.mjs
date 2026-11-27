const copy = {
  en: {
    title: "Open the request in Praxi",
    body: "Use Praxi on the computer where the request started. Open the desktop app to fill in the secure form.",
    button: "Open Praxi",
    fallback: "You can also start Praxi yourself and find the request under Needs you."
  },
  tr: {
    title: "İsteği Praxi’de aç",
    body: "İsteğin başladığı bilgisayarda Praxi’yi kullan. Güvenli formu doldurmak için masaüstü uygulamasını aç.",
    button: "Praxi’yi aç",
    fallback: "Praxi’yi kendin de açabilir ve isteği Senden bekleyenler bölümünde bulabilirsin."
  }
};

export function renderOpenPage(lang) {
  const text = copy[lang];
  const assets = lang === "tr" ? "../../" : "../";
  return `<!doctype html>
<html lang="${lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<meta name="robots" content="noindex">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'self'; style-src 'self'; base-uri 'none'; form-action 'none'">
<title>${text.title}</title>
<link rel="stylesheet" href="${assets}style.css">
<script type="module" src="${assets}open-app.js"></script>
</head>
<body>
<main class="wrap">
  <h1>${text.title}</h1>
  <p>${text.body}</p>
  <p><a id="open-app" hidden>${text.button}</a></p>
  <p>${text.fallback}</p>
</main>
</body>
</html>
`;
}
