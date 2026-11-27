# Uygulamaları bağlama (Uygulamalar penceresi)

Sol çubuktan ya da komut paletinden (`Ctrl/Cmd+K`) **Uygulamalar**'ı aç.
Ana görünümde Composio üzerinden uygulama bağlayabilirsin. Doğrudan, yerel ve
özel bağlantılar için üst çubuktaki **MCP sunucuları** düğmesini kullan.

## Composio hesabını bağla

**Composio hesabını bağla** ile tarayıcında oturum aç ya da **Proje API anahtarı
kullan** seçeneğini seç. Daemon anahtarı doğrular ve gizli bilgi deposunda
saklar. Composio kuruluş veya proje adını verdiğinde bu ad bağlantı kartında görünür.

## Uygulamayı bağla ve yönet

1. **Keşfet** bölümünden uygulamayı bul. Popüler uygulamalar başta görünür.
   Arama, kategoriler ve **Daha fazla göster** ile kataloğu gezebilirsin.
2. Uygulama sayfasını aç. Botların yapabileceği işlemleri incele ve **Bağlan**'ı seç.
3. Tarayıcıda uygulamaya izin ver. Praxi, izin tamamlanana veya süresi dolana
   kadar durumu Composio'dan denetler.

Botların eşleştirilmiş bir sunucuda çalışırken de aynı akışı kullanırsın.
Tarayıcı masaüstünde açılır, yanıtı Composio alır ve sunucu sonucu HTTPS
üzerinden sorgular.

Uygulamayı denemek, bağlantıyı kesmek veya bot erişimini ayarlamak için
**Uygulamaların** bölümünden sayfasını aç. Composio hesap adını verdiğinde veya
salt okunur bir deneme bunu bulduğunda hesap adı görünür. **Bağlantıyı dene**,
uygulama izninin geçerli olduğunu doğrular. Gmail, Google Drive, Google Takvim
ve GitHub için salt okunur denemeler hesap adını veya en çok üç örneği sana
gösterebilir. Araç girdileri ve sonuçları Composio sunucularından geçer.
Praxi'nin bot erişimi ve onay kuralları geçerlidir.

### Hangi işlem çalışmadan önce sorar

Uygulama sayfasında her işlemin yanında bir sınıf görürsün. Sınıfı Praxi,
Composio'nun işlem hakkında söylediklerinden (istek yöntemi, etiketler ve adındaki
sözcükler) belirler; botun söylediği hiçbir şey sınıfı değiştirmez. Bir işlem
yalnızca Composio onun `GET` ya da `HEAD` isteği olduğunu söylerse ya da salt
okunur diye işaretlerse **Sadece okur** olur. Praxi emin olamazsa (`POST` ya da
istek yöntemi yok) adı `list` veya `get` ile başlasa bile işlem **Değişiklik
yapar** sayılır.

| Sınıf | Ne yapar | Sorar mı? |
|---|---|---|
| **Sadece okur** | Bilgi arar (listeler, getirir, arar). | Okumaya izin veren her düzeyde kart çıkmaz. |
| **Değişiklik yapar** | Bir şey oluşturur, gönderir ya da günceller. Adı belirsiz işlemler de buraya düşer. | Botun erişim düzeyine bağlıdır: Sor ve Dengeli'de sorar, Tam erişimde sormadan çalışır. |
| **Siler** | Bir şeyi siler, kaldırır ya da iptal eder. | Botun erişim düzeyine bağlıdır, üstteki satır gibi. |
| **Para hareketi yapar** | Ödeme yapar, çeker, satın alır, abone olur, para aktarır, iade eder, fatura keser, sipariş verir, fatura bilgisini değiştirir ya da planı yükseltir. | Her düzeyde, Tam erişim dahil, her zaman sorar. Hiçbir "her zaman izin ver" bunu kapsamaz. |

Tam erişimde bir değişiklik ya da silme kart çıkmadan çalışır; bu, Tam erişimin
her yerde kapsadığı çalıştırmalar için geçerlidir (bkz. "Tam erişim" bölümü,
Sohbet ve onaylar). Tam erişim dışında, güvenilmeyen içerik (bir web sayfası, bir
e-posta) okumuş bir bot, veri gönderebilecek bir değişiklik, silme ya da
ödemeden önce ayrıca sorulur; bu denetim okumalar için uygulanmaz.
`GET` ile yalnızca bilgi arayan `list charges` gibi adlar **Sadece okur** kalır.
Paylaşan, davet eden, ileten, yanıtlayan, gönderi atan ya da rol, izin veya üye
değiştiren adlar (`get invite link`) **Değişiklik yapar** sayılır; yalnızca
`GET` üzerinden düz bir `list` ya da `search` (`list members`) **Sadece okur**
kalır.
`get and pay` gibi arama ile ödeme fiilini karıştıran adlar **Para hareketi
yapar** sayılır.

Composio kartından oturumu kapatırsan botların Composio uygulamalarına erişimi
duraklar. Uygulama ayarları ve hangi botların erişebildiği saklanır.

## MCP sunucuları

Doğrudan ve kendin yönettiğin bağlantılar için üst çubukta **MCP sunucuları**'na tıkla:

- **Uzak MCP sunucuları:** Atlassian, Linear, Notion ve Stripe gibi doğrudan OAuth
  sunucularını ekleyebilir, tarayıcıda giriş yapabilir ve mevcut bağlantıları yönetebilirsin.
- **Kendi Google kurulumunla:** Google'ın kendi MCP sunucularını (Gmail, Drive, Takvim)
  kendi oluşturduğun bir istemciyle bağlayan yönlendirmeli bir kurulum. Bkz.
  [Google Workspace](google-workspace.md). Ana listedeki Gmail, Drive ve Takvim uygulamaları
  Composio kullanır ve kurulum gerektirmez.
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

## MCP sunucusunun durumu

Yeni bir OAuth bağlantısı, izin tamamlanana kadar **Giriş yapılıyor** durumunda
kalır. İptal edersen veya beş dakika içinde bitmezse Praxi yarım kalan sunucu
kaydını siler.

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

**MCP sunucuları** içinde bağlı sunucunun satırına odaklan ve **Yönet**'i seç.

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

## Bot başına erişim

Composio uygulamalarını ve MCP sunucularını tüm botlarla paylaşabilir ya da
belirli botlarla sınırlayabilirsin. Composio için uygulama sayfasını aç,
botları seç ve **Kaydet**. Aşağıdaki denetimler MCP sunucuları içindir. Bağlı satırlarda en çok dört bot avatarı (daha fazla botun erişimi
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
  **Uygulamalar** ekranındaki Composio kartından yeniden bağlan.

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
