# Uygulamaları bağlama (Uygulamalar penceresi)

Masaüstü uygulamasındaki **Uygulamalar** penceresi, Praxi'yi dış servislere
(Linear, Notion, Stripe, Atlassian, GitLab ya da Composio üzerinden yüzlerce uygulama gibi)
ve yerel ya da özel [MCP sunucularına](mcp-servers.md) bağlar.

Pencereyi sol çubuğun altındaki **Uygulamalar** düğmesinden açabilirsin. Çubuk genişken
bu düğmede bağlı MCP sunucularının en çok beş logosu, darken bir ızgara simgesi görünür.
Komut paletinden de (`Ctrl/Cmd+K`) açılır.

Pencerenin üstünde şunlar bulunur:

- **Uygulamalarda ara**: bağlı uygulamaları ve keşif listesini ada ya da açıklamaya göre süzer.
- **Gelişmiş**: ana Uygulamalar görünümü ile gelişmiş görünüm arasında geçiş yapar. Gelişmiş
  görünümde özel MCP sunucuları, yerel geliştirici sunucuları, anahtarla çalışan servisler ve
  Composio hesap ayarları bulunur.

## Uygulamalar üç yolla bağlanır

> **Google (Gmail, Drive, Takvim), kendi Google kurulumunla:** **Gelişmiş** altında, kendi
> oluşturduğun bir istemciyle Google'ın kendi MCP sunucularını bağlayan yönlendirmeli bir kurulum
> vardır. Bkz. [Google Workspace](google-workspace.md). Ana listedeki tek tıklık Gmail, Drive ve
> Takvim kutucukları Composio kullanır ve kurulum gerektirmez.

### 1. Doğrudan OAuth uygulamaları

Yerleşik katalogdaki, API anahtarı istemeyen uzak HTTP MCP sunucuları ana ekranda doğrudan
**Keşfet** altında görünür (örneğin **Atlassian**, **GitLab**, **Linear**, **Notion** ve **Stripe**).

1. Uygulama kutusunda **Bağlan**'a tıkla.
2. Sistem tarayıcın, servisin giriş ve izin sayfasını açar.
3. Tarayıcıda girişi bitir. Daemon yanıtı aldığında uygulama **Bağlı** bölümüne geçer.

Doğrudan OAuth uygulamaları, arada hiçbir aracı olmadan servisin kendi uzak MCP sunucusuyla
konuşur. Bir servis hem doğrudan hem Composio üzerinden kullanılabiliyorsa Praxi yalnızca
doğrudan kutuyu gösterir. Böylece her ürün listede bir kez yer alır.

### 2. Composio uygulamaları

**Keşfet** ızgarasının geri kalanı, Composio üzerinden kullanılabilen üçüncü taraf
uygulamaları listeler. Bunları kategori etiketleriyle (**Tümü**, **İletişim**, **Sosyal**,
**Verimlilik**, **Geliştirme**, **Finans**, **İş**) ya da arama kutusuyla süzebilirsin.
Daha fazla sayfa yüklemek için **Daha fazla göster**'e bas.

**Bağlan**'a tıklayınca kutunun yanında küçük bir ilerleme penceresi açılır:

- **İlk kez (Adım 1 / 2):** Praxi, bir kerelik ücretsiz Composio girişini tarayıcında açar.
  Bu bitince akış kendiliğinden uygulamanın kendi izin sayfasına geçer (**Adım 2 / 2**).
  **Bağlan**'a yeniden basman gerekmez.
- **Zaten bağlıysan (Adım 1 / 1):** Praxi uygulamanın izin sayfasını hemen açar.

> **Gizlilik notu:** Composio üzerinden çalışan uygulamalarda araç girdileri ve sonuçları
> Composio sunucularından geçer. Praxi'nin izin sistemi, veri değiştiren ya da gönderen
> işlemlerden önce yine sana sorar (bkz. [Sohbet ve onaylar](chatting-and-approvals.md)).

Tarayıcıyla giriş yapmak yerine kendi Composio proje anahtarını kullanmak istersen
**Gelişmiş → Composio → Kendi proje anahtarımı kullan** yolunu izle. Anahtar hemen
denetlenir, işletim sisteminin anahtarlığında saklanır ve bir daha hiç gösterilmez.

### 3. Gelişmiş: MCP sunucuları ve anahtarla çalışan servisler

Geliştiriciye yönelik ve kendin yönettiğin bağlantılar için üst çubukta **Gelişmiş**'e tıkla:

- **Özel MCP sunucusu:** Elle bir `Stdio` (yerel bir komut, istersen `KEY=value` biçiminde
  ortam değişkeni satırları) ya da `HTTP` (sunucu URL'si) MCP sunucusu ekle ve hangi botların
  kullanabileceğini seç. Altta yatan `mcp.toml` biçimi ve güvenlik ayarları için
  [MCP sunucuları](mcp-servers.md) sayfasına bak.
- **Yerel geliştirici sunucuları:** Yerleşik katalogdaki, API anahtarı istemeyen yerel stdio
  sunucuları (**Filesystem**, **Playwright**, **Fetch**, **Context7**, **SQLite** ve
  **Google Drive** gibi) ile daha önce eklediğin yerel sunucular. Klasör isteyen sunucular
  (**Filesystem** gibi) tek bir **Botun okuyup yazabileceği klasör** alanını hemen orada sorar.
  Praxi kendi veri ve kimlik bilgisi klasörlerini kabul etmez.
- **Erişim anahtarı isteyen hizmetler:** Kişisel bir API anahtarı ya da token ile giriş yapan
  katalog sunucuları (**GitHub**, **Brave Search**, **Sentry**, **Todoist**, **Obsidian**,
  **Home Assistant**, **Exa**, **Tavily**, **Firecrawl**, **PostgreSQL**, **Slack** ve yerel
  **Notion** gibi). **Bağlan**'a tıklayınca satırın altında parola alanları açılır. Varsa bir
  **API anahtarı al** bağlantısı da görünür. Gizli bilgiler işletim sisteminin anahtarlığına
  kaydedilir ve bir daha hiç gösterilmez.

## “Bağlı” ne demek

Ana ekranın üstündeki **Bağlı** bölümü yalnızca **tamamlanmış** uzak bağlantıları gösterir:

- **Doğrudan OAuth / uzak HTTP MCP sunucuları**, giriş bittikten sonra (ya da zaten
  `mcp.toml` içinde kayıtlıysa) burada görünür. Yepyeni bir OAuth girişi sürerken uygulama
  **Bağlı** bölümüne geçmez. **Keşfet** içinde **İptal** düğmeli bir **Giriş yapılıyor**
  kutusu olarak kalır. Girişi iptal edersen ya da **10 dakika** içinde bitirmezsen Praxi
  yarım kalan sunucu kaydını kendiliğinden siler.
- **Composio uygulamaları**, durumları **Bağlı** (`active`) ya da **Giriş süresi doldu**
  (`expired`) olduğunda burada görünür. Bitmemiş bir Composio girişi (**Giriş yapılıyor** ya da
  **Tamamlanmadı**), Composio hesabın etkin olduğunu doğrulayana kadar **İptal** ya da
  **Yeniden dene** düğmesiyle **Keşfet** içinde kalır.

Yerel stdio sunucuları ana **Bağlı** bölümünde görünmez. Onları
**Gelişmiş → Yerel geliştirici sunucuları** altında yönetirsin.

MCP sunucularındaki durum etiketlerinin anlamı:

| Etiket | Anlamı |
| --- | --- |
| **Bağlı** | Sunucu oturumu açık ve araçları izin verilen botlar için hazır. |
| **Bağlanıyor** | Daemon sunucuyu başlatıyor ya da oturumu kuruyor. |
| **Oturum açılması gerekiyor** | Uzak HTTP sunucusu, araçlar çalışmadan önce bir OAuth girişi istiyor. |
| **Bağlı değil** | Sunucu şu an bağlı değil. **Yönet** altındaki **Yeniden bağlan**'ı kullan. |
| **Hata** | Son başlatma ya da bağlanma denemesi başarısız oldu. Ayrıntılar satırın altında görünür. |
| **Karantinada** | Sunucu üst üste başarısız oldu ve sen sorunu düzeltip **Yeniden bağlan**'a basana kadar duraklatıldı. |

## Bağlantıları yönetmek ve oturumu kapatmak

**Bağlı** bölümündeki (ya da **Gelişmiş → Yerel geliştirici sunucuları** altındaki) bir
satırın üstüne gel ya da ona odaklan, sonra **Yönet**'e tıkla. İlgi bekleyen satırlarda
(**Hata**, **Oturum açılması gerekiyor**, **Giriş süresi doldu** ya da **Karantinada** gibi) **Yönet** her zaman
görünür kalır.

### Doğrudan OAuth ve MCP sunucuları

**Yönet** içinde şunları yapabilirsin:

- **Uygulamayı hangi botların kullanabileceğini seç:** **Bu uygulamayı kimler kullanabilir**
  altında seçimini yap ve **Kaydet**'e tıkla.
- **Araçları tek tek aç ya da kapat:** **Botların kullanabileceği araçlar** altında yapılır
  (bu, `mcp.toml` içine `tool_exclude` yazar). Veri değiştiren ya da veri gönderen veya silen
  araçlar **veri değiştirir** ya da **silebilir veya gönderebilir** etiketini taşır.
  `mcp.toml` içinde özel bir `tool_allow` listesi kurduysan Praxi, araçların bu izin listesiyle
  sınırlı olduğunu belirtir.
- **Oturumu kapat** (giriş yapılmış HTTP OAuth sunucularında): saklanan OAuth tokenlarını
  daemonun gizli bilgi deposundan siler, sunucu ayarını ise korur. Sunucu, sen yeniden
  **Oturum aç**'a tıklayana kadar **Oturum açılması gerekiyor** durumuna geçer.
- **Yeniden bağlan** (**Hata**, **Bağlı değil** ya da **Karantinada** olduğunda): daemona sunucuyu
  hemen yeniden başlatmasını ya da yeniden bağlamasını söyler.
- **Bağlantıyı kes**: önce onay ister (**Bu sunucunun bağlantısı kesilsin mi?**), sonra sunucu
  ayarını `mcp.toml` dosyasından siler.

### Composio uygulamaları

Bir Composio satırının **Yönet** bölümünde şunları yapabilirsin:

- **Kapat** / **Aç** (**Bağlı** olduğunda): OAuth bağını silmeden uygulamanın araçlarını geçici
  olarak kapatır ya da yeniden açar.
- **Yeniden bağlan** / **Yeniden dene** (**Giriş süresi doldu** ya da **Tamamlanmadı**
  olduğunda): izin akışını tarayıcında yeniden açar.
- **Bağlantıyı kes**: onay ister ve uygulamanın bağlantısını keser. Uzak taraftaki kayıt
  kendiliğinden silinemezse Praxi, uygulamanın yalnızca bu bilgisayarda kaldırıldığını söyler
  ve onu Composio panelinden de kaldırmanı hatırlatır.

Composio hesabının kendisinin bağlantısını kesmek için (tarayıcı girişiyle de bağlamış olsan
proje anahtarıyla da) **Gelişmiş → Composio** bölümünü aç ve **Composio'dan çıkış yap**'a tıkla.

## Bot başına erişim

Her MCP sunucusu (doğrudan OAuth uygulamaları dahil) tüm botlarla paylaşılabilir ya da belirli
botlarla sınırlanabilir. Bağlı satırlarda en çok dört bot avatarı (daha fazla botun erişimi
varsa `+n`) ile birlikte **Tüm botlar**, **Hiçbir bot** ya da **Botlar: …** yazısı görünür.

Erişimi iki yerden değiştirebilirsin:

1. **Uygulamalar penceresinden:** sunucu satırında **Yönet**'i aç, **Tüm botlar** ya da
   **Yalnızca seçilen botlar** seçeneğini seç, sunucuyu görmesi gereken botları işaretle ve
   **Kaydet**'e tıkla.
2. **Botun ayarlarından:** botun profilini (ya da yeni bot penceresini) aç, **Uygulamalar**
   bölümüne in ve bağlı her sunucuyu aç ya da kapat. Tüm botlara açık bir uygulamanın işaretini
   kaldırırsan uygulama diğer botlarınla sınırlanır.

Birkaç önemli kural:

- **Yalnızca seçilen botlar** seçiliyken hiçbir botu işaretlemezsen (**Hiçbir bot**,
  `bots = []` olarak saklanır) **hiçbir bot** erişemez. Bu asla tüm botlar anlamına gelmez.
- Atanmamış botlar sunucunun araçlarını görmez ve onları çağıramaz.
- Panel açıkken sunucunun bot erişimi başka bir yerden değiştiyse Praxi bunu sessizce ezmeyi
  reddeder ve güncel durumu göstermek için paneli yeniler.
- Bir uygulamayı bağlamak ve bir bota erişim vermek onayları **atlamaz**. Silen ya da yazan
  araçlar yine izin ister (bkz. [Güvenlik modeli](security.md) ve
  [MCP sunucuları](mcp-servers.md)).

## Takılan ya da başarısız bir bağlantıyı düzeltmek

### OAuth girişi “Tarayıcıda oturum açılması bekleniyor…” ekranında takıldı

- **İptal edip yeniden dene:** Pencerenin üstündeki giriş kartında **İptal**'e tıkla. Kimsenin
  bitirmediği bir giriş **5 dakika** sonra sona erer ve kendiliğinden temizlenir; hiç giriş
  yapılmamış yepyeni bir uygulama yeniden kaldırılır.
- **Praxi bir sunucuda (ya da eşleştirilmiş bir ev sahibinde) çalışıyor:** Giriş sayfası yine
  senin bulunduğun bilgisayarın tarayıcısında açılır. Masaüstü uygulaması yanıtı geçici bir yerel
  portta yakalar ve eşleştirilmiş, şifreli bağlantı üzerinden sunucuya iletir; yani kopyalanacak
  bir şey yoktur. Bunun için masaüstü uygulaması gerekir ve bu bilgisayar sunucuda yönetici
  olarak onaylanmış olmalıdır ([Uzak sunucu](remote-host.md)). Uygulama yanıtı yakalayamıyorsa
  (örneğin Praxi'yi düz bir tarayıcı sekmesinde kullanıyorsan) kart bunu söyler ve tek bir kutu
  gösterir: giriş yaptıktan sonra tarayıcı açılmayan, adresi `http://127.0.0.1:` ile başlayan
  bir sayfada durur. O adresin tamamını kopyalayıp kutuya yapıştır ve **Girişi tamamla**'ya bas.
- **Tarayıcı penceresi açılmadı:** Kart bunu söyler ve **Sayfayı yeniden aç** ile
  **Bağlantıyı kopyala** seçeneklerini sunar; giriş beklemeye devam eder.
- **GitLab bir namespace hatası veriyor:** GitLab, MCP sunucusunun en üst düzey grubunda açık
  olmasını ister (grubun General ayarlarında `group[mcp_server_enabled]`).

### Composio girişi takıldı ya da katalog yenilenmiyor

- **“Giriş yapılıyor” ya da ilerleme penceresinde takıldı:** Akış **10 dakika** sonra zaman
  aşımına uğrar. Bekleyen durumu temizlemek için kutuda ya da pencerede **İptal**'e tıkla,
  sonra yeniden başlamak için **Bağlan**'a (ya da **Yeniden dene**'ye) tıkla.
- **“Uygulama listesi yenilenemedi: kayıtlı liste gösteriliyor.”:** Çevrimdışısın, Composio
  istekleri sınırlıyor ya da Composio girişinin süresi doldu. İnternete döndüğünde
  **Yeniden dene**'ye tıkla. Girişin süresi dolduysa
  **Gelişmiş → Composio → Composio'dan çıkış yap** yolunu izle ve yeniden bağlan.

### Yerel bir MCP sunucusu “Hata” ya da “Karantinada” gösteriyor

Başarısız MCP sunucuları, satırın hemen altında duruma özel bir ipucu gösterir. Yanında,
**Hata** mesajını ve **Sunucunun yazdıkları** bölümünü (`stderr`) içeren bir
**Ayrıntıları göster** düğmesi bulunur:

- **Eksik çalışma ortamı (`Node.js`, `uv`, `pipx`, `docker`):** **Node.js** ve **uv** için
  Praxi orada bir **Kur** önerisi gösterir (istersen bunları resmi sitelerinden kurup
  Praxi-Bot'u yeniden başlatabilirsin). Sonra **Yeniden bağlan**'a bas.
- **Eksik klasör (`Filesystem`):** Botun okuyup yazabileceği tek klasörün tam yolunu gir ve
  **Kaydet ve bağla**'ya tıkla.
- **Sunucu başlarken durdu:** `stderr` çıktısını okumak için **Ayrıntıları göster**'e tıkla
  (örneğin Docker kurulu ama Docker daemon çalışmıyor olabilir). Asıl sorunu düzelt ve
  **Yeniden bağlan**'a tıkla.
- **Zaman aşımı:** İlk açılış, `npx` ya da `uvx` paketleri indirirken daha uzun sürebilir.
  Biraz bekle ve **Yeniden bağlan**'a bas.
- **Eksik ya da geçersiz gizli bilgi / ayar:** Sunucu ayarını kontrol et ya da sunucuyu yeni
  bir token ile yeniden ekle, sonra **Yeniden bağlan**'a bas.
- **Karantinada:** Daemon, üst üste gelen hatalardan sonra sunucuyu duraklattı.
  **Ayrıntıları göster** içinde görünen nedeni düzelt, sonra karantinayı kaldırmak için
  **Yeniden bağlan**'a bas.
