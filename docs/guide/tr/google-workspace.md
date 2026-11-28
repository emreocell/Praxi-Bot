# Google Workspace: kendi Google istemcinle Gmail, Drive ve Takvim

Praxi, **Google'ın kendi MCP sunucuları** üzerinden Gmail, Google Drive ve Google Takvim'e
bağlanabilir. E-postaların, dosyaların ve etkinliklerin yalnızca Google ile Praxi'nin çalıştığı
bilgisayar arasında dolaşır. Kurulum gerektirmeyen tek tıklık yol için ana [Uygulamalar](apps.md)
listesindeki Gmail, Drive ve Takvim kutucuklarını kullan (Composio üzerinden çalışırlar). Bu sayfa
diğer yolu anlatır.

**Uygulamalar → Gelişmiş → Kendi Google istemcinizle Google uygulamaları** bölümünü aç. Her
uygulama ayrı bir karttır (**Gmail**, **Google Drive**, **Google Calendar**).

## Neden Google istemcisini kendin oluşturuyorsun

Praxi'nin ortak bir Google girişi yoktur. Ortak bir giriş Google'ın uygulama doğrulamasından
(uygulamanın, gizlilik politikasının ve Gmail izinlerinin incelenmesi) geçmek zorunda olur ve herkes
bir "doğrulanmamış uygulama" uyarısı görürdü. **Kendi** Google hesabında oluşturduğun istemciyi
yalnızca sen kullanırsın; bunların hiçbiri geçerli olmaz. **Kısa cevap: kendi istemcin için Google
doğrulaması gerekmez; ortak bir istemci için gerekirdi.**

## Kurulum (yaklaşık beş dakika, bir kez)

Bir Google kartında **Bağlan**'a bas. Google Cloud konsolunun tam sayfalarını gösteren bir
rehber açılır; her düğme sayfayı tarayıcında açar.

1. **Proje oluştur.** Herhangi bir ad, örneğin `Praxi`. Varsa mevcut bir projeni kullan.
2. **Hizmetleri aç.** İstediğin uygulamaları işaretle, sonra listelenen her sayfayı açıp
   **Etkinleştir**'e bas. Her uygulamanın ikisi var: API'si (`gmail.googleapis.com`,
   `drive.googleapis.com`, `calendar-json.googleapis.com`) ve MCP hizmeti
   (`gmailmcp.googleapis.com`, `drivemcp.googleapis.com`, `calendarmcp.googleapis.com`).
3. **Marka.** Bir uygulama adı (örneğin `Praxi`) ve destek adresi olarak e-postan.
4. **Kitle.** **Harici**'yi seç (Google Workspace'in izin veriyorsa **Dahili** de olur) ve
   **Test kullanıcıları** altına kendi Google adresini ekle.
5. **Veri Erişimi.** Rehberin listelediği yalnızca okuma izinlerini ekle (**İzinleri kopyala**
   düğmesi var).
6. **İstemciyi oluştur.** Tür **Masaüstü uygulaması**. **İstemci kimliğini** ve **İstemci gizli
   anahtarını** iki kutuya kopyala ve **Kaydet ve giriş yap**'a bas.

Praxi gizli anahtarı gizli deposunda saklar (`mcp.toml` içinde değil, bir daha gösterilmez, hiçbir
bota verilmez) ve istemci kimliğini her Google uygulamasının ayarlarıyla birlikte tutar (gizli
değildir: giriş sayfasının adresinde yer alır). Diğer Google uygulamaları aynı istemciyi kullanır,
yani bir kez yazarsın. Giriş sayfası tarayıcında açılır; sonra kart **Bağlı** bölümünde görünür.

Giriş bitince Praxi senin için bir **Deneme** çalıştırır: ilk üç kaydı gösteren sabit, yalnızca
okuyan bir çağrı (son üç e-posta, son üç dosya ya da üç takvim). İstediğin zaman **Yönet → Dene:
ilk kayıtları göster** ile tekrarlayabilirsin. Yanıt yalnızca sana gösterilir; hiçbir bota verilmez.

## Önce yalnızca okuma

Praxi Google'dan yalnızca okuma izinleri ister (örneğin `gmail.readonly`, `drive.readonly`,
`calendar.events.readonly`) ve yalnızca ilgili okuma araçlarını geçirir. Bu kartlarla bir bot
e-posta gönderemez, dosya silemez ya da paylaşamaz, etkinlik değiştiremez. Okuma yine olağan onay
kurallarına bağlıdır ([Güvenlik modeli](security.md)): bir Google uygulaması etiketlerine
**güvenilmeyen** biri olarak başlar; **Yönet** altında değiştirmedikçe her eylem önce sorar.

## Botun e-posta göndermesine ya da etkinlik oluşturmasına izin verme

Yazma izni senin kararındır ve üç adımda verilir; hiçbir şey kendiliğinden genişlemez.

1. `mcp.toml` içinde (aşağıya bak) yazma aracını uygulamanın `tool_allow` listesine, ilgili izni de
   `scopes` listesine ekle (örneğin `https://www.googleapis.com/auth/gmail.send` ya da
   `https://www.googleapis.com/auth/calendar.events`).
2. **Yönet → Çıkış yap**, sonra yeniden **Oturum aç**: Google'ın sayfasında onayladığın izinler
   giriş sırasında belirlenir; bunu yapana kadar Google yazma isteğini reddeder (izinler yetersiz
   diyen bir 403).
3. Araç **Yönet** altında “hiç onaylamadığın bir araç” olarak görünür; onay düğmesine basana kadar
   kapalı kalır. Sonra olağan onay kurallarına bağlıdır: uygulamaya güvenmediysen her gönderme ya da
   yeni etkinlik yine önce sorar.

## Praxi bir sunucuda

Praxi bir sunucuda çalışıyorsa aynı şekilde giriş yap. Sayfa tarayıcında açılır, masaüstü
uygulaması yanıtı yakalayıp sunucuya iletir; bkz.
[Uygulamaları bağlama](apps.md#oauth-girişi-tarayıcıda-oturum-açılması-bekleniyor-ekranında-takıldı).
Google, **Masaüstü uygulaması** istemcisi için herhangi bir portlu geri dönüş adresini kabul eder;
sunucu için bir şey kaydetmen gerekmez.

## Neler ters gidebilir

- **"Google bu uygulamayı doğrulamadı."** Kendi istemcin için beklenen bir durum.
  **Gelişmiş**'i seçip devam et. Ona yalnızca sen giriş yapabilirsin.
- **Yaklaşık bir hafta sonra yeniden çıkış.** Uygulama **Test** durumundayken Google girişi yaklaşık
  7 gün sonra sonlandırabilir. Uygulamayı Google konsolunda yayınlamak bunu önler (yine yalnızca
  senin kalır). Kesin kural Google'a aittir ve değişebilir.
- **"Erişim engellendi" ya da yönetici mesajı.** Bir Google Workspace yöneticisi bu tür uygulamaları
  engelleyebilir. Yöneticinden izin vermesini iste.
- **Deneme Google'ın reddettiğini söylüyor.** Projende API ya da MCP hizmeti açık değil ya da
  **Veri Erişimi** altında bir izin eksik.
- **İstemci gizli anahtarını kaybettim.** Google konsolunda yenisini oluştur ve **Başka bir Google
  istemcisi kullan**'ı seç.
- **"Bu bilgisayar henüz yönetici olarak onaylanmadı."** Sunucuda giriş yapmayı yalnızca sunucunun
  yönetici masaüstü yapabilir; bkz. [Uzak sunucu](remote-host.md).

## Çıkış ve kaldırma

**Yönet → Çıkış yap**, Praxi'nin çalıştığı bilgisayardaki kayıtlı girişi siler ve Google'dan da
girişi bitirmesini ister. **Bağlantıyı kes** aynısını yapar ve uygulamayı kayıtlı girişiyle birlikte
kaldırır. Google'a bildirmek elinden geleni yapma esasına dayanır: Google'a ulaşılamazsa bile burada
çıkış yapmış olursun. Praxi'nin Google tarafındaki erişimini kendin görmek ya da kaldırmak için
Google hesabının **Güvenlik → Üçüncü taraf erişimi** bölümünde oluşturduğun istemciyi kaldır.

Tüm Google uygulamaları tek bir istemci sırrını paylaşır. **Bağlantıyı kes** adımı **Kayıtlı istemci
sırrını da sil** seçeneğini sunar; başka bir Google uygulaması sırrı kullanmıyorsa işaretli gelir ve
sır yalnızca hiçbiri kullanmıyorsa silinir. İlk girişi iptal etmek ya da zaman aşımına bırakmak,
yarım kalan uygulamayı ve sırrını aynı şekilde kaldırır. Sırrı yenilemek için Google konsolunda yeni
bir sır oluştur ve **Başka bir Google istemcisi kullan** seçeneğini kullan: saklanan sırrın yerine
geçer ve tüm Google uygulamalarına uygulanır.

Giriş kartı, giriş yaptığın sayfanın adresini gösterir. Praxi uygulamanın kendi sitesinden (Google için
`accounts.google.com`) başka bir siteye yönlendirilirse sayfayı kendiliğinden açmaz; sayfanın nerede
olduğunu söyler ve yalnızca **Yine de aç** düğmesine basınca açar.

## `mcp.toml` içinde

Kartlar olağan sunucu ayarlarını yazar; sen de elle yazabilirsin:

```toml
[[server]]
id = "google-gmail"
tool_allow = ["search_threads", "get_thread", "get_message", "list_labels"]

[server.transport]
type = "http"
url = "https://gmailmcp.googleapis.com/mcp/v1"

[server.oauth]
client_id = "123456789012-abc.apps.googleusercontent.com"
client_secret_ref = "mcp_google_client_secret"   # bir gizli bilgi ADI
issuer = "https://accounts.google.com"
scopes = ["https://www.googleapis.com/auth/gmail.readonly"]
offline_access = true                            # yenileme tokenı, oturum açık kalsın
```

Gizli bilgiyi `bot secrets set mcp_google_client_secret` ile kaydet. Bkz.
[MCP sunucuları](mcp-servers.md).
