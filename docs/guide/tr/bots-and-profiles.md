# Botlar ve profiller

**Bot**, kendi kişiliği, model seçimi, araç kümesi ve izin kuralları olan adlı bir asistandır.
İstediğin kadar bot oluşturabilirsin: örneğin yalnızca web'de gezinen dikkatli bir
"Araştırmacı" ve dosyalara dokunabilen bir "Operatör". Her botun kendi sohbetleri ve kendi
"her zaman izin ver" izinleri vardır.

Bir botu bir **profil** tanımlar: küçük bir TOML dosyası (ya da aynı yapının JSON hâli).

## Bot oluşturma

```sh
bot bots add mira.toml
# bot created: 3f1c... (Mira)
bot bots list
```

`bot bots add`, dosyayı çalışan daemon'a gönderir (`bot serve` açık olmalı). Masaüstü
uygulamasında aynı yapıyı yazan bir bot formu vardır.

## Kayıtlı tarayıcı oturumları

Bir botun **Araçlar** sekmesini aç ve **Kendiniz oturum açın**'ı seç. Botun çalıştığı
bilgisayarda ayrı, yönetilen bir tarayıcı açılır. Siteye git ve orada oturum aç, sonra
**Oturum açmayı bitirdim**'i seç. Praxi'de parola formu yoktur ve tarayıcının parola kaydetme
özelliği kapatılır. Elle oturum açarken bu botun tarayıcı araçları duraklatılır ve tarayıcısının
canlı görünümü ayrılır.

Çerezler ve site verileri, tarayıcı yeniden başlasa ve görünür/başsız (headed/headless) kip
değişse de bu botun profilinde kalır. Profiller botlar arasında asla paylaşılmaz. Oturum açılmış
sitelerde bot senin yerine iş yapabilir; mevcut onay kuralları yine geçerlidir. Dengeli kipin
anonim gezinme için kısayolu, kayıtlı hesapları kapsamaz.

Sekme, profil boyutunu ve çerezi kayıtlı alan adlarını gösterir. Bir çerez, geçerli bir oturumun
kanıtı değildir ve yalnızca yerel depolama kullanan siteler görünmeyebilir. **Bu siteyi unut**,
varsayılan profilde o alan adını ve kayıtlı alt alan adlarını temizler; **Her şeyi unut**,
adlandırılmış profiller dahil botun tüm tarayıcı profilini siler. İkisi de önce tarayıcıyı
kapatır. Verileri unutmak onayları asla değiştirmez.

Kalıcı tarayıcılar Chromium'un yerel işletim sistemi çerez korumasını korur. Windows'ta Praxi
ayrıca profil klasörünü ve mevcut dosyaları EFS ile şifrelemeyi dener; desteklenmeyen Windows
sürümleri ya da dosya sistemleri bunu reddedebilir. Sekme, klasör şifrelemesini yalnızca
doğrulayabildiğinde bildirir. Dosya sistemi şifrelemesi yoksa diğer site verileri şifresiz
kalabilir. Eski `headed/` profilleri varsayılan profile taşınmaz; iki kipte de tek bir profil
kullanmak için yeniden oturum aç. Eşleşmiş bir sunucunun tarayıcısını kullanan bir bot için
oturum açmayı o sunucuda yönet; yerel profil yönetimi, bu bilgisayardaki kullanılmayan bir
profili değiştirmek yerine reddeder.

## Eksiksiz bir profil

```toml
[bot]
id = "mira"                       # slug
name = "Mira"                     # görünen ad
persona = "You are Mira, a careful research assistant. Cite your sources."
language = "en"                   # isteğe bağlı tercih edilen dil kodu

[models]
executor   = "gemini/gemini-2.5-flash-lite"   # zorunlu: döngüyü çalıştıran model
planner    = "gemini/gemini-3.1-pro-preview"  # isteğe bağlı, şimdilik yalnızca bilgi amaçlı
classifier = "gemini/gemini-2.5-flash-lite"   # isteğe bağlı: hızlı yönlendirme modeli (K3)
fallback   = ["ollama/llama3.2"]              # isteğe bağlı: hata olursa yedek modeller
# light    = "provider/your-light-model"     # isteğe bağlı: model katmanlamayı açar

[permissions]
preset = "approve-destructive"    # read-only | read-and-draft | approve-destructive | full

[permissions.categories.shell.level]   # isteğe bağlı kategori başına geçersiz kılmalar
level = "deny"

[permissions.categories.fs.level]
level = "allow"

[tools]
allow   = ["fs_*", "browser_*"]   # boş = her araç
exclude = ["fs_delete"]           # izin verilse bile her zaman çıkarılır

[router]
enabled = true                    # ana modelden önce karar basamağını çalıştır
k3 = true                         # sınıflandırıcı modeli kullan (biri ayarlıysa varsayılan olarak açık)
```

Yalnızca `[bot]` (`id` ve `name` ile) ve `executor` içeren `[models]` zorunludur. Geri kalan her
şey isteğe bağlıdır.

### Model başvuruları

Bir model `provider/model` biçiminde yazılır; burada provider, senin ayarladığın sağlayıcının
kimliğidir (bkz. [Sağlayıcılar ve modeller](providers-and-models.md)), örneğin
`gemini/gemini-2.5-flash-lite` ya da `anthropic/<model-id>`.

`planner` alanı bilgi amaçlıdır. `classifier`, `[router]` izin verdiğinde karar basamağı
tarafından kullanılır. Döngü `executor` ile başlar ve yeniden denenebilir hatalardan ya da eksik
yeteneklerden sonra `fallback` modellerini dener. İsteğe bağlı bir `light` model, basit ilk
turlar için [model katmanlamayı](model-tiering.md) açar; tüm kurulum hazır ayarları onu
boş bırakır.

### Profilde izinler

- `bot setup` ile Genel hazır ayarıyla oluşturulan botlar `[tools] allow` içinde `mcp:*`
  alır, böylece MCP sunucularına ulaşabilirler. Bu yalnızca araçları görünür yapar: MCP
  araçları varsayılan politikayla yine onay ister. Daha önce kaydedilmiş profiller
  değişmez; aynı erişimi vermek için botun `[tools] allow` listesine `"mcp:*"` ekle. Bir
  botun hangi sunucuları kullanabileceği sunucuda (`bots`) ayarlanır, bkz. MCP sunucuları
  rehberi.
- E-posta varsayılan olarak açık değildir. `email_list_folders`, `email_search` ve
  `email_read` araçları yalnızca okur, bu yüzden sormadan çalışırlar ve `web_*` ile birlikte
  kötü niyetli bir sayfa botun posta kutunu okuyup bir web aramasına koymasını sağlayabilir. Bir
  botu bu araçlara dahil etmek için `[tools] allow` listesine `"email_*"` ekle (`email_send`
  ile göndermek yine sorar) ve o botun web araçlarını kapalı ya da "sor" seviyesinde tut.
- `preset` bir başlangıç politikası seçer. Dört hazır ayar
  [Sohbet ve onaylar](chatting-and-approvals.md) sayfasında anlatılır.
- `[permissions.categories.<name>.level]` ardından tek tek kategorileri geçersiz kılar
  (`level` sözcüğünün iki kez geçtiğine dikkat et: tablonun adı `level` ve içinde `level`
  anahtarı var). Bir seviye `deny`, `ask`, `allow` ya da `allow_with_limits` olur
  (saatlik bir üst sınır ya da yol desenleri gibi sınırlar içinde izin anlamında düşünülmüş;
  mevcut çözücüde `allow` gibi davranır).
  Kategori adları: `fs`, `shell`, `desktop`, `browser`, `accounts`, `messaging`,
  `money`, `system`, `remote`, `script` (ve her zaman izinli olan dahili `meta`).
- Bir kural, çalışmayı kimin başlattığına göre de farklı olabilir; bunun için `by_initiator`
  kullanılır (başlatan türleri: `human`, `schedule`, `routine`, `channel`, `subagent`; örneğin
  `level = "deny"` ile `[permissions.categories.fs.by_initiator.schedule]`). Zamanlanmış bir
  çalışma, `schedule` için ayrı bir kural vermedikçe `human` ayarını izler.
- Bir profilde hiç `[permissions]` yoksa **genel varsayılan politika** uygulanır (başlangıçta
  "Yıkıcı olanları onayla"). Bu politika API (`GET`/`PUT /v1/policy`) ve masaüstü uygulaması
  üzerinden değiştirilebilir.

### Araç süzgeçleri

Araç adları yerleşik adlardır, örneğin `fs_read`, `fs_write`, `shell_run`, `browser_open`,
`desktop_click`, `script_run`. `*` ve `?` joker karakterlerini kullan. Yerleşik araç grupları:

| Grup | Araçlar |
| --- | --- |
| Dosyalar | `fs_read`, `fs_list`, `fs_search`, `fs_write`, `fs_edit`, `fs_move`, `fs_delete`, `checkpoint_list`, `checkpoint_restore` |
| Kabuk | `shell_run` |
| Tarayıcı | `browser_open`, `browser_snapshot`, `browser_click`, `browser_fill`, `browser_select`, `browser_press`, `browser_scroll`, `browser_hover`, `browser_select_text`, `browser_dismiss_consent`, `browser_wait`, `browser_back`, `browser_tabs`, `browser_screenshot`, `browser_pointer`, `browser_attach`, `browser_eval` |
| Web (salt okunur, tarayıcısız) | `web_search`, `web_extract`, `web_feed`, `web_sitemap`, `web_metadata` (bkz. [Web araçları](web.md)) |
| Zamanlamalar (yalnızca Genel hazır ayar) | `schedule_list`, `schedule_create`, `schedule_update`, `schedule_pause`, `schedule_resume`, `schedule_delete` (bkz. [Zamanlanmış görevler](scheduled-tasks.md)) |
| Masaüstü (Windows) | `desktop_list_apps`, `desktop_launch`, `desktop_get_state`, `desktop_click`, `desktop_type`, `desktop_set_value`, `desktop_key`, `desktop_scroll`, `desktop_drag` |
| Betikler | `script_run` |
| Ajan içi | `ask_user`, `todo_write`, `spawn_subagent` |
| Beceriler ve iş akışları | `skill_list`, `skill_read`, `skill_write`, `skill_hint`, `workflow_list`, `workflow_run` |

### Tarayıcı nasıl hareket eder

Asistanın kendi tarayıcısı bir insan gibi hareket edebilir; böylece canlı görünümde onu
izleyebilirsin ve yalnızca imleç üzerindeyken açılan menüler açılabilir:

- `browser_hover` imleci bir öğenin üzerine götürür ve orada bekletir; sonuç, görünen şeyi
  gösterir (`browser_scroll` gibi okumaya benzer).
- `browser_dismiss_consent`, bir çerez ya da onay iletişim kutusunu, gizliliği koruyan düğmesine
  tıklayarak kapatır (reddet / yalnızca gerekli olanlar; daha dar bir seçenek sunulmuyorsa kabul
  et). Hiçbir bağlantıyı izlemez ve başka bir sitenin çerçevesinin içinde tıklamaz, `browser_click`
  gibi sorar.
- Zaten açık olan sayfanın `browser_open` ile açılması sayfayı yeniden yüklemez (yazılan girdi
  korunur); `reload: true` yeniden yüklemeyi zorlar.
- `browser_select_text` fareyle bir cümleyi, bir sözcüğü (çift tıklama) ya da bir paragrafı
  (üç kez tıklama) işaretler (girdi benzeri, `browser_click` gibi sorar ve yalnızca onayladığın
  sayfada izinlidir).
- `browser_scroll` ekran ekran (`pages`), en üste ya da en alta (`to`) gidebilir ya da bir
  öğeyi görünür yapabilir (`into_view` ile `ref`) ve sayfada şimdi nerede olduğunu söyler. Daha
  uzun bir kaydırma, kısa duraklamalı küçük adımlardan oluşur.
- Tıklamalarda imleç öğeye süzülür ve basmadan önce ve basarken kısa bir süre durur; bir alana
  yazma ise kısa bir duraklamanın ardından, onaylanan alanda hâlâ tek adımda yapılır (böylece
  bir sayfa yazılanı çalamaz).

### Adıyla tıklanacak bir şey olmadığında

Çoğu öğeye, sayfa görüntüsünün verdiği başvuruyla tıklanır (`browser_click`, `browser_fill`). Bazılarının başvurusu yoktur: başka bir sitenin çerçevesindeki bir onay kutusu ("Robot değilim" kutusu), bir tuval ya da rolü olmadan çizilmiş bir anahtar. Bunlar için asistan ekran görüntüsü alır, bir nokta seçer ve orada işlem yapar:

- `browser_screenshot` görüntüyü kaydeder, boyutunu söyler ve ekrandaki çerçeveleri ve tuvalleri kutularıyla listeler. Görüntü görebilen bir modele bir sonraki adım için görüntünün kendisi de gösterilir (aynı anda tek görüntü, yalnızca o adım için; yalnızca metin bilen bir model açıklamadan ve sayfa metninden çalışır).
- `browser_pointer`, bu görüntüdeki bir noktada tıklar, çift tıklar, sağ tıklar, imleci götürür, sürükler, fare tekerleğini çevirir, odaktaki alana yazar ya da bir tuşa basar. `browser_click` gibi sorar ve aynı denetimlerden geçer: parola, tek kullanımlık kod ya da kart alanına yazmayı reddeder; sonuç tarayıcının o noktada bulduğunu söyler ve yeni bir ekran görüntüsüyle biter. Saklanan bir gizli bilgi yine yalnızca `browser_fill` ile yazılır.
- `desktop_click` ve `desktop_drag`, masaüstü için de aynı şekilde ekran noktaları alır (`desktop_get_state` ile ekran görüntüsü, sonra incelenen pencerenin içindeki noktalar).

Hiçbir CAPTCHA çözülmez. Tek bir "Robot değilim" kutusu, her onay kutusu gibi en çok iki kez tıklanabilir; görsel, kutucuk, kaydırıcı ya da basılı tutma bulmacaları sana devredilir (`browser_handover`); sen başında değilsen çalışmaya, yeniden denemek yerine öğeyi engellenmiş diye bildirmesi ve devam etmesi söylenir.

### Bir adım başarısız olduğunda

Başarısız bir tıklama ya da doldurma sayfayı korur: hatanın ardından tarayıcının şimdi gösterdiği (öğeler ve değerleri, bir ekran görüntüsü) ve yeniden yüklemeden devam etmenin yolları gelir. Asistana aynı adresi yeniden açmaması (bu, formu boşaltır) ve yalnızca o adımı yerinde onarması söylenir; aynı adım üst üste üç kez başarısız olursa durur ve sana bildirir. Gerçekten bozulmuş bir sayfa için `browser_open` ile `reload: true` hâlâ vardır.

### Zaten açık olan tarayıcıyı kullanmak

Asistana "bu tarayıcıyı kullan" dersen kendi tarayıcısını başlatmak yerine `browser_attach` çağırır. **Bu bilgisayardaki**, uzaktan hata ayıklamayla başlatılmış (`chrome --remote-debugging-port=9222` ya da `chrome://inspect` içinde uzaktan hata ayıklamayı aç) bir Chrome ya da Edge'e bağlanır ve baktığın sekmeyi yönetir. Bu tarayıcı oturum açtığın hesapları taşıdığı için her bağlanma onayını ister; gösterdiği şey çalışmanın geri kalanında özel verin sayılır, yani özel veri için güvenlik kuralları geçerlidir. Başka bir makinedeki hata ayıklama portuna asla bağlanılmaz. Bağlıyken her tarayıcı sonucu kendi tarayıcından geldiğini söyler. `browser_attach` ile `detach: true` asistanın kendi tarayıcısına döner; her iki durumda da tarayıcın çalışmaya devam eder. Bağlantı onu isteyen çalışmaya aittir: sonraki çalışma (zamanlanmış görev, webhook, başka bir sohbet) onu asla yeniden kullanmaz; tarayıcı boşta kaldığı için kapanınca da biter. Açık sekmelerinin listesi adresleri sorgu dizesi olmadan gösterir. İşaretçi tıklamaları, yazma ve tuş basmaları gerçekten onları alan sayfaya ya da çerçeveye göre denetlenir; izin verilmiş bir sayfanın içindeki başka bir sitenin çerçevesi kendi sitesine göre değerlendirilir.

Sayfa okuyucu (Ayarlar > Güvenlik) ağır bir sayfayı özetleyemezse asistan sayfanın öğelerini ve adreslerini alır ve etkileşimli tarayıcıda devam eder; artık sana sormak için durmaz. Bir sitenin reddettiği `web_extract` (LinkedIn betiklere hata ya da yalnızca bir giriş duvarı döndürür) için de aynı öneri verilir: adresi `browser_open` ile aç.

Ayarın adı `human_like` ve üç değeri var: `off` (anlık, duraksama yok), `auto` (varsayılan:
doğal hareket yalnızca birinin izleyebildiği durumda, yani görünür bir tarayıcı penceresi ya da
açık bir canlı görünüm varken) ve `high` (her zaman, daha yavaş ve daha kararlı, gösteriler ve
kayıtlar için). Bunu `host.toml` içinde (`[browser]` `human_like = "auto"`) ya da
`BOT_BROWSER_HUMAN_LIKE` ile ayarla; ortam değişkeni üstün gelir.
Bu, izlenebilir, gösteriye uygun bir his içindir. Tarayıcının otomatik olduğunu gizlemez ve bot
algılamayı aşmanın bir yolu değildir.

Çok sayıda araç kayıtlıyken (40'tan fazla) model, tüm liste yerine küçük bir keşif arayüzü
(`find_tool`, `get_tool_details`, `use_tool`) görür.

## Botları yönetme

| İş | Nasıl |
| --- | --- |
| Botları listele | `bot bots list` |
| Birini, etkin politikasıyla birlikte incele | `GET /v1/bots/{id}` (API; masaüstü uygulaması) |
| Bir profili değiştir | Değiştirilen profildeki `expected_version` ile `PUT /v1/bots/{id}`; eksik ön koşul `428 precondition_required`, eski sürüm ise güncel botla birlikte `409 version_conflict` olur. Eski çağıranlar yeniden yüklemeli ve açıkça yeniden denemelidir; sunucu eski bir tam belgeyi asla kendiliğinden yükseltmez |
| Tek tek alanları güvenle değiştir | `expected_version` ve `ops` ile `PATCH /v1/bots/{id}` (`/models/planner` ya da `/permissions/categories/fs/level` gibi JSON Pointer yolları üzerinde `set` / `unset`); eski bir sürüm, güncel botu döndüren bir `409` olur. `GET` ve liste, profili ve `version` değerini tek bir depolama anlık görüntüsünde döndürür. Başarılı profil yazmaları ve beceri atama değişiklikleri aynı bot revizyonunu kendi yazma işleminde ilerletir. Düzenleme penceresi bununla kaydeder. En fazla 128 işlem, 32 yol bölümü / ortaya çıkan JSON düzeyi, 4096 yol baytı ve temkinli bir 1 MiB kaçışlı JSON bütçesi; aşırı girdi uygulanmadan önce reddedilir |
| Bir botu sil | `DELETE /v1/bots/{id}`; yalnızca hiç sohbeti ya da çalışması yokken mümkündür |
| "Her zaman izin ver" izinlerini gör ya da iptal et | `bot grants list <bot-id>`, `bot grants revoke ...` |

CLI'de botları düzenlemek ya da silmek için henüz bir komut yok; masaüstü uygulamasını ya da
HTTP API'sini kullan (bkz. `docs/api/openapi.json`).

### Ana bot ve bot sırası (masaüstü uygulaması)

Yeni bir masaüstü kurulumunda ilk çalıştırma sihirbazı varsayılan olarak beyaz hayalet
avatarlı **Praxi** adında bir bot oluşturur. Sol çubukta ilk sırada başlar ve senin **ana
botun** olur (uygulama açıldığında ya da yeni bir sohbet başladığında varsayılan bot).

Her bilgisayar kendi ayrı bot sırasını ve ana botunu tutar. **Bu bilgisayar** ve eşleşmiş
her ana makine kendi listesini masaüstü ayarlarında yerel olarak saklar; bu yüzden bilgisayar
değiştirmek ya da eşleşmiş bir ana makinede ana botu değiştirmek, başka hiçbir bilgisayardaki
ana botu değiştirmez.

Seçili bilgisayarda birden fazla botun olduğunda:

- Başka bir botu tek tıkla ana bot yapmak için sağdaki **bot panelini** aç (başlık çubuğundaki bot
  etiketine tıkla) ve panel başlığında botun adının yanındaki yıldız düğmesine
  (**Ana bot yap**) bas. Bot, o bilgisayarın listesinin en üstüne çıkar ve diğer tüm botların
  sırasını korur.
- Bot panelindeki bilgisayar kartı da güncel ana botu, durumuyla (**Çevrimiçi**,
  **Çalışıyor** ya da **Çevrimdışı**) birlikte bir yıldız simgesiyle gösterir.
- O bilgisayarın listesini yeniden sıralamak için sol çubukta bir botun tutamacını sürükleyebilirsin
  (ya da tutamağa odaklanıp Yukarı/Aşağı ok tuşlarına basabilirsin).

### Bot avatarları (masaüstü uygulaması)

Masaüstü uygulamasında (**Yeni bot**, **Botu düzenle** ya da ilk çalıştırma sihirbazı) her botun
şunlardan biri olabilir:

- Bir renk örneğiyle eşleşen **16 yordamsal vektör karakterden biri** (`pebble`, `cube`,
  `diamond`, `cloud`, `tri`, `flower`, `gem`, `drop`, `star`, `ghost`, `capsule`, `ears`,
  `gumdrop`, `heart`, `clover`, `boulder`). Katmanlı gölgelemeli, canlı gözlü (göz kırpma ve
  büyük avatarlarda imleç takibi) saten mat SVG'ler olarak çizilirler; boşta, düşünüyor,
  çalışıyor, onay bekliyor ve hata için hareket durumları vardır. İşletim sistemindeki
  **hareketi azalt** tercihi açıksa, soluma ve bakış hareketi kendiliğinden durur.
- **Yüklenmiş özel bir resim ya da GIF**.

Avatarlar masaüstü arayüzünde yalnızca görsel üst verilerdir; model çağrılarını, izinleri ya da
CLI/Telegram davranışını etkilemez.

## Alt ajanlar

Bir bot, odaklı bir alt görev için bir **alt ajan** (`spawn_subagent`) başlatabilir. Alt ajan
aynı bottur: aynı kişilik, aynı izin kuralları ve izinler, ve yalnızca ana botunkinden daha dar
olabilen bir araç kümesi. Kendisini başlatan bottan asla daha serbest olamaz.

## Planned

Tasarım belgeleri henüz profilin parçası olmayan şeyleri anlatır: çalışma zamanı seçimi (bir
botun hangi bilgisayarı denetlediği), hafıza ayarları ve bot başına bağlayıcı hesapları.

## Bir botu paylaşma: profil dosyaları ve şablon galerisi

Bir botun **paylaşılabilir profili** tek bir JSON dosyasıdır (`*.praxi-bot.json`,
`"format": "praxi-bot-profile"`, `"format_version": 1`). Adı, talimatları (kişilik), model
tercihini, araç seçimini (`allow` / `exclude`), izin seviyesini ve kategori kurallarını, botun
beklediği becerileri (yalnızca ad ve sürüm) ve isteğe bağlı olarak zamanlanmış görev fikirlerini
taşır.

**Dosyada asla bulunmayanlar:** API anahtarları ve diğer gizli bilgiler, kasa girdileri, "her zaman
izin ver" izinleri, dışarı veri gönderme muafiyeti (`allow_egress_after_reads`), hafıza, bilgi
kaynakları, sohbetler, bağlı uygulamalar ve hesaplar (`mcp:*` araçları), güvenilen ya da diğer
klasörler, CLI motor ayarları, bilgisayar hedefi, eşleşmiş cihazlar ve avatar. Dosya, alanlar
silinerek değil, izin verilen alanlar listesinden oluşturulur ve kişilik bir gizli bilgi
taşıyor gibi görünüyorsa dışa aktarma reddedilir.

- Dışa aktarma: botun ayarlarındaki **Profili dışa aktar** düğmesi ya da
  `bot profile export <BOT_ID> [--out FILE] [--schedules]`. Yalnızca sahibi.
- İçe aktarma: **Yeni bot → Dosyadan içe aktar…** ya da
  `bot profile import FILE [--level LEVEL] [--model provider/model] [--yes]`.
  İçe aktarma önce bir **önizleme** gösterir: dosyanın istediği seviyeyi ve botun alacağı
  seviyeyi, her kategoriyi, kabuk komutu çalıştırıp çalıştıramayacağını, dosyaları, masaüstünü,
  bir tarayıcıyı ya da betikleri kullanıp kullanamayacağını, araçları, beklediği becerileri,
  nelerin dışarıda bırakıldığını ve kişilik metninin tamamını. Sen onaylamadan hiçbir şey
  oluşturulmaz.
- **Bir istek değil, bir üst sınır.** Yeni botun alabileceği en yüksek izin seviyesini sen
  seçersin (varsayılan `read-only`). Dosyanın seviyesi ve her kategori ya da başlatan başına
  kural bu seviyeye indirilir, asla yükseltilmez. Bilinmeyen araçlar, uygulama bağlamaları ve
  komut satırı motorları bir notla çıkarılır; boşalmış bir araç listesi asla "her araç"
  anlamına gelmez. Dosya erişimi, tüm bilgisayara izin verene ya da klasör seçene kadar "klasör
  yok" ile başlar. Beceriler yalnızca listelenir (bir içe aktarma hiçbir şey kurmaz ya da
  güvenilir yapmaz) ve zamanlanmış görev fikirleri gösterilir ama asla oluşturulmaz.
- Katılık: 64 KiB'ı aşan dosyalar, bilinmeyen alanlar, daha yeni bir `format_version`, denetim
  ya da çift yönlü geçersiz kılma karakterleri, yol benzeri beceri adları ve gizli bilgiye
  benzeyen metin reddedilir. İçe aktarma, dosyada adı geçen hiçbir yolu asla okumaz ve hiçbir
  URL'yi getirmez.
- **Şablon galerisi.** Yeni bot penceresi yerleşik şablonlar sunar (Araştırmacı, Gelen kutusu
  ayıklama, Günlük özet, Yazılımcı, Aile yardımcısı); adları İngilizce, Türkçe ve Almancadır.
  Bunlar `crates/daemon-api/templates/` içinde aynı biçimde veri dosyalarıdır; bir test her
  şablonun ayrıştığını ve bir üst sınır altında içe aktarıldığını denetler. Aynı önizlemeden
  geçerler.

Dışa ve içe aktarma denetim kaydına yazılır (`profile.export`, `profile.import`). Eşleşmiş
cihazlar bu yollara ulaşamaz.


### Bilgisayar hedefi

Bir profilin `[runtime]` bölümü `computer = "auto"`, `"pc"`, `"host"`, `"browser"`, `"off"`,
`node:<canonical peer public key>` ya da `server:<server key>` değerini kabul eder
(varsayılan: `auto`). `local`, `pc` için eski bir takma addır.
`node:<canonical peer public key>` belirli bir eşleşmiş, masaüstü yapabilen düğümü seçer;
görünen adlar ve harf büyüklüğü farklı anahtarlar arama için asla kullanılmaz.

- **`server:<key>` (yalnızca tarayıcı)**: botu, model sağlayıcılarını ve anahtarlarını, dosya
  araçlarını, kabuk komutlarını ve onay isteklerini **bu** bilgisayarda tutar ve yalnızca
  `browser_*` araçlarını `<key>` ile belirlenen eşleşmiş, sürekli açık sunucuya yönlendirir
  (harfler, rakamlar, `-`, `_`, `.`, en fazla 128 karakter; bkz. `bot host devices` ya da
  eşleşmiş ana makine listen). Her bot, sunucuda kendi yalıtılmış tarayıcı sürecini ve profilini
  alır (aynı anda en fazla 8 açık). Masaüstü araçları (`desktop_*`) sunucuya ulaşmaz ve
  `browser_eval` (sayfa betikleri) orada kapalıdır. Güvenli tarafta kalır: `<key>` bilinmiyorsa,
  eşleşmemişse ya da çevrimdışıysa çalışmanın hiç bilgisayar aracı olmaz ve asla bu bilgisayarın
  tarayıcısına ya da başka bir sunucuya geri dönmez. Masaüstündeki Botu düzenle ekranı
  `"Eşleşmiş sunucu (yalnızca tarayıcı)"` ifadesini salt okunur gösterir; `server:<key>` değerini
  TOML profilinde ya da API ile ayarla. Bkz.
  [Eşleşmiş bir sunucuyu botun tarayıcı bilgisayarı olarak kullanmak](remote-host.md).

`pc`, `desktop` rolüne sahip bağlı bir eşi, ya da tek başına bir bilgisayarda yerel masaüstünü
seçer. Sunucu düğümlerine yalnızca açık anahtarla ulaşılır. `auto` önce bir PC'yi, sonra ana
makine masaüstünü, sonra bir tarayıcıyı tercih eder. Açık bir `host` yerel bir masaüstü ister;
ekran yoksa çalışma bir nedenle başarısız olur. Eş ağı kapalıysa ve yerel bir PC masaüstü yoksa
açık `pc` başarısız olur.

Uzak PC çalışmaları yalnızca masaüstü araçlarını sunar. Tarayıcı sürücüsü daemon bilgisayarında
çalıştığı için tarayıcı araçları ve tarayıcı iş akışı adımları kapalıdır. Daemon'daki bir
tarayıcı için `browser`, onun masaüstü ve tarayıcısı için `host` seç. Yerel PC çalışmaları yerel
tarayıcısını kullanabilir. Görünür ve başsız tarayıcı oturumlarının ayrı oturum anahtarları ve
profil klasörleri vardır, bu yüzden çerezleri ya da oturumları paylaşmazlar ve başlatma kipini
değiştiren bir çalışma diğer kipin yönetilen profilini yeniden kullanmaz.

İzinlere ve çalışmalara kaydedilen yerel bilgisayar kimliği, bu daemonun kimlik anahtarı
parmak izidir. Görünen ad "Yerel bilgisayar" olarak kalır. `local` sözcüğü bir kimlik değildir:
bir makinede `local` olarak yazılmış bir izin ya da çalışma, `bot.db` dosyasını alan başka bir
makinede geçerli olmaz.

Her çalışma `computer_target`, `computer_node` ve `computer_node_name` değerlerini kaydeder.
Yeniden başlatılan bir çalışma, profil düzenlemesinden sonra bile özgün PC anahtarını kullanır.
Bilgisayar hedefleri kaydedilmeden önceki duraklatılmış bir çalışmanın hedefi yoktur; onu
sürdürmek bu daemonun kendi bilgisayarına bağlar ve `auto` ile eşleşmiş bir PC'yi izlemez.
Kayıtlı bir PC çevrimdışıysa (ya da sağlayıcı henüz kurulmamışsa) çalışma `waiting_for_pc`
durumuyla duraklar; başka bir PC ya da ana makine masaüstü devralamaz. Kurtarılabilir bir düğüm
anahtarı olmayan eski bir PC çalışması da tahmin yürütmek yerine duraklar. Hiç PC seçmemiş
çalışmalar, ilk PC geldiğinde ona bağlanabilir. Bekleme, diğer satır güncellemelerinden bağımsız
olarak `paused_at_ms` değerinden başlayarak 24 saat sonra sona erer. `auto`, ulaşılamayan
eşleşmiş bir PC'ye sahipken ana makineye ya da tarayıcıya düştüğünde çalışma bu seçimi kaydeder.

Bilgisayar onayları hedef türünü, adını ve anahtarını içerir. Masaüstü/tarayıcı "her zaman izin
ver" ve "bu sohbet" izinleri yalnızca o somut hedefe uygulanır (`server:<key>` dahil). Eski,
kapsamsız bilgisayar izinleri görünür kalır ve hiçbir şeye yetki vermez. `runtime` alanını
atlayan API bot güncellemeleri kayıtlı değeri korur; istekteki ve profildeki çelişkili
`runtime` bildirimleri reddedilir.
