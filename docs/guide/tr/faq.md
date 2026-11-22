# SSS

### Praxi kullanıma hazır mı?
Erken geliştirme aşamasında. README'de kararlı bir API ve sürüm olmadığı, bazı şeylerin bozulacağı yazıyor.
Denemek ve üzerine bir şeyler kurmak için hazırlandı.

### Bir şey bilgisayarımdan dışarı çıkıyor mu?
İstemler ve araç sonuçları, yapılandırdığın model sağlayıcıya gider; Telegram'ı açarsan Telegram mesajları
Telegram üzerinden geçer. Telemetri yoktur. Yerel bir modelle (Ollama, LM Studio) ve Telegram olmadan,
hiçbir şey bilgisayarından çıkmaz.

### Para harcar mı?
Praxi'nin kendisi ücretsiz ve açık kaynaklıdır (Apache-2.0). Model sağlayıcına ödeme yaparsın; ücretsiz
katmanda ya da yerel modellerle hiçbir şey ödemezsin. Bkz. [Sağlayıcılar ve modeller](../providers-and-models.md).
API, token kullanımını ve katalogda fiyat varsa tahmini maliyeti bildirir (`/v1/usage`; masaüstü uygulamasında
bir kullanım ekranı var).

### Hangi işletim sistemleri çalışıyor?
CLI ve daemon Windows, Linux ve macOS'ta derlenir. Masaüstü denetimi (başka uygulamalarda tıklama ve yazma)
Windows'ta (UI Automation) ve X11 ya da sanal Xvfb ekranı altındaki Linux'ta (AT-SPI2 ve XTest; Wayland
oturumları reddedilir) çalışır. macOS arka ucu henüz bir taslaktır ve kullanılamaz. Masaüstü uygulaması ve
yükleyici Windows içindir. Tarayıcı araçları, Chromium tabanlı bir tarayıcıyı DevTools protokolüyle sürer ve
böyle bir tarayıcının bulunduğu her yerde çalışır.

### Terminal olmadan kullanabilir miyim?
Windows'ta evet: masaüstü uygulamasını kur, sihirbazı izle, sohbet et, onayla ve arayüzden zamanla.

### Bot, bana sormadan bir şey yapabilir mi?
Yalnızca politikanın izin verdiği şeyleri. Varsayılan olarak okumak serbesttir, değişiklikler sorar.
Bunu bot başına ve kategori başına genişletebilirsin. Para ve yıkıcı kabuk komutları her zaman sorar.
Bkz. [Sohbet ve onaylar](../chatting-and-approvals.md).

### Bir botu hemen nasıl durdururum?
Sohbette ya da Telegram'da `/stop` gönder, genel kısayola bas (masaüstü uygulamasında varsayılan `Ctrl+Alt+Esc`),
masaüstü uygulamasında kontrolü devral ya da daemon'u durdur (Ctrl-C); bu, canlı çalışmaları duraklatır.
`POST /v1/runs/{id}/cancel` tek bir çalışmayı iptal eder.

### Birden fazla bot çalıştırabilir miyim?
Evet. Her birinin kendi profili, sohbetleri ve izinleri vardır; bkz. [Botlar ve profiller](../bots-and-profiles.md).

### Verilerim nerede ve nasıl yedeklerim?
Veri klasöründe (`--data-dir` / `BOT_DATA_DIR`; yoksa `praxi-bot` için işletim sisteminin kullanıcıya özel
uygulama verisi klasörü): bir SQLite veritabanı, olay günlükleri, `providers.toml`, beceriler, iş akışları ve
API belirteci. `bot backup create <dosya>` komutunu çalıştır: parolayla şifrelenmiş tek bir dosya yazar ve
bunu `bot backup restore` ile geri yükleyebilirsin (bkz. [Şifreli yedekleme ve geri yükleme](../backup.md)).
API anahtarları işletim sisteminin anahtarlığında (ya da veri klasöründeki şifreli bir dosyada) durur ve
yalnızca `--include-secrets` ile yedeğe dahil edilir.

### Docker içinde nasıl yedek alırım?

Kapta ayarlı bir parola yoktur. Kabuğunda `BACKUP_PASSPHRASE` değişkenini tanımla ve adıyla geçir:
`docker exec -e BACKUP_PASSPHRASE praxi-host bot backup create
/tmp/praxi.bak --passphrase-env BACKUP_PASSPHRASE`, ardından dosyayı `docker cp` ile dışarı al. Geri
yüklemek için daemon'un durmuş olması gerekir (aynı birimde tek seferlik bir kap); bkz.
[Docker ana makinesi](../docker-host.md).

### Bir botu nasıl daha hızlı ve ucuz yaparım?
Küçük bir `classifier` modeli kullan, tekrarlayan işler için [iş akışları](../workflows.md) kaydet,
iyi giden bir çalışmadan sonra **Bunu hatırla**'ya bas ve botun zorlandığı siteler ve uygulamalar için
[beceriler](../skills.md) yaz.

### Bir iş akışını göstererek kaydedebilir miyim?
Henüz değil (**Planlandı**). Bugün iş akışları senin yazdığın TOML dosyalarıdır ve "Hatırla", başarılı bir
çalışmanın yolunu önbelleğe alır. Bkz. [İş akışları](../workflows.md).

### Uzun süreli hafızası var mı?
Depoda bir hafıza kütüphanesi var (turdan önce hatırlama, sonra çıkarma), ancak daemon bunu henüz etkinleştirmiyor
(**Planlandı**). Sohbetler kendi geçmişini tutar.

### MCP sunucuları ya da eklentiler ekleyebilir miyim?
MCP sunucuları için evet: `bot serve`, `mcp.toml` dosyasını yükler ve etkin sunucuları çalıştırır; bunları
`/v1/mcp/servers` üzerinden yönetebilirsin. Bkz. [MCP sunucuları](../mcp-servers.md). Eklentiler hâlâ
**Planlandı**: tasarım araçlar, sağlayıcılar ve kanallar için eklentiler öngörüyor, ancak son kullanıcılar için
henüz bir eklenti API'si yok.

### Bir bot başka bir bilgisayarı yönetebilir mi?
Evet, deneysel olarak. Bir bot, eşleştirilmiş bir bilgisayara yönlendirilebilir (profilin bilgisayar hedefi;
bkz. [Botlar ve profiller](../bots-and-profiles.md)) ve daemon, onu sunan eş ağını başlatır. Node-agent
(`bot-node`, `join`, `run`, `devices` komutları), sahip olduğun bir makinenin gelen port açmadan yönetilmesini
sağlar. Uzak bilgisayar çalışmaları yalnızca masaüstü araçlarını sunar. Bu yolun tamamını **deneysel** say.

### Başka hangi mesajlaşma uygulamaları destekleniyor?
Yalnızca Telegram. E-posta ya da sohbet servisleri gibi diğerleri tasarım belgelerinde (**Planlandı**).

### Ses, görüntü ya da video üretimi var mı?
Hayır. Bunlar yalnızca tasarım yol haritasında (**Planlandı**).

### Türkçe kullanabilir miyim?
Evet. Masaüstü uygulaması İngilizce, Türkçe ve Almanca olarak gelir; komut satırı sihirbazı İngilizce ve
Türkçe sunar, zamanlama ayrıştırıcısı `her gün 09:00` gibi Türkçe ifadeleri anlar. Model, ona hangi dilde
yazarsan o dilde yanıt verir ve profilin `language` ayarı bir tercih belirleyebilir.

### Nasıl güncellerim?
Kaynaktan yeniden derle ya da daha yeni bir yükleyici kur; veri klasörün korunur. Proje hızla değiştiği için
önce veri klasörünü yedekle.

### Nasıl kaldırırım?
Windows: kaldırıcıyı kullan (**Uygulama verilerini sil**'i işaretlemedikçe verilerini korur). CLI: ikili dosyayı sil;
istersen veri klasörünü ve anahtarlığındaki `provider.*` ile Telegram kayıtlarını da sil (`bot secrets delete`).

### Hataları nerede bildirir ya da nasıl katkıda bulunurum?
Bkz. [CONTRIBUTING.md](../../../CONTRIBUTING.md).
