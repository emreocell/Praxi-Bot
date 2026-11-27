# MCP sunucuları

Daemon (`bot serve`) açılırken `<data dir>/mcp.toml` dosyasını yükler ve o dosyadaki etkin her
sunucuyu **çalıştırır**. Bir `stdio` sunucusu, daemonun kendi kullanıcısı olarak başlattığı bir
programdır: o kullanıcının okuyup yazabildiği her şeyi okuyup yazabilir. Bir `http` sunucusu ise
daemonun çağırdığı bir URL'dir. Uygulamaları masaüstündeki **Uygulamalar** penceresinden bağlamak
için (doğrudan OAuth uygulamaları, Composio, bot başına erişim ve oturumu kapatma) bkz.
[Uygulamaları bağlama](apps.md).

Daemon çalışırken sunucu ekleyebilir, değiştirebilir ve kaldırabilirsin. Bunun için
`/v1/mcp/servers` adresinde `POST`, `PUT` ve `DELETE` kullanılır (bkz.
[`docs/api/openapi.json`](../../api/openapi.json)). Her değişiklik uygulanmadan önce
`mcp.toml` dosyasına yazılır. `GET /v1/mcp/servers` kayıtlı ayarı döndürür: araç süzgeçlerini,
çalışma klasörünü, düz ortam değişkenlerini, başlık adlarını ve gizli bilgilerin **adlarını**.
Hiçbir gizli bilginin değerini asla döndürmez. Değerleri `POST /v1/secrets/{name}` ile kaydet
(yalnızca yazılır).

`PUT /v1/mcp/servers/{name}`, değişikliği kayıtlı sunucunun üzerine birleştirir. Boş bıraktığın
alan eski değerini korur. `bots: null` her bot demektir.

## Resmî MCP Kayıt Defteri’nden veya bir paketten içe aktar

Sunucunun çalışacağı bilgisayarın sahibi olarak **Uygulamalar → MCP
sunucuları** bölümünü aç. Eşleştirilmiş bir ana makinede yönetici yetkin de
varsa sunucuyu oraya kurabilirsin. **Resmî MCP Kayıt Defteri**’ni seç ve sunucu
adını ara. Praxi arama metnini aramayı başlattığında kayıt defterine gönderir.
Sonuçlarda yayıncının ad alanı ve denetiminin doğrulanıp doğrulanmadığı görünür.
Bu doğrulama kimin yayın yapabildiğini gösterir. Erişim vermeden önce sunucuyu
incele.

Çalıştırma seçeneğinin yanında **Kurulumu incele**’yi seç. Kartta çalıştırılacak
komutu, her argümanı ayrı ayrı veya HTTPS adresini, ayrıca sunucunun alacağı
ortam değişkenlerini ya da HTTP başlıklarını görürsün. Praxi, sürümü belirtilmiş
npm ve PyPI paketlerini ve desteklenen streamable HTTPS sunucularını kurabilir.
Başka kurucular, özel çalışma ortamı argümanları veya şablonlu değerler isteyen
seçenekler kullanılamaz. Onay verdiğinde Praxi kayıt defterinden ilgili sürümü
ve çalıştırma bilgilerini tekrar kontrol eder.

Dosyadan kurmak için **.mcpb dosyası aç**’ı seç. Yayıncı dosyanın tamamı için
bir SHA-256 değeri verdiyse **Paketi incele**’ye basmadan önce bu değeri gir.
Kart dosyanın SHA-256 değerini, imza durumunu, çalışma ortamını, manifestteki
araçları ve programın alacağı dosya veya klasör yollarını gösterir. Praxi,
pakete ekli imzayı imzalayan sertifikaya göre kontrol eder. Yayıncıya güvenip
güvenmeyeceğine sen karar verirsin. Doğrulama Windows’ta Crypt32, Linux ve
macOS’ta CMS desteği olan kurulu OpenSSL ile yapılır. Praxi geçersiz imzaları,
eşleşmeyen sağlama değerlerini, güvensiz yolları, sembolik bağlantıları ve boyut
ya da dosya sayısı sınırını aşan paketleri reddeder. Eşleştirilmiş ana makinenin
yükleme sınırı, yerel 100 MiB sınırından daha düşük olabilir.

Erişim bilgilerini inceledikten sonra **Kapalı olarak kur**’u seç. İçe aktarma
sırasında sunucu çalıştırılmaz veya uzak MCP oturumu açılmaz. Ortam değişkeni
ve başlık değerleri gizli bilgi kasasına kaydedilir. Hazır olduğunda **Yönet →
Sunucuyu etkinleştir** ile sunucuyu başlatabilirsin. Yerel program demonun
işletim sistemi izinleriyle dosyalara, cihazlara ve ağa erişebilir. Uzak sunucu,
yapılandırılan başlıkları ve araçlarına gönderilen verileri alır. Kaldırmak
için **Yönet → Bağlantıyı kes**’i seç. Praxi yapılandırmayı, açılan paket
dosyalarını ve başka bir sunucunun kullanmadığı, içe aktarmada oluşturulan
kimlik bilgilerini kaldırır.

## Bir sunucu neye dönüşür

- **İki aktarım türü:** stdin/stdout üzerinden konuşulan yerel bir program (`stdio`) ya da
  akışlı bir HTTP uç noktası (`http`).
- **Adlandırma:** `web` kimlikli bir sunucudaki `search` aracı `mcp:web/search` olarak sunulur.
- **Süzme:** her sunucu için, sunucu tarafındaki araç adı üzerinde `tool_allow` ve `tool_exclude`
  glob listeleri. Dışlama listesi, izin listesinden sonra uygulanır.
- **Bir sunucunun kendi araçlarını etiketlemesine güveniyor musun?** Her MCP aracı etiket
  taşıyabilir (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`). Bunlar
  sunucunun kendi iddialarıdır. Bu yüzden ancak o sunucuya güvendiğini, sunucuyu eklerken bir kez
  söylersen geçerli olurlar (Uygulamalar: “Bu sunucunun kendi araçlarını dürüstçe tarif ettiğine
  güveniyor musunuz?”, varsayılan **Hayır, her işlem için bana sor**). Bunu sonra sunucunun
  Yönet panelinden değiştirebilirsin. Kartta **Etiketlere güveniliyor** ya da **Hep sorar**
  görünür. `mcp.toml` içinde bu `trust_hints` ayarıdır, varsayılanı `false` ve her değişiklik
  denetim günlüğüne yazılır.
  - **Hayır (varsayılan):** sunucunun her aracı, ne iddia ederse etsin sorar.
  - **Evet:** salt okunur etiketli (ve aynı zamanda yıkıcı olmayan) bir araç sormadan çalışır.
    Geri kalan her şey yine sorar. Hiç etiketi olmayan bir araç, kendine “yıkıcı değil” diyen bir
    yazma işlemi (kartında yine “bu görev için izin ver” seçeneğini kullanabilirsin) ve yalnızca
    “idempotent” işaretli bir araç da buna dahildir. `openWorldHint` ile işaretli bir araç (web'e,
    e-postaya ya da başka dış sistemlere ulaşır) yalnızca okuyor olsa bile dışarı veri
    gönderebilen bir araç sayılır.
  - **Bir etiket sonradan değişirse**, örneğin “yıkıcı” diyen bir araç şimdi “yıkıcı değil”
    diyorsa, araç sen yeniden onaylayana kadar bekletilir, tıpkı açıklaması değişmiş bir araç gibi.
    Etiketler, sabitlenen şeylerin bir parçasıdır.
  Tam tablo: [MCP açıklama politikası](../../design/mcp-annotations.md).
- **Sonuçlar nedir.** Bir MCP sonucu güvenilmezdir. Ayrıca `public_content = true` olmadıkça
  özel bir okuma sayılır ve `local_only = true` olmadıkça her araç, veriyi makineden dışarı
  gönderebilen bir araç gibi ele alınır. Dışarı çıkış kapısı, sonradan yapılacak bir göndermenin
  sorup sormayacağına bu iki ayara bakarak karar verir. İkisi de varsayılan olarak false'tur.
- **Tarayıcı sunucusu.** `browser = true`, bu sunucuyu bir tarayıcı olarak bağladığını Praxi'ye
  söyler (Playwright ve benzerleri). Yerleşik Playwright hazır ayarı, bu ayar olmadan da bir
  tarayıcı sayılır. Sayfa içindeki araçları (gezinme, anlık görüntü, tıklama, yazma, değerlendirme
  ve diğerleri) tek başına bir web sayfası okunduktan sonra sormaz, görev özel bir şey de
  okuduktan sonra bir kez sorar. Sayfanın dışındaki kod ve dosya aktarımları her zaman sorar.
  Bunu yalnızca sen ayarlayabilirsin (yapılandırma dosyasıyla ya da yalnızca sahibine açık API ile).
  Bkz. [Güvenlik modeli, 12a maddesi](security.md).
- **Kimler kullanabilir:** `bots`, bot kimliklerinden oluşan bir listedir (botun profil kimliği
  değil, depolama UUID'si). Atlarsan ya da `null` yaparsan her bot kullanabilir. Boş bir liste
  `[]` hiçbir bot demektir, asla her bot değil. Listede olmayan bir bot, sunucunun araçlarını
  `find_tool` içinde ya da araç listesinde görmez ve ondan gelen çağrı reddedilir. Masaüstü
  uygulamasında bunu sunucu başına Uygulamalar'dan (**Botlar** düğmesi) ya da bot başına bot
  formundan (Uygulamalar kartı) ayarlarsın. Bir botu silmek kimliğini her sunucunun listesinden
  çıkarır. Boşalan bir liste `[]` olarak kalır.
- **Ortamın yalıtılması:** bir stdio sunucusu yalnızca senin listelediğin değişkenleri (ve
  işletim sisteminin gerekli küçük bir kümesini) alır. Başka hiçbir şey miras kalmaz. Programın
  kendisi yine daemon kullanıcısı olarak çalışır.
- **Kabuk aracının her zaman işaretleyeceği komutlar reddedilir.** `shell_run` için onayı
  zorunlu kılan aynı asla listesi (örneğin `rm -rf /`), süreç başlamadan önce stdio `command` ve
  `args` değerlerini reddeder. Bu, hem açılışta `mcp.toml` için hem de API ile ekleme ya da
  güncellemede geçerlidir.
- **Dayanıklılık:** artan aralıklarla yeniden bağlanır ve sürekli başarısız olan bir sunucuyu
  karantinaya alır. Kapanırken daemon her oturumu kapatır ve alt sürecin çıkmasını bekler. Stdio
  sunucuları kendi süreç grubunda (Unix) ya da iş kapanınca ölen bir Windows işinde başlatılır.
  Böylece bir `npx` ya da `uvx` sarmalayıcısı ile onun başlattığı süreç birlikte durdurulur.

## `mcp.toml`

Dosya veri klasöründe durur (`BOT_DATA_DIR` ya da Praxi için işletim sisteminin uygulama veri
klasörü). Dosya yoksa daemon onu oluşturur. Dosya ayrıştırılamazsa içindeki hiçbir sunucu
başlatılmaz ve sen düzeltip yeniden başlatana kadar daemon dosyanın üzerine yazmaz.

```toml
[[server]]
id = "files"
enabled = true
category = "fs"          # accounts (default) | fs | shell | browser | ...
trust_hints = false      # true: believe this server's readOnlyHint
public_content = false   # true: results are not treated as private reads
local_only = false       # true: tools cannot send data off this machine
browser = false          # true: you connected this server as a browser (security.md 12a)
# bots = ["bot-a"]       # omit or null: every bot
tool_allow = ["read_*"]

[server.transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem@2.2.0", "."]

[server.secret_env]
API_KEY = "mcp/files-key"   # name of a secret, never its value
```

Sunucu kimlikleri yalnızca harf, rakam, `.`, `_` ve `-` içerebilir. `category = "meta"`
reddedilir. Katalogdaki paketler tam bir sürüme sabitlenir (`name@1.2.3` ya da `name==1.2.3`). Her katalog girdisi ayrıca lisansını ve oturum açmak için gerekeni belirtir; bkz. [Her uygulamayı bağlamak için ne gerekir](app-catalog.md).
Özenle seçilmiş katalogda `@latest` ve sürümsüz, yalın bir `npx -y` / `uvx` paketi kabul edilmez.

Bir stdio `command` çalıştırılır. Onu bir kabuk tek satırlığına ya da daemon kullanıcısı olarak
bir terminalde kendin çalıştırmayacağın bir programa yöneltme.

HTTP OAuth için istemci kimliği isteğe bağlıdır (CIMD, önceden kayıtlı istemci, sonra eski
dinamik kayıt). Gizli bilgi saklayan bir istemci, bir gizli bilgi **adı** kullanır:

```toml
[[server]]
id = "remote"
[server.transport]
type = "http"
url = "https://example.com/mcp"
[server.oauth]
client_id = "registered-client"
client_secret_ref = "mcp/remote-client-secret"
# client_metadata_url = "https://your-domain.example/mcp-client.json"
# allow_loopback_http = true   # local development only; default false
```

İsteğe bağlı iki `[server.oauth]` alanı daha var: `scopes = ["..."]` (girişin istediği izinler;
boşsa sunucunun bildirdikleri) ve `offline_access = true` (yenileme tokenı da iste; Google'ın
oturumu açık tutması için gerekir). Google kartları ikisini de kullanır; bkz.
[Google Workspace](google-workspace.md).

O gizli bilgiyi `bot secrets set mcp/remote-client-secret` ile kaydet. Değerini asla
`mcp.toml` içine yazma. Erişim ve yenileme tokenları, gizli bilgi deposunda `mcp/remote/oauth`
altında durur. İstemci gizli bilgileri yeniden bağlanırken tekrar çözülür, böylece yenileme
(rotasyon) hemen geçerli olur. OAuth ayarları stdio aktarımlarında reddedilir.

### Daemon başka bir bilgisayarda çalışırken giriş

`POST /v1/mcp/servers/{name}/auth/start` isteğe bağlı bir gövde alır: `{"relay_port": 53123}`.
Giriş sayfası senin bulunduğun bilgisayarın tarayıcısında açılır; yetkilendirme sunucusu tarayıcıyı
**o** bilgisayardaki `http://127.0.0.1:53123/callback` adresine geri gönderir ve masaüstü
uygulaması verilen portta tam olarak bu yönlendirmeyi dinler. Uygulama adresi eşleştirilmiş bağlantı
üzerinden `POST /v1/mcp/servers/{name}/auth/callback` yoluna iletir. Bu kipte daemon dinleyici
açmaz, adresi yalnızca bir kez kabul eder ve güvenmeden önce adresin duyurduğu yönlendirmenin
(yanıttaki `redirect_uri`) birebir aynısı olduğunu ve `state` değerinin eşleştiğini denetler. 1024
altındaki portlar reddedilir. İki rota da yalnızca sahibe açıktır; ev sahibinde yalnızca yönetici
masaüstü bunları çağırabilir.
