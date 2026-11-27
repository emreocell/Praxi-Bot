# Web araçları: ara ve sayfa oku

Her bot, tarayıcı açmadan web'de bir şeye bakabilir. Web araçları varsayılan olarak açıktır ve
kurulum gerektirmez. Bunlar hızlı katmandır. [Tarayıcı araçları](bots-and-profiles.md) ise
JavaScript isteyen, girişle açılan ya da tıklayarak gezmeyi gerektiren sayfalar için durur.

| Araç | Ne yapar |
| --- | --- |
| `web_search` | Web'de arar, başlıkları, URL'leri ve kısa özetleri döndürür. |
| `web_extract` | Bir URL'yi HTTP ile çeker ve sayfayı okunabilir Markdown olarak döndürür (ana içerik, tablolar, bağlantılar). Bir sayfayı okumak için bunu kullan. |
| `web_feed` | Bir RSS, RDF ya da Atom akışını çeker ve ayrıştırır. |
| `web_sitemap` | Bir `sitemap.xml` dosyasını, site haritası dizinini ya da metin site haritasını çeker ve ayrıştırır. |
| `web_metadata` | Sayfa başlığı, açıklama, kanonik URL, OpenGraph, Twitter kartı, akışlar ve JSON-LD. |

İsteğe bağlı [web kazıma eklentisi](web-scraping.md), daha temiz makale metni, bağlantı listeleri
ve tek bir sitenin küçük taramaları için `web_scrape` aracını ekler. Varsayılan olarak kurulu
gelmez.

Güncel bir bilgi gerektiren tipik bir soru, önce `web_search`, sonra en iyi sonuçta `web_extract`
ile çözülür. Tarayıcı, yalnızca sayfanın kendisi gerçek bir tarayıcı gerektirdiğinde kullanılır.

“Genel asistan” ve “Tarayıcı araştırmacısı” hazır ayarlarından oluşturulan botlar bu araçları
kullanabilir. Kendi `[tools] allow` listesi olan bir bota `"web_*"` ekle (bkz.
[Botlar ve profiller](bots-and-profiles.md)).

## Arama arka ucu

Ayar yapmadan arama, hesap ya da API anahtarı gerektirmeyen bir yedek zincir kullanır
(`duckduckgo` → `duckduckgo_lite` → `bing`). Bir arka uç bot doğrulaması ya da hız sınırı
döndürürse o arka uç kısa bir bekleme süresine girer ve `web_search` ayarlı bir sonraki arka uca
geçer.

**Sunucuda bu zincir çoğunlukla çalışmaz.** DuckDuckGo ve Bing, veri merkezlerinin ve bulut
makinelerinin adreslerini robot sayar ve sonuç yerine bir robot denetimi sayfası gönderir. Praxi bu
sayfaları tanır (Cloudflare "Just a moment", bir captcha, DuckDuckGo'nun "bots use DuckDuckGo too"
sayfası, HTTP 403, 429 veya 202) ve çareyle birlikte sade bir dille söyler: "ücretsiz arama
motorları bu bilgisayarı robot sayıp reddetti". Evdeki bir bilgisayarda genelde çalışır, sunucuda
genelde çalışmaz. Çare, programların kullanması için yapılmış bir arama sağlayıcısıdır; bkz.
[Sunucuda arama](#sunucuda-arama).

Arka uçları, öncelik sırasını ve API anahtarlarını ayarlayabilir, canlı bir deneme de
yapabilirsin. Bunu masaüstü uygulamasında **Ayarlar → Bağlantılar → E-posta ve web** altından ya da
`<data_dir>/web.toml` dosyasından yaparsın. Arka uçlar ayarlı öncelik sırasıyla denenir ve sonuç
döndüren ilki kazanır (varsayılan sıra):

1. Kendi çalıştırdığın bir [SearXNG](https://docs.searxng.org/) örneği (`searxng_url`),
2. Brave Search API (`brave_key`),
3. Tavily Search API (`tavily_key`),
4. DuckDuckGo HTML (`duckduckgo`, varsayılan `true`),
5. DuckDuckGo Lite (`duckduckgo_lite`, varsayılan `true`),
6. Bing HTML (`bing`, varsayılan `true`).

```toml
[search]
searxng_url = "http://127.0.0.1:8080"   # your own instance; it may be local/private
brave_key = "web/brave"                 # NAME of a secret, never the key itself
tavily_key = "web/tavily"               # NAME of a secret, never the key itself
duckduckgo = true                       # keyless fallback (default true)
duckduckgo_lite = true                  # keyless fallback (default true)
bing = true                             # keyless fallback (default true)
order = ["searxng", "brave", "tavily", "duckduckgo", "duckduckgo_lite", "bing"]

[http]
timeout_secs = 20            # whole request (search backends are capped at 8s each)
connect_timeout_secs = 8
max_bytes = 5242880          # response body cap (at most 64 MiB)
max_redirects = 5            # at most 10
```

Her anahtar isteğe bağlıdır. Brave ya da Tavily anahtarlarını gizli bilgi deposuna kaydet (ya da
**Ayarlar → Bağlantılar → E-posta ve web** altına gir, orası anahtarı yalnızca yazılır biçimde saklar). Sonra
`web.toml` içinde gizli bilginin adını göster:

```sh
bot secrets set web/brave
bot secrets set web/tavily
```

Bozuk bir `web.toml` (bir yazım hatası, geçersiz bir değer) daemonu asla durdurmaz. Bir uyarı
günlüğe yazılır ve varsayılanlar kullanılır. Bilinmeyen anahtarlar yazım hatası sayılır, böylece
hatalar sessizce göz ardı edilmez.

Büyük sayfalar ve akışlar, modelin okuyabileceği bir boyuta kısaltılır ve metnin tamamı
`<data_dir>/spill/` altına kaydedilir. Bu dosyalar henüz kendiliğinden temizlenmez. Birikirlerse
eski olanları elle sil.

## Sunucuda arama

Bir arama sağlayıcısı seç, bir kez. Bunlardan herhangi biri sunucuda `web_search` aracını çalıştırır:

| Sağlayıcı | Ne gerekir | Notlar |
| --- | --- | --- |
| Brave Search API | Brave Search API hesabından bir API anahtarı | Barındırılan hizmet. Kullanım sınırı planına göre değişir. |
| Tavily | Tavily hesabından bir API anahtarı | Barındırılan hizmet. Yapay zekâ ajanları için yapılmış. Kullanım sınırı planına göre değişir. |
| SearXNG | Bir SearXNG sunucusunun adresi | Kendi sunucunda çalışır, hesap ya da arama başına ücret yok. Tek komutla kurulum aşağıda. |

**Uygulamada:** **Ayarlar → Bağlantılar → E-posta ve web** sayfasını aç, sağlayıcıyı seç, anahtarı (ya da SearXNG
adresini) yapıştır ve **Kaydet ve dene** düğmesine bas. Deneme, o sağlayıcı üzerinden gerçek bir
arama yapar ve cevap verip vermediğini söyler. Anahtarlar gizli depoda yalnızca yazılabilir olarak
saklanır, bir daha gösterilmez.

**Uyarıyı nerede görürsün:** Bilgisayarın ekranı yoksa ve hiçbir sağlayıcı ayarlı değilse Web
araması sayfası bir uyarı gösterir, `bot doctor` `web search` satırında uyarı verir ve arka plan
hizmeti başlarken aynı uyarıyı günlüğe yazar. `bot doctor --online` ve **Ara** düğmesi gerçek bir
arama yapar; ücretsiz motorların hepsi reddederse çıplak bir HTTP kodu yerine "bu bilgisayarı robot
sayıp reddetti" derler.

### Sunucuya SearXNG kur (tek komut)

Sunucuda Docker olmalı. Sunucuda, bu deponun bir kopyasından:

```sh
sudo bash scripts/install-searxng.sh --allow-latest
```

`--allow-latest` olmadan (ya da aşağıdaki gibi özetle sabitlenmiş bir `--image` olmadan) betik hiçbir
şey indirmeden durur: kapsayıcı root olarak başlar, bu yüzden sen açıkça söylemedikçe altından
değişebilecek bir imajı çalıştırmaz. Komut `/opt/praxi/searxng/settings.yml` dosyasını yazar (JSON cevabı açık, rastgele bir gizli
anahtar), yalnızca `127.0.0.1:8080` üzerinde dinleyen ve sunucuyla birlikte yeniden başlayan
`praxi-searxng` adlı bir kapsayıcı başlatır, ona gerçek bir arama sorar ve Praxi'ye verilecek adresi
yazar: `http://127.0.0.1:8080`. Bunu **Ayarlar → Bağlantılar → E-posta ve web → SearXNG** alanına yapıştır ya da
`web.toml` içine (`searxng_url`) yazıp Praxi'yi yeniden başlat.

- `--port <n>` başka bir port, `--dir <yol>` başka bir ayar klasörü, `--image <ref>` çalışacak
  imajı seçer. Yalnızca resmi `docker.io/searxng/searxng` kabul edilir ve özetle sabitlenmelidir
  (`--image docker.io/searxng/searxng@sha256:<64 onaltılık hane>`; güvendiğin bir özeti imajın Docker
  Hub etiket sayfasından seç). Alternatifi `--allow-latest`: değişebilen bir etiketi (`latest` ya da
  `--image docker.io/searxng/searxng:<etiket>` ile verdiğin) kabul eder, bir kez indirir, aldığı özeti
  yazar ve tam o özeti çalıştırır; bir dahaki sefere sabitlemek için komutu da gösterir.
- İstediğin zaman yeniden çalıştırabilirsin: kapsayıcıyı değiştirir, `settings.yml` dosyanı korur.
- `sudo bash scripts/install-searxng.sh --uninstall` kapsayıcıyı kaldırır, ayar klasörünü korur.
- Docker kurmaz. `docker` yoksa betik durur ve bunu söyler.
- SearXNG, sorgularını kullandığı motorlara sunucunun adresinden iletir. O motorlar da sunucunu
  reddederse `settings.yml` içindeki motor listesini düzenle (bkz.
  [SearXNG belgeleri](https://docs.searxng.org/)).
- Praxi yalnızca senin kendi ayarladığın tek SearXNG adresi için özel bir adrese (kendi bilgisayarın
  ya da yerel ağın) erişebilir; adres `http` ya da `https` olmalı ve içinde kullanıcı adı veya parola
  bulunmamalı. O sunucunun verdiği her yönlendirme, başka herhangi bir adres gibi yeniden denetlenir:
  başka bir özel adrese, başka bir porta, bulut meta verisine ya da link-local adrese ya da Praxi'nin
  kendi API portuna yönlendirme reddedilir.

## Gizlilik

Arama sorguları, onları işleyen ayarlı arka uca gider (kendi SearXNG örneğin, Brave, Tavily,
DuckDuckGo ya da Bing). Çekilen URL'ler ise işaret ettikleri sitelere, makinenin IP adresinden
gider. Sayfa çekimleri (`web_extract`, `web_feed`, `web_sitemap`, `web_metadata`)
`User-Agent: Praxi-Bot/<version> (web-tools)` başlığını taşır. HTML arama istekleri ise arama
motorları bot doğrulaması göstermesin diye standart tarayıcı `User-Agent` / `Accept` başlıklarını
kullanır. Sorgu gizliliği senin için önemliyse `searxng_url` adresini kendi denetlediğin bir
örneğe yönelt ya da `web_*` araçlarını botun `allow` listesinden çıkar.

## Güvenlik

- **Ağ sınırları.** Araçlar yalnızca `http` ve `https` URL'lerini çeker. Loopback (`127.0.0.1`,
  `localhost`), özel ağlar, bağlantı-yerel (link-local) ve bulut meta veri adresleri
  (`169.254.169.254`) ile daemonun kendi API'si reddedilir. Bu denetim her yönlendirmede ve bir
  ana makine adının çözüldüğü adreslerde yinelenir. Böylece herkese açık bir sayfa botu senin
  ağına yönlendiremez. Özel olmasına izin verilen tek adres, senin kendi ayarladığın SearXNG
  adresidir (o makine ve port; link-local ve meta veri adresleri hiçbir zaman); onun
  yönlendirmeleri de aynı denetimlerden geçer.
- **Güvenilmeyen içerik.** Bir sayfanın söylediği her şey veridir. Her sonuç “güvenilmeyen dış
  içerik” uyarısıyla başlar ve bir bot, sayfanın içindeki yönergeleri senden geliyormuş gibi
  ele almamalıdır.
- **Onaylar.** Araçlar, veriyi nereye gönderdiklerine göre ayrılır:
  - `web_search` yalnızca ayarladığın arama sağlayıcısıyla konuşur. Bu yüzden, çalışma özel veri
    okuyana kadar, diğer salt okunur araçlar gibi sormadan çalışır. Yalnızca güvenilmeyen içerikten
    sonra (bir sayfa, bir akış) sonraki bir arama yine sormaz. Çalışma bir kez kirlendikten sonra
    kalıcı bir izin, botun “Bu bot bir şey okuduktan sonra veri göndermesine izin ver” ayarı açık
    olmadıkça bir çekmeyi muaf tutmaz.
  - `web_extract`, `web_feed`, `web_sitemap` ve `web_metadata`, modelin seçtiği her adrese
    ulaşır. Bu yüzden **varsayılan politikada her çağrıda onay ister**, tıpkı `browser_open` gibi.
    Çalışma güvenilmeyen ya da özel içerik okuduktan sonra, o bot ayarı açık olmadıkça kalıcı bir
    izin ya da `full` hazır ayarı altında bile sorarlar.
  - `web_scrape` (yalnızca isteğe bağlı eklenti kuruluysa) `web_extract` gibi ele alınır: modelin
    seçtiği her adrese ulaşır, bu yüzden varsayılan politikada ve bir okumadan sonra her çağrıda
    onay ister.
  - Hepsi `browser` izin kategorisine aittir: bir bot için `browser` ayarını `deny` yaparsan
    hepsi reddedilir.
  - **Full** hazır ayarında, kendi başlattığın bir çalışma webi kartsız okur, `web_scrape` dahil:
    `web_extract` ve `browser_open` ile aynı kuralı izler. Adres ya da sorgu çalışmanın özel
    verisini kopyalıyorsa, bir anahtar ya da kart numarası taşıyorsa ya da bu bilgisayarı ya da
    ağını gösteriyorsa yine sorar. Para her zaman sorar. Bkz.
    [Sohbet ve onaylar](chatting-and-approvals.md).

### Bir botun sormadan çekmesine izin vermek

Bir bota yeterince güveniyorsan, bir çekmeyi “her zaman” seçeneğiyle onayla (bkz.
[Sohbet ve onaylar](chatting-and-approvals.md) içindeki `always` kapsamı). Bu, `web_extract` aracını
(ya da onayladığın aracı) yalnızca o bot için hatırlar. Çalışma henüz hiçbir şey okumadığı sürece
sonraki çağrılar sormadan çalışır. Bir sayfa ya da özel bir okuma, modelin gördüğü sohbette hâlâ
duruyorsa sonraki bir çekme yine sorar. Her çekme aracına ayrı ayrı izin ver. `always_category`
diğer tüm `browser` araçlarını da kapsar, bu yüzden araç başına seçimi tercih et. İzinleri o
kılavuzda anlatıldığı gibi gözden geçirebilir ve kaldırabilirsin. Bir okumadan sonra sormayı
atlamanın tek yolu, bot başına “Bu bot bir şey okuduktan sonra veri göndermesine izin ver”
ayarıdır.

## Bilinen risk

İzin verdiğin bir çekme ya da `full` bir bot, çalışma bir şey okuyana kadar yine veri
gönderebilir. O okumadan sonra, okuma sohbette durduğu sürece, bot başına ayarı açmadıysan çıkış
kapısı sonraki turlarda da sorar (bkz. [Güvenlik modeli](security.md)). URL ve sorgu kartta
eksiksiz gösterilir. URL'ler en çok 2.048, arama sorguları en çok 400 karakterle sınırlıdır,
yerel ve özel adresler reddedilir ve sonuçlar güvenilmeyen olarak etiketlenir. Bu ayarı, gizli
bilgilerle uğraşan bir bot için açma.

## Çalıştığını kontrol et

```sh
bot doctor --online
```

`web search` satırı hangi arka uçların ayarlı olduğunu ve canlı bir aramanın yanıt verip
vermediğini gösterir. Yanıt vermeyen bir arama bir hata değil, uyarıdır.

## Tarayıcı kullanımı: ne beklemeli

Botun kendi tarayıcısı (`browser_open`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_tabs` ve diğerleri), ilk çağrıda başlayan yönetilen bir Chrome ya da Edge'dir.

- **Nerede çalışır.** Bilgisayarında gerçek bir pencere açar, böylece izleyebilirsin. Ekranı olmayan, hep açık bir sunucuda ekransız (headless) çalışır ve sohbetin canlı görünümünden izlersin.
- **Yavaş sayfalar.** 20 saniye sonra hâlâ yüklenmemiş bir sayfa artık bir hata sayılmaz: bot, ekranda olanı sayfanın hâlâ yüklendiği notuyla birlikte alır ve körü körüne beklemek yerine devam eder.
- **İndirmeler engellenir.** Bir sayfa, botun tarayıcısı aracılığıyla diskine asla dosya koyamaz. İndirme iptal edilir.
- **Tarayıcı araçları yok mu?** Bota yönergelerinde nedeni tam olarak söylenir (bilgisayar kapalı, erişilemiyor ya da Chrome veya Edge kurulu değil) ve bot tahmin yürütmek yerine bunu söyler. Chrome ya da Edge kur ya da sohbet çubuğundaki “Çalıştığı yer” seçimini değiştir.
- **Bota öğretilen çalışma yöntemi.** Bot sayfayı açar (sonuç, tıklanabilir öğeleri `ref` değerleriyle zaten listeler), bu `ref` değerleri üzerinden işlem yapar, önce çerez bildirimlerini kapatır, açılır pencereler için sekme listesini kullanır ve ekran görüntüsünü yalnızca bir sayfanın nasıl göründüğüne dair sorularda alır.
- Onaylar değişmez: herkese açık http(s) sayfaları ek onay istemez. `file:` ve yerel adresler sorar.
