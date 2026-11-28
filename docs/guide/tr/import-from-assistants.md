# Diğer asistanlardan içe aktarma

Praxi, Claude, Cursor, VS Code, Codex, OpenClaw, Hermes ya da seçtiğin bir
klasördeki mevcut kurulumları okuyabilir ve güvenli olanları Praxi'ye kopyalar
(tarama sırasında hiçbir şey ağa gönderilmez).

## İçe aktarma ekranını aç

- **Ayarlar → Genel → Başlangıç ve kısayollar → Diğer asistanlardan içe
  aktar** kartı (**Kurulumları ara**).
- İlk çalıştırma sihirbazı: erken bir adımdaki bağlantı.
- **Yeni bot → Dosyadan içe aktar** tek bir dışa aktarılmış profili alır; bkz.
  [Botlar ve profiller](bots-and-profiles.md).

Ekran neyi okuduğunu anlatır, hangi uygulamaları bulduğunu gösterir ve neyi
getireceğini işaretlemeni sağlar. **İçe aktar** seçimleri yerel daemon üzerinden
uygular; `bot import detect` ve `bot import run` aynı işi terminalden yapar
(bkz. [`bot import`](cli-reference.md#bot-import)).

Praxi, sormadan botların veya anahtarların üzerine yazmaz. Sırlar, taşımayı
seçmediğin sürece diğer uygulamanın deposunda kalabilir.