# Sorun giderme (teknik)

Önce `bot status` komutunu çalıştır. Arka plan servisinin (daemon) çalıştığını, anahtarının kabul edildiğini ve kaç onayın seni beklediğini gösterir. Terminal kullanmıyorsan önce sade dille yazılmış [Sık görülen sorunları çöz](fix-common-problems.md) sayfasına bak.

## Son günlükler ve destek paketi

**Ayarlar → Yardım → Son günlükleri görüntüle** yolunu aç (Yardım penceresinde
de bulunur). Ara veya düzey seç; **Gösterilen günlükleri kopyala**, ekranda
gördüğün arındırılmış kayıtları kopyalar. **Destek paketini önizle** dört dosya
hazırlar: `versions.json`, `diagnostics.json` (servisin hazır olma ve güvenli mod
bilgileri), `recent-logs.jsonl` ve arındırılmış `doctor.md`. **Destek paketini
kaydet** öncesinde her dosyanın önizlemesini aç. ZIP tam bu içerikleri kaydeder;
arama filtresinin gizlediği son kayıtlar da eklenir. Sohbet veya kimlik bilgisi
içermez, kendiliğinden yüklenmez. Gizli bilgiler güvenle ayıklanamazsa paket sunulmaz.

Terminalde `bot logs --since 15m --level warn`, son 15 dakikanın uyarılarını gösterir.
`bot logs --follow` yeni kayıtları bekler (durdurmak için Ctrl-C). `--since`, `2h`,
`1d` veya UTC Unix milisaniye zamanını da kabul eder. Yerel dosyaları okur, servis
kapalıyken de çalışır; servisin veri klasörünü kullan. Boş sonuç, filtrenin hiçbir
kayıtla eşleşmediği veya eski kayıtların yer açmak için kaldırıldığı anlamına gelebilir.

Dosyalar `<veri klasörü>/logs` içindedir: daemon ve yan servis yöneticisinin her
biri için en fazla 2 MiB boyutunda beş dosya tutulur; görüntüleyici en son 500
kaydı gösterir. `RUST_LOG` izleme düzeyini belirler (varsayılan `info`); `trace`
bile istek gövdelerini, başlıkları, kimlik bilgilerini ve kişisel yolları içermez.
Başarısız HTTP istekleri yol şablonu, durum, hata kodu ve üretilmiş istek kimliğiyle;
başlangıç adımları ve yeniden başlatma nedenleri de kaydedilir. Linux systemd
sunucularında aynı çıktı sistem günlüğüne gider: `journalctl -u praxi.service
--since '15 minutes ago'` (kurulumundaki birim adını kullan).
`GET /v1/logs/recent?level=warn&since=<milisaniye>` hem API erişim anahtarını hem
yerel sahip anahtarını gerektirir. Eşleşmiş cihazların tümüne kapalıdır.

## Tanılama (Ayarlar → Tanılama, `bot doctor`)

**Hata bildirmek için:** Yardım'ı aç ve *Tanılama bilgisini kopyala* düğmesini kullan. Önce kopyalayacağı metnin tamamını gösterir (sürümler, platform, kontrol sonuçları; anahtarlar, parolalar, e-posta adresleri ve kullanıcı klasörün gizlenir, hiçbir mesaj eklenmez) ve yalnızca sen *Kopyala*'ya basınca kopyalar. Hiçbir yere bir şey gönderilmez.

Masaüstü uygulamasının Ayarlar bölümünde bir **Tanılama** sayfası var. `bot doctor` ile aynı kontrolleri listeler: sağlayıcı anahtarları ve kodlama CLI'sinde oturum, yüklü sağlayıcılar, veri klasörü (Türkçe harfli dosya adı dahil), tarayıcı, masaüstü otomasyonu, eş / VPS bağlantısı, disk alanı ve saat. Platformun sunmadığı bir şey hata olarak değil, *kullanılamaz* olarak gösterilir.

- **Sayfayı okumak hiçbir şeyi değiştirmez.** Bazı sorunların onarım düğmesi vardır (veri klasörünü oluştur, sağlayıcıları yeniden yükle). Onarım yalnızca düğmesine basınca çalışır ve her deneme denetim günlüğüne yazılır. Başka hiçbir şey tek tıkla düzeltme olarak sunulmaz; API anahtarı asla yeniden üretilmez.
- **Destek raporunu kopyala** Markdown özeti kopyalar (sürümler, platform, kontrol sonuçları, ayar dosyalarının *adları*). Kullanıcı klasörün `~` olarak gösterilir; daemon'un bildiği gizli bilgiler (URL kodlu, base64 veya JSON kaçışlı hâlleri de), API anahtarı, anahtara benzeyen metinler, URL'lerdeki kimlik bilgileri ve sunucu adları, ayar dosyası ayrıştırma hatasının alıntıladığı satırlar, kullanıcı ve bilgisayar adların ile e-posta adreslerin gizlenir. Gizli bilgiler okunamazsa rapor oluşturulmaz. Paylaşmadan önce oku.
- **Eş / VPS bağlantısı**, daemon'un canlı bağlantısına göre denetlenir: çevrimdışı, eşleşmemiş bir sunucu ya da eş ağı olmayan bir daemon sorun olarak bildirilir. Oturumu doğrulanamayan bir kodlama CLI'si uyarıdır, asla "hazır" sayılmaz.
- **Sunucu ekranı (`host_display`)**, 7/24 çalışan bir Linux sunucuda gereksinimleri sırayla denetler ve genel bir uyarı yerine ilk eksik parçayı bildirir: `DISPLAY` tanımlı mı, ekran yanıt veriyor mu (ve `XAUTHORITY` tanımlı ve okunabilir mi), kullanılabilir, snap olmayan bir tarayıcı var mı (`BOT_BROWSER_PATH` veya `/opt/praxi-browser/chrome`). systemd'li bir sunucuda `sudo praxi doctor` çalıştır; böylece kontrol, kendi etkileşimli kabuğunun yerine servisin kendi kullanıcısı, ortamı (`/etc/praxi/praxi.env`) ve bağlama alanıyla çalışır.
- **Boş bellek (`free_ram`)** Linux'ta `/proc/meminfo` içindeki `MemAvailable` değerini okur ve 512 MiB'ın altında uyarır (`host.toml` içindeki `limits.max_computer_runs` değerini düşürmeyi önerir).
- Sayfanın arkasındaki daemon yolları `GET /v1/doctor`, `POST /v1/doctor/repair/{id}` ve `GET /v1/doctor/report`'tur (erişim anahtarı gerekir). Yalnızca bu bilgisayardan çağrılabilir; eşleşmiş telefonlar ve masaüstleri çağıramaz. `bot doctor`, terminal gerektiren daha fazla kontrol yapar (port yoklaması, 5 saniye başlangıç / 8 saniye toplam zaman aşımlı sınırlı başsız tarayıcı açılışı, `--online` bağlantı testleri).

## 7/24 çalışan sunucu

Günlükler, yeniden başlatma, yükseltme, yedek ve sunucu çevrimdışı / yavaş başlangıç / eş tablosu [Sunucu işletimi](../host-operations.md) sayfasında.

## `daemon not reachable at http://127.0.0.1:47821` (çıkış kodu 3)

Daemon çalışmıyor ya da başka bir portu dinliyor.

- Başlat: `bot serve` (ya da onu kendisi başlatan masaüstü uygulamasını aç).
- `serve` için `--port` veya `BOT_PORT` mu kullandın? İstemci komutunda da aynısını kullan ya da `--url http://127.0.0.1:<port>` ver.
- Port zaten kullanımda: `bot serve --port 47822` ile başlat ve istemcileri ona yönelt.

## Kimlik doğrulama reddedildi (çıkış kodu 4)

Erişim anahtarı ya da Host/Origin denetimi başarısız oldu.

- İstemci `<veri klasörü>/api-token` dosyasını okur. Daemon'u farklı bir `--data-dir` veya `BOT_DATA_DIR` ile başlattıysan istemciye de aynısını ver.
- API yalnızca loopback adreslerini kabul eder. Önüne başka bir Host adı olan bir vekil sunucu (proxy) koyma.
- Daemon'a karşı bir web arayüzü mü geliştiriyorsun? `bot serve` öncesinde kaynağını (origin) `BOT_ALLOWED_ORIGINS` içine (virgülle ayırarak) ekle.

## `no usable provider; run bot setup or bot providers add`

`bot serve`, kurabileceği etkin bir sağlayıcı bulamadı.

- `bot providers list` neyin ayarlı olduğunu gösterir. Anahtarı eksik bir sağlayıcı, başlangıçta bir uyarıyla atlanır (`provider <id> skipped: ...`).
- Bir tane ekle: `bot providers add gemini --key-env GEMINI_API_KEY`.
- Ortam değişkenlerindeki anahtarlar komut çalıştığında okunur; değişkeni aynı kabukta tanımla, masaüstü uygulamasında da uygulamanın değişkeni tanımladıktan sonra açıldığından emin ol (ya da anahtarı gizli istemle kaydet).

## `connection test failed`

`bot providers add` ve `bot providers test` sağlayıcıyı çağırır. Anahtarı, ağı ya da vekil sunucuyu, yerel sağlayıcılarda da Ollama veya LM Studio'nun çalıştığını ve modelin indirildiğini kontrol et. `--no-test` sağlayıcıyı denemeden kaydeder.

## Bir bot "has no usable profile" yanıtı veriyor

Botun kayıtlı profili geçerli bir profil değil. Bu, CLI sihirbazının (`bot setup`) kaydettiği ilk botta olur. Bunun yerine `bot bots add` ile bir profil dosyasından bot oluştur; bkz. [Botlar ve profiller](../bots-and-profiles.md). Bir profilde en az `id` ve `name` içeren `[bot]` ile `executor` içeren `[models]` bulunmalı.

## `bot bots add` reddedildi (çıkış kodu 6)

Mesaj sorunu adıyla söyler: geçersiz TOML, eksik `executor`, bilinmeyen bir izin ön ayarı (`read-only`, `read-and-draft`, `approve-destructive` veya `full` kullan) ya da hatalı bir `[tools]` glob'u.

## Bot durup bekliyor

Seni bekliyor: bir onay ya da bir soru var. Sayısını görmek için `bot status` çalıştır, ya da sohbet çıktısında `[approval needed]` / `[question]` satırlarını izle ve `bot approve <id>` / `bot answer <id> "..."` ile yanıtla. Zamanlanmış çalışmaların onayları 30 dakika sonra zaman aşımına uğrar, ardından çalışma duraklatılır.

## Bir araç çağrısı reddediliyor

- Politika **deny** diyor: `read-only` ön ayarı ya da bir `deny` kuralı. Botun profiline, etkin politikası için de `GET /v1/bots/{id}` yoluna bak.
- Masaüstü araçları: masaüstü uygulamasında kontrolü sen aldın ya da botu duraklattın. Kontrolü geri ver.
- Hiç masaüstü aracı yok: yalnızca Windows'ta bulunurlar ve `BOT_DESKTOP=off` onları kapatır.

## "Her zaman izin ver" tutmuyor ya da hiç sormuyor

`bot grants list <bot-id>` kalıcı izinleri gösterir. Botun yeniden sormasını istiyorsan birini geri al. Para ve "asla" listesindeki işlemler, tasarım gereği hiçbir izne bakmadan her zaman sorar.

## Gemini'den hız sınırı hataları (HTTP 429)

Ücretsiz kullanım sınırına takıldın, büyük olasılıkla o modelin günlük istek sınırına. Sıfırlanmasını bekle, daha yüksek sınırlı bir modele geç, zamanlamaları azalt ya da ücretli bir anahtar ya da başka bir sağlayıcı kullan. Bkz. [Gemini ücretsiz kullanım](../providers-and-models.md#gemini-free-tier).

## Güvenli mod: her şey kapalı başlasın

Kendi kendine iş yapan bir şey sorunu düzeltmene engel olduğunda kullan: her açılışta hata veren bir çalışma başlatan zamanlama, açılışta takılan bir MCP sunucusu, ekranı denetlemeyi bırakmayan bir bot.

Güvenli mod **yalnızca servis başlarken** ve yalnızca senin tarafından seçilir:

- **Masaüstü uygulaması:** Ayarlar → Tanılama → **Güvenli modda yeniden başlat**, ya da servis ayağa kalkmayınca görünen ekrandaki **Güvenli modda başlat**.
- **Terminal:** `bot serve --safe` ya da `bot serve` öncesinde `BOT_SAFE_MODE=1` ayarla.

Açıkken uygulama bir şerit gösterir ("Güvenli mod. Araçlar, zamanlamalar ve kanallar kapalı.") ve `bot doctor`, Ayarlar → Tanılama ile `GET /v1/health` (`safe_mode`) da bunu söyler. Yine de sohbet edebilirsin (model araçsız yanıt verir; her araç çağrısı reddedilir) ve botları, zamanlamaları ve ayarları okuyup değiştirebilirsin.

Kapalı olanlar: tüm araçlar, zamanlamalar ve iş akışları (hiçbir şey tetiklenmez), Telegram köprüsü ve diğer kanallar, MCP sunucuları ve uygulamalar, e-posta / takvim / web bağlayıcıları, belleğin hatırlaması ve geri yazması, masaüstü arka ucu, eş ya da sunucu ağı. Kayıtlı iş akışları asla yeniden oynatılmaz (eşleşen bir sohbet satırına model yanıt verir, çalıştırma yolu reddedilir) ve kodlama CLI yürütücüleri reddedilir. Yarım kalan çalışmalar sürdürülmez: servis başlarken çalışan ya da onay bekleyen çalışmalar, her yeniden başlatmada olduğu gibi duraklatılmış işaretlenir ve sonraki normal başlangıç da onları sürdürmez; görünür kalırlar. Kapalı parçaların ayar sayfaları, güvenli moddan çıkana kadar "ayarlanmamış" yanıtı verir.

Hiçbir şey silinmez ya da değişmez: botlar, sohbetler, zamanlamalar, kanallar ve ayarlar olduğu gibi kalır; `mcp.toml` dosyası açılmaz bile. Çıkmak için şeritteki **Normal başlat**'a bas (ya da servisi `--safe` ve `BOT_SAFE_MODE` olmadan yeniden başlat); her şey geri gelir. Servis çalışırken hiçbir araç, bot ya da eşleşmiş cihaz güvenli modu açıp kapatamaz. Bu bir hata ayıklama yardımıdır, kötü amaçlı yazılıma karşı bir korunak değildir.

## Zamanlamalar tetiklenmiyor

- Daemon, vakti geldiğinde çalışıyor olmalı. Kaçırılan tetiklemeler varsayılan olarak atlanır.
- `bot schedules list` bir sonraki çalışma zamanını gösterir. İfade anlaşılmadıysa `add` başarısız olur; [Zamanlanmış işler](../scheduled-tasks.md) sayfasından bir kalıp dene ya da `POST /v1/schedules/parse` ile önizle.
- Takvim ifadeleri, `--tz` vermezsen UTC kullanır.
- Aynı zamanlamanın önceki çalışması hâlâ bekliyor ya da çalışıyorsa yeni tetikleme atlanır.

## Telegram

- `pair` daemon'a ihtiyacı olduğunu söylüyor: önce `bot serve` başlat; kod çalışan daemon tarafından üretilir ve 10 dakika sonra geçerliliğini yitirir.
- `setup` sonrası hiçbir şey olmuyor: `bot serve`'i yeniden başlat; `bot channels telegram status` ayarlı / etkin / çalışıyor durumunu gösterir.
- Aynı bot anahtarını yoklayan iki program çakışır (Telegram 409 yanıtı verir). Çalışan her daemon için ayrı bir anahtar kullan ve botta webhook tanımlı olmadığından emin ol.
- Eşleşmen söyleniyor: yeni bir kodla bota `/pair <kod>` gönder.

## Gizli bilgi deposu parola soruyor

Şifreli dosya arka ucunda parola `BOT_SECRETS_PASSPHRASE` değişkeninden ya da bir istemden gelir. Sende varsa `--secrets-backend keyring` ile işletim sisteminin anahtar zincirini zorla.

## Tarayıcı açılmıyor ya da zaman aşımına uğruyor

- **Sınırlı başlangıç ve oturum beklemeleri**: `browser_open`, tarayıcıyı başlatmayı (Chrome/Chromium'u çalıştırma, `DevToolsActivePort` bekleme ve ilk sekmeyi açma) her denemede **40 saniye** ile, oturumu tutan başka bir tarayıcı çağrısını beklemeyi de **45 saniye** ile sınırlar. Zaman aşımı olursa `browser_open`'ı döngüye sokma; neyin eksik olduğunu görmek için o bilgisayarda `bot doctor` çalıştır (Linux sunucuda `sudo praxi doctor`).
- **Chrome'un erken kapanma mesajları**: Tarayıcı işlemi başlangıçta kapanırsa hata, Chrome çıktısının son satırlarını (`Browser said: ...`) içerir ve bilinen nedenleri vurgular: eksik X ekranı, eksik ya da okunamayan `XAUTHORITY`, engellenmiş Chrome kum havuzu (Ubuntu 24.04'te eksik AppArmor profili) ya da eksik paylaşılan kütüphaneler.
- **Snap tarayıcılar**: Snap ile paketlenmiş Chromium/Chrome, sertleştirilmiş Linux servisinde desteklenmez. Snap olmayan bir tarayıcı kur ya da `BOT_BROWSER_PATH` ayarla.
- **Linux'ta yavaş Xvfb başlangıcı**: Yoğun bir açılışta X11 ekranının yoklaması başarısız olursa daemon, yeniden başlatmaya kadar başsız modda takılı kalmak yerine **15 saniye** sonra yeniden yoklar.

## Windows SmartScreen kurulum programını engelliyor

Sürümler henüz kod imzalı değil. **Daha fazla bilgi**'yi, sonra **Yine de çalıştır**'ı yalnızca kurulum programını kendin derlediysen ya da ona güveniyorsan seç.

## Hata raporu için bilgi toplama

`bot --version`, işletim sistemin, tam komut ve hata, masaüstü uygulaması mı CLI mı kullandığın ve ilgili daemon çıktısını ekle. `api-token` dosyanı, API anahtarlarını ya da veri klasörünün içeriğini yapıştırma.
