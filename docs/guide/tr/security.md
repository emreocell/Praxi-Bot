# Güvenlik modeli

Bilgisayarını kullanabilen bir ajan güçlü bir şeydir, bu yüzden de cazip bir hedeftir:
bir web sayfası ya da belge, bota emir vermeye çalışan metinler taşıyabilir
(buna "prompt injection" denir). Praxi bunun olabileceğini varsayar ve modelin ulaşamayacağı
yerde, kodun içinde sert sınırlar koyar. Bu sayfa o sınırların neler olduğunu anlatır,
en az bunun kadar önemlisi, neler olmadığını da.

## Seni ne korur

**1. İzinleri model değil, kod uygular.** Her araç çağrısı, aracın kategorisine, botun
politikasına ve çalışmayı kimin başlattığına bakan bir çözücüden geçer. Model konuşarak
bunu aşamaz. Ayrıntılar ve hazır ayarlar
[Sohbet ve onaylar](chatting-and-approvals.md) sayfasında.

**2. Varsayılan ayarlar önce sorar.** Varsayılan hazır ayarla, dosyaları değiştiren,
uygulamalarda tıklayan ya da yazan, komut çalıştıran veya mesaj gönderen her şey senin
onayını ister.

**3. Hiçbir ayarın kaldıramadığı en alt sınırlar.** Para işleri her zaman sorar.
Yıkıcı kabuk komutlarından oluşan ve ayrıştırılarak denetlenen bir yasak liste
(disk biçimlendirme, bir sürücünün kökünü, ev klasörünü ya da sistem klasörlerini silme
ve benzerleri) her zaman sorar, `full` hazır ayarında bile, bir keresinde "her zaman izin
ver" demiş olsan bile. Alt ajanlar hiçbir zaman kendilerini başlatan bottan daha serbest
olamaz.

**4. Betikler ve iş akışları hiçbir şeyi atlatmaz.** `script_run` ve `workflow_run`
araçları aynı kapıdan geçerek çağırır, içlerindeki her çağrı tek tek denetlenir.

**5. Yalnızca bu bilgisayarda dinleyen daemon.** API yalnızca `127.0.0.1` adresine bağlanır.
Her isteğin `Host` başlığının loopback olup olmadığına bakılır, bir tarayıcı `Origin`
denetimi web sayfalarının API'yi çağırmasını engeller (yalnızca masaüstü uygulaması ve
`BOT_ALLOWED_ORIGINS` içinde saydığın kaynaklar geçer) ve çağrılar bir bearer token ister:
`<data dir>/api-token` içindeki 256 rastgele bit, yalnızca senin kullanıcın okuyabilir
(Unix'te `0600`, Windows'ta `icacls` ile kısıtlanır; bu başarısız olursa daemon uyarır).
İki yol token'ı atlar ve onun yerine geçerli bir HMAC imzası ister: `POST /v1/webhooks/{id}`
(zamanlanmış bir iş) ve `POST /v1/incoming-webhooks/{id}` (bir botun webhook'u). Gelen
webhook ayrıca taze bir zaman damgası ister, aynı imzayı yalnızca bir kez kabul eder ve
gövdeyi güvenilmeyen veri sayar. Bkz. [Gelen webhook'lar](incoming-webhooks.md).

**6. Gizli bilgiler dosyalarda ve günlüklerde durmaz.** API anahtarları ve Telegram token'ı
işletim sisteminin anahtarlığında ya da şifreli bir dosyada tutulur, `providers.toml`
içinde ya da komut satırında asla bulunmaz (yalnızca `--key-env` ve gizli giriş kutuları
kullanılır). Telegram'a giden çıktı, bilinen gizli bilgileri maskeleyen bir süzgeçten geçer.

**7. Denetim kaydı.** Onaylar, redler ve izin değişiklikleri veritabanında yalnızca ekleme
yapılabilen bir denetim kaydına yazılır. Bu kayıt API üzerinden düzenlenemez ya da silinemez,
veritabanı tetikleyicileri de bunu zorunlu kılar.

**8. Devralma ve durdurma.** Masaüstü uygulamasından bir botun masaüstü erişimini devralabilir
ya da duraklatabilirsin. Genel kısayol (varsayılan `Ctrl+Alt+Esc`) botu durdurur. Sen
kullanırken botun masaüstü eylemleri reddedilir.

**9. Dosyalar için geri alma.** Dosya değişiklikleri olmadan önce kontrol noktasına kaydedilir.

**10. Tarayıcının yalıtılması.** Tarayıcı araçları varsayılan olarak kendi yalıtılmış
profilini kullanır, açık oturumlarla dolu gündelik tarayıcını kullanmaz.

**11. Telemetri yok.** Projenin kararı (ADR-0020) şudur: Praxi eve haber göndermez. Veri,
ayarladığın model sağlayıcıya gider, başka hiçbir yere gitmez. Tek istisna, sen onayladığında
botun senin adına gönderdiği şeylerdir.

**12. Okumadan sonra dışarı veri göndermek sorar.** Bir çalışma modelin önüne güvenilmeyen
ya da özel bir içerik koyduysa (bir dosya, bir posta kutusu, bir sayfa, bir ekran görüntüsü,
bir MCP sonucu, uzaktaki bir aracın açıklaması, hatırlanan hafıza, bir iş akışı raporu),
veriyi makineden dışarı gönderebilen her araç sorar. Bu araç bir okuma aracı olsa da,
onun için kalıcı bir iznin olsa da fark etmez. Yazma, sürükleme, program başlatma ve tarayıcı
sekmesi değiştirme sorar. Düz tıklamalar ve kaydırmalar sormadan devam eder; yerel dosya
seçicileri bilinen bir yükleme riskidir (aşağıya bak). Posta kutusu okuma, arama ve okundu
işaretleme de sorar, çünkü bu çağrılar modelin argümanlarını başka bir makineye gönderir.
Böyle bir okumadan sonra bir beceriyi kaydetmek ya da yayımlamak da sorar, beceri de
güvenilmeyen veri olarak saklanır: sonraki çalışmalar onu yalnızca `skill_list` ya da
`skill_read` çağırdıklarında görür, otomatik istemlerin ve yönlendirici önerilerinin dışında
kalır. Yalnızca senin ayarladığın arama sağlayıcısına ulaşabilen bir arama, güvenilmeyen
içerikten sonra da sormadan çalışır, ta ki çalışma özel bir şey de okuyana kadar. Sormadan
geçen bu yol da emoji varyasyon seçicilerine, ZWJ, ZWNJ, LRM/RLM/ALM ve CR karakterlerine
izin verir. Aynı sohbetin sonraki turları, bu içeriği hâlâ taşıyan bir mesaj istemde
durduğu sürece kapalı kalır. Bu soruyu atlamanın tek yolu, bot formunda açtığın açık ve botun
kendine ait bir ayarıdır: "bu bot bir şey okuduktan sonra veri göndermesine izin ver".
Bu muafiyet yalnızca soruyu atlar. Gösterilemeyen karakterlere izin vermez: gizli etiketler,
çift yönlü geçersiz kılmalar ve yalıtımlar, görünmez işleçler, özel kullanım karakterleri
ve U+FFFC o durumda da reddedilir. Kart, giden metni olduğu gibi tam gösterir; yazdırılamayan
karakterler `<U+XXXX>` olarak açığa çıkarılır, satır ve karakter sayıları verilir, daraltılmış
her boş ya da sondaki boşluk dizisindeki boşluk ve sekme sayıları da gösterilir. 8 KB'ın
üzerinde masaüstü, onaydan önce tüm yükü açıp sonuna kadar ulaşmanı ister. CLI ve Telegram,
onay sunmadan önce metnin tamamını yazdırır. Boş bir araç izin listesi bir atlatma yolu
değildir. Reddetme kuralları ve 3. maddedeki alt sınırlar yine de üstün gelir. Hiçbir şey
okumamış bir çalışma değişmez, mevcut izinler de ona uygulanmaya devam eder. Tek bir
gönderimi onaylamak kalıcı bir izin oluşturmaz. Kart, gidecek her alıcıyı ve her alanı
listeler; buna varsayılan tarayıcı profili, arama arka ucu ve varsayılan posta hesabı da
dahildir. E-posta ekleri `email_read` kapsamındadır, ayrı bir ek aracı yoktur. Bir çağrı ek
listesi taşıdığında kart her adı ve boyutu gösterir.

**12a. Tarayıcı olarak bağladığın bir tarayıcı.** Tarayıcı olarak bağladığın bir MCP sunucusu
(yerleşik Playwright hazır ayarı ya da yapılandırmasında `browser = true` olan herhangi bir
MCP sunucusu; bu ayar `mcp.toml` içinde ya da yalnızca sahibin kullanabildiği API ile
yapılır, bir sunucu, bir sayfa ya da bir model bunu asla kendisi ayarlayamaz) o tarayıcının
içinde iş yapar. Araçları, modelin argümanlarına göre değil, sunucunun kaydettiği ada göre,
sabit bir listeden değerlendirilir:

| Böyle bir sunucunun aracı | Temiz çalışma | Yalnızca bir web sayfasından sonra | Özel veriden sonra da (dosyalar, posta, hafıza, ekranın) |
|---|---|---|---|
| Sayfa içi eylem: `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, `browser_evaluate` (sayfanın içindeki bir betik), `browser_wait_for`, `browser_tabs`, `browser_take_screenshot`, ... | seviye belirler | Full seviyesinde **kart yok**; "yıkıcı olanlarda sor" seviyesinde de yok (yıkıcı değiller) | sorar |
| Sayfanın dışındaki kod (`browser_run_code`, `browser_run_code_unsafe`, adı kod, betik, exec ya da install gibi olan her şey) | **her zaman sorar** | her zaman sorar | her zaman sorar |
| Dosya yükleme ya da indirme (`browser_file_upload`, `browser_pdf_save`, ...) | **her zaman sorar** | her zaman sorar | her zaman sorar |
| Sunucunun diğer her aracı | herhangi bir MCP aracı gibi | sorar | sorar |

**Adresler.** Bir URL açan çağrı (`browser_navigate`, `browser_tabs`, `browser_tab_new`)
yalnızca URL argümanı herkese açık bir DNS adı ya da herkese açık bir IP taşıyan, kullanıcı
adı ve parola içermeyen bir `http` ya da `https` adresiyse sormadan çalışır. Şunlar, her
çalışmada ve her seferinde "tarayıcında <adres> adresini aç" düz cümlesiyle her zaman sorar:
`file:` yolu, `about:`, `chrome:`, `data:`, `javascript:`, `view-source:`, `blob:`, `ftp:`,
`localhost`, `*.local`, `*.internal`, `*.lan`, `*.home.arpa`, tek sözcüklü bir ana makine
adı, bir loopback, özel (10/8, 172.16/12, 192.168/16), link-local, carrier-grade
(100.64/10), unique-local ya da ayrılmış başka bir IP (tek sayı olarak, onaltılık ya da
sekizlik yazılmış ya da bir IPv6 biçiminin içinde de olsa), ayrıştırılamayan bir adres,
eksik, metin olmayan ya da çok uzun bir URL. O kartta "Bu görev için izin ver" sunulmaz ve
daha önce verilmiş hiçbir izin böyle bir çağrıyı kapsamaz: adres her çağrıda yeniden
denetlenir. Birini onayladığında çalışma, özel veri okumuş sayılır; bu yüzden veri
taşıyabilecek sonraki her sayfa içi eylem sorar, bir betik de buna dahildir. Sayfanın kendi
izlediği bağlantıları ya da bir yönlendirmeyi bot değerlendirmez, orada kararı tarayıcının
adres çubuğu verir.

Bu kuraldan önce böyle bir sunucunun her çağrısı "veriyi makineden dışarı gönderme", sonucu
da özel sayılıyordu, bu yüzden bir sayfada beş dakikalık bir iş yüz karta mal olabiliyordu.
O tarayıcıda açtığın sayfa web içeriğidir, özel verin değildir: sayfa içi bir sonuç özel değil,
güvenilmeyen sayılır. Bilmen gereken sonuç şu: otomasyon tarayıcısında özel hesaplarda oturum
açtıysan ve görev başka araçlarla özel bir şey okumadıysa, talimat enjekte etmiş bir sayfa
botun bu hesapları açıp okumasını ve başka bir yere gitmesini kart çıkmadan sağlayabilir.
Özel bir adrese çözülen bir ad (DNS rebinding), çözülmeden görülemez, bu yüzden herkese açık
görünen böyle bir ada gitmek kartsız geçer; bu, yukarıdaki oturum açılmış hesaplar gibi
kabul edilen bir artık risktir. Playwright hazır ayarı kendi yalıtılmış profilini kullanır;
hazır ayar yalnızca tam katalog biçimiyle tanınır (`npx -y @playwright/mcp@<version>`),
bu yüzden `--extension`, `--cdp-endpoint` ya da bir kullanıcı veri klasörü açık `browser`
bayrağını gerektirir, bu da senin kararındır. Kabuk, betikler, masaüstünde yazma, program
başlatma, posta gönderme ve para kendi alt sınırlarını korur; tarayıcı olmayan MCP
sunucuları, harici ajanlar ve MCP sunucu kipi daha sıkı kurallarını korur.

**12aa. Bir sunucunun kendi araç etiketleri.** MCP araçları kendilerini etiketlerle
tanımlar (salt okunur, yıkıcı, idempotent, açık dünya). Praxi bunlara kendi başına asla
inanmaz: etiketi olmayan bir araç ya da güvenilir diye işaretlemediğin bir sunucunun herhangi
bir aracı sorar. Güvenilir diye işaretlediğin bir sunucuda yalnızca salt okunur araçlar
sormadan çalışır, açık dünya etiketli bir araç veriyi dışarı gönderebilen araç sayılır ve
sen onayladıktan sonra etiketi değişen bir araç yeniden onaya gönderilir. Bkz.
[MCP sunucuları](mcp-servers.md) ve [politika tablosu](../../design/mcp-annotations.md).

**12b. Onay kapsamları.** Bir MCP aracı için kart çıkması kaçınılmaz olduğunda şunları
sunabilir: **Bir kez izin ver**; **Bu görev için izin ver**, aynı sunucu ve aynı araç için
yalnızca bu çalışmanın geri kalanında (asla kalıcı olmaz, başka bir araca geçmez, alt
ajanlara verilmez ve çalışma, sen izni verdiğinde okumamış olduğu bir şeyi okur okumaz,
örneğin bir web sayfasından sonra özel veriyi, kapsamayı bırakır; sonraki çağrı yeniden
sorar); **Bu bot için her zaman izin ver (bu araç)**, yalnızca politikanın tek başına sorduğu
bir kartta sunulan, özel verinin yanında ve her zaman soran bir araç, bir ödeme ya da yasak
listedeki bir komut için asla sunulmayan kalıcı bir izin (kalıcı bir izin 12. maddeyi asla
geçersiz kılmaz); ve **Reddet**. Her onay, kapsamıyla birlikte denetim kaydına yazılır
(görev kapsamı için `RunTool`). "Bu görev için izin ver", untrusted içerikten sonra bir
program başlatmada, bir alt sınırda, her zaman soran bir araçta ya da harici bir çağıranda
sunulmaz ve daemon, onu sunmamış bir kartta bu isteği reddeder. Bir dosya, localhost ya da
yerel ağ adresini asla kapsamaz (bkz. madde 12a, Adresler); adres her çağrıda yeniden
değerlendirilir.

## Seni neden korumaz

Sınırların konusunda gerçekçi ol:

- **Model sağlayıcın istemlerini ve araç sonuçlarını görür.** Bulut sağlayıcı, botun
  okuduğu ve üzerinde çalıştığı her şeyi alır, buna dosya içerikleri ve sayfa metni de
  dahildir. Hassas şeyler için yerel bir model kullan (Ollama, LM Studio). Bkz.
  [Gemini ücretsiz katmanı](providers-and-models.md) notu.
- **Bir onay, onu ne kadar iyi okuduğun kadar değerlidir.** Bir istek zararsız görünüp yine
  de önemli olabilir. Özeti oku, emin değilsen reddet.
- **"Her zaman izin ver" ve `full` hazır ayarı kalıcı izinlerdir.** 12. maddedeki dışarı
  veri gönderme kuralını geçersiz kılmazlar: bir çalışma özel ya da güvenilmeyen içerik
  okuduktan sonra, o botun açık ayarı açık değilse, veriyi makineden göndermek yine sorar.
  Kategori kurallarını, dar `[tools]` listelerini ve riskli işler için ayrı botları tercih et.
- **Yasak liste bir güvenlik ağıdır, kum havuzu (sandbox) değildir.** Bilinen yıkıcı
  kalıpları tanır, zararlı her komutu yargılayamaz. Kabuk erişimi (`shell_run`), birine
  terminal vermek kadar tehlikelidir.
- **Onaylanan tek bir kabuk komutu daemon'a ulaşabilir.** `shell_run` senin kullanıcı
  haklarınla çalışır. Bearer token dosyasını o kullanıcı okuyabilir ve daemon API'si
  loopback üzerindedir, yani bir kabuk komutu token'ı okuyup API'yi çağırabilir (onaylar,
  devralma). Yasak liste ve dosya araçlarının korumaları çıtayı yükseltir ama bunlar metin
  denetimidir, bir sınır değildir. Kabuğun daemon kimlik bilgisinden gerçekten ayrılması
  planlanıyor.
- **Kabuk ve masaüstü eylemleri geri alınamaz.** Yalnızca dosya araçları kontrol noktası
  alır.
- **Gözetimsiz çalışmalar.** Zamanlamalar sen izlemezken çalışır. Bir onayın süresi dolunca
  dururlar, ama politikanın izin verdiği her şey olduğu gibi çalışır.
- **Windows masaüstü yükleyicisi henüz kod imzalı değil**, bu yüzden SmartScreen uyarır.
  Yalnızca kendi derlediğin ya da güvendiğin sürümleri kur.
- **Erken geliştirme aşaması.** Kod genç ve denetlenmemiş. Yüksek riskli otomasyon için
  henüz buna güvenme.

## Bir ana makinede sahip kim sayılır

Kendi bilgisayarında sahip, klavyenin başındaki kişidir (masaüstü uygulaması her açılışta
bir sahip anahtarı tutar). Sürekli açık bir ana makinede klavye yoktur, bu yüzden ana
makine eşleşmiş cihazlardan yalnızca bir türüne sahip olarak güvenir: **yönetici olarak
onayladığın bir Desktop** (`sudo praxi host invite --role desktop` bir tane oluşturur;
`bot host admin --all-desktops` daha önce eşleşmiş desktop'ları onaylar). Bu bilgi her
istekte eşleşme deposundan okunur, bu yüzden bir iptal ya da geri çekilen bir izin hemen
işler. Tek bir yerde tek bir kuraldır: yönetici bir Desktop, kısa bir yasak liste dışında
sahibe özel her yoldan geçer. Denetlenmesi gereken kısım bu yasak listedir: eşleştirme ve
yöneticiler, ana makinenin yaşam döngüsü ile durum içe aktarma ve devretme, ana makinenin
diğer bilgisayarlara bağlantıları, MCP cephesi kimlik bilgileri, doctor onarımları, Telegram
eşleştirmesi ve imzalı webhook girişi. Yaptığı her değişiklik denetim kaydına yazılır
(desktop, değişikliğin türü, yöntem, yol; gövde asla), sahip anahtarının kendisi de
bağlantı üzerinden asla gönderilmez.

Gizli bilgiler o bağlantıdan asla geri dönmez. Anahtarlar ve parolalar yalnızca yazılabilir,
sahip anahtarı ile API token'ı asla bir çıktı olmaz ve yönetici bir Desktop, kayıtlı bir
oturum bilgisinin, parolanın ya da kartın düz metnini açığa çıkaramaz
(`POST /v1/fill-secrets/reveal` yasak listededir ve işleyici ayrıca ana makinenin kendi
uygulamasındaki kişi ister), yani çalınmış bir yönetici Desktop kasayı boşaltamaz. Tek bir
yol, bir kimlik bilgisini bilerek bir kez gösterir: bir bot için gelen webhook oluşturmak
(`POST /v1/bots/{id}/incoming-webhooks`) HMAC gizli anahtarını o tek yanıtta döndürür, çünkü
webhook'u kuran kişi onu gönderen servise yapıştırmak zorundadır; bu sahibin işidir ve
denetlenir. Yönetici bir Desktop'ın hassas koleksiyonları okuması (denetim dışa aktarma,
sohbet dışa aktarma, kayıtlı gizli bilgi adları listesi, posta gönderim günlüğü, kullanım
dışa aktarma) değişiklikler gibi denetim kaydına yazılır; bir politika değişikliği de
önceki ve sonraki kaba seviyeyi kaydeder (gövdeyi asla).

Telefonlar, düğümler, diğer ana makineler ve onaylanmamış Desktop'lar asla sahip sayılmaz:
sahibe özel yollardan yalnızca bir cihazın her gün yaptığı şeyleri korurlar (onayları ve
soruları karara bağlama, ekran devralma düğmeleri, zamanlayıcılar) ve gönderdikleri bir
başlık onları sahip yapamaz.

## Daemon süreç koruması (derinlemesine savunma)

Daemon başlarken kendi sürecini kısıtlar, böylece aynı işletim sistemi kullanıcısı olarak
çalışan bir alt süreç (bir kabuk komutu, MCP sunucusu ya da kodlama CLI'si) sahip
sırrını tuttuğu belleği okuyamaz:

- **Windows:** süreç korumalı bir erişim listesi alır: sahip onu yalnızca sorgulayabilir,
  bekleyebilir ve sonlandırabilir. Bellek okuma/yazma, tutamaç çoğaltma ya da iş parçacığı
  oluşturma için açmak "erişim engellendi" ile başarısız olur.
- **Linux:** henüz etkin değil; `doctor` "skipped" gösterir. `PR_SET_DUMPABLE=0`
  `/proc/<pid>` klasörünü aynı kullanıcıdan da gizlerdi, masaüstü uygulaması gerçek
  daemon ile konuştuğunu doğrulamak için bunu kullanıyor, yani uygulama bağlanmayı
  reddederdi. Önce masaüstü denetiminin bir pid+stdin el sıkışmasına ya da `SO_PEERCRED`
  yöntemine taşınması gerekir; bu gelecekteki iştir.
- **macOS ve diğerleri:** hiçbir şey yapılmaz; `doctor` "not supported" gösterir.

Yani şimdilik bu koruma **yalnızca Windows'ta** var.

Alt süreçler normal başlar ve çalışır, kısıtlamayı devralmazlar. İşletim sistemi reddederse
daemon yine de başlar, bir `DEGRADED` uyarısı yazar ve `doctor` (`process guard`) bunu
gösterir.

Bunun **yapmadığı** şey: bir kum havuzu değildir. Aynı kullanıcıdan bir alt süreç yine de
token dosyasını okuyabilir, onunla loopback API'yi çağırabilir, yapılandırma dosyalarını
düzenleyebilir, arayüzü otomatikleştirebilir ya da koruma uygulanmadan önce var olan bir
tutamacı kullanabilir. Yalnızca bellek yolunu kapatır. Root/yönetici ve hata ayıklama
ayrıcalıklı hesaplar bundan etkilenmez.

Windows'ta **yükseltilmiş** (`SeDebugPrivilege` taşıyan yönetici token'ı ya da SYSTEM) bir
saldırgan süreç her süreç erişim listesini aşar, bu yüzden koruma ona karşı koruyamaz.
Bu nedenle daemon, listeyi uyguladıktan sonra kendini bellek okuma/yazma için açmayı dener;
bu hâlâ başarılı olursa koruma varmış gibi davranmak yerine `DEGRADED` bildirir (GitHub
Windows çalıştırıcısı gibi yükseltilmiş bir yönetici hesabında olan budur). Bu korumayı
istiyorsan daemon'u normal, yükseltilmemiş bir hesaptan çalıştır.

## İyi alışkanlıklar

1. Varsayılan `approve-destructive` hazır ayarını koru, genişletmeyi bilerek yap.
2. Her iş türü için ayrı ve dar kapsamlı bir bot oluştur (bkz. [profil](bots-and-profiles.md)
   `tools` ve `permissions` tabloları).
3. İzinleri ara sıra gözden geçir: `bot grants list <bot-id>`.
4. Zamanlamalara yalnızca zamanlanmış işi yapabilen bir bot ver.
5. Özel veriler için yerel bir model kullan.
6. `<data dir>/api-token` dosyasına bir parola gibi davran, veri klasörünü paylaşma.
7. Telegram'ı yalnızca kendi hesabınla eşleşmiş tut.

## Bir güvenlik açığını bildirmek

Lütfen sömürü ayrıntılarını herkese açık bir issue'ya yazma. Depoda henüz ayrı bir güvenlik
iletişim adresi yok (**Planned**); bakımcılara, onlara ulaşmak için kullandığın kanaldan
özel olarak yaz ve açık kapanana kadar bildirimi gizli tut.


## Canlı ekran gizliliği ve insan denetimi

Canlı görünümün pikselleri parolaları, tek kullanımlık kodları, özel belgeleri ve bildirimleri
içerebilir. Bunlar güvenilir biçimde maskelenemez. Bir masaüstü hedefi seçilen ekranın
tamamını paylaşır; bir tarayıcı hedefi işletim sistemi pencerelerini ya da tarayıcının
kendi arayüzünü değil, sayfa içeriğini paylaşır. `screen:view` ve `screen:input` izinlerini
birbirinden ayrı, güçlü ayrıcalıklar olarak gör. Bir masaüstü terminaline insanın yazdığı
girdi, bilgisayarın tam yetkisini taşır.

Canlı görünüm kareleri ve insan girdisi yükleri, yalnızca bellekte tutulan özel aktarım
hattında kalır. NDJSON/yeniden oynatmaya, sohbet eklerine, denetime, izlere, telemetriye ya da
model sağlayıcı isteklerine asla girmezler. Canlı görünümün son kare önbelleği hiçbir zaman
ekran görüntüsü araçlarının girdisi olmaz. Bir botun açıkça aldığı normal ekran görüntüsü,
kendi izni ve veri yoluyla bağımsız olarak çalışır. Ürünün yönettiği kalıcı depolama
yasaktır; işletim sisteminin takas alanı, hazırda bekletme, bellek dökümleri ve üçüncü taraf
uygulamaların kayıtları bu garantinin dışındadır.

Girdi için, kimliği doğrulanmış oturum sahibinin etkin ve kalıcı genel devralma mandalını
tutması, geçerli bir dönem (epoch) ile güncel hedef/kare geometrisine sahip olması gerekir.
Bağlantının kopması, süre dolması, iptal ve yeniden başlatma girdiyi geçersiz kılar, basılı
tuşları elinden geldiğince sıfırlar ve duraklatmayı korur. Eski bir bırakma isteği, sahibi
olan bir devralmayı bırakamaz. Devralma sahibi olmayan bir duraklatma (senin kendi
Duraklat'ın, daemon yeniden başladıktan sonra da) yerel kullanıcı tarafından normal bırakma
yoluyla kaldırılır; duraklatılmış bir devralma mandalı (yeniden başlatmadan sonra geri
yüklenen de dahil) `POST /v1/control/recover` ile temizlenir, bunun için görünüm oturumu ya da
kayıtlı hedef gerekmez. `POST /v1/control/recover`, yerel kullanıcıya ve hâlâ `screen:input`
iznini taşıyan eşleşmiş bir **ana makine yönetici desktop'ına** açıktır (bir telefona,
yönetici olmayan bir eşe ya da bir bota asla; açık bir iptal ana makinede üstün gelir). Bir
ana makine yönetici desktop'ı duraklatmayı `recover` ile temizlediğinde kendi ekran girdisi
izni korunur; yalnızca yerel kullanıcının geçersiz kılması bir eşin girdi iznini iptal eder.
Aksi hâlde devam etmek için açık sahipli bırakma ya da ayrıcalıklı kurtarma, kalıcı ve güvenli
devam notları ve başarılı sıfırlama gerekir.

Eşleşmiş bir cihazdan her zaman üstünsün. `POST /v1/control/override` (ve uygulamanın
Duraklat'ı) herhangi bir devralmayı hemen keser, basılı tuşları sıfırlar ve botu duraklatılmış
tutar; bunu yalnızca yerel kullanıcı çağırabilir. Eşleşmiş bir cihazın, o cihaz için sen
vermedikçe (`PUT /v1/view/privileges/{device}`) `screen:view` ve `screen:input` izni yoktur;
bir eşleşmeyi ya da izni iptal etmek o cihazın oturumlarını ve devralmasını birkaç saniye
içinde bitirir. `GET /v1/control/state` kimin hangi hedefi hangi durumda sürdüğünü gösterir;
devralma kimliğini ve dönemi yalnızca sahip de alır. Genel olay akışı, bir devralma
alındığında, etkin olduğunda, kesildiğinde, bırakıldığında ya da kurtarıldığında bir
`control_state_changed` bildirimi taşır (kim, hangi hedef, durum, neden; kimlik, piksel ya da
metin yok).

Sen sürerken (`human_driving`) botun gözlem araçları da reddedilir (anlık görüntüler, ekran
görüntüleri, OCR, sekme ve pencere listeleri): model, parolalar ya da tek kullanımlık kodlar
gibi yazdığın şeyleri okumamalıdır. Düz bir duraklatma okumalara izin vermeye devam eder.
Senin duraklatmanın ya da devralmanın bitmesini bekleyen bir bot eylemi en fazla 30 dakika
bekler (zamanlanmış bir onayla aynı sınır) ve sonra hâlâ kontrolün sende olduğunu bildirir;
duraklatma kendisi sürer. **Durdur**'a basmak, bir duraklatmanın ya da devralmanın arkasında
bekleyen her çalışmayı o sınırın dolmasını beklemeden hemen iptal eder.

Bir devralmanın sohbete bıraktığı devam notu, modelin saklanan geçmişten sistem mesajı olarak
aldığı tek mesajdır. Yalnızca daemonun yazdığı kendi rolü ve tam olarak tek sabit metin
biçimi vardır (müdahale türü, olay sayıları, hedef türü); başka bir metin ya da rol taşıyan
saklanmış bir mesaj asla sistem mesajı olarak iletilmez, bir durum paketi içe aktarma da
böyle satırları (ve bilinmeyen her rolü) modele asla gönderilmeyen etkisiz `imported_note`
rolüne çevirir; geçmiş yalnızca izin verilen roller listesini iletir (user, assistant, gerçek
denetim notu) ve diğer her değeri yok sayar. Daemonun kendi dosyaları (`bot.db`,
`schedules.db`, `memory.db` ile WAL/SHM/journal dosyaları, `control-latch.json`,
`policy.json`, `host.toml`, `config.toml`, `mcp.toml`, `providers.toml`, `web.toml`,
`fill-secrets.json`, Telegram durum dosyaları, `peer`, `bots`, `profiles` ve `runs`
klasörleri ve bearer token), botun dosya araçlarından okuma ve yazmaya karşı korunur;
Windows'ta yalnızca sondaki bir nokta ya da boşlukla veya bir `:stream` ekiyle ayrışan adlar
da buna dahildir. Beceriler ve iş akışları kendi sahibi olan araçları kullanır; genel dosya
araçları onları değiştiremez. Bunların adını anan bir kabuk komutu her zaman onay ister. Bu
kural yalnızca bir caydırıcıdır: onaylanan tek bir `shell_run` yine de senin olarak çalışır
ve bu dosyalara ile loopback API'ye ulaşabilir (aşağıya bak). Denetim yalnızca
aktör/hedef kimliklerini, olay türü sayılarını, sıra aralıklarını, zamanlamayı ve sabit
sonuçları saklar; denetim hatası yetkilendirmeyi reddeder ve girdiyi askıya alır.
Bkz. [canlı görünüm API'si ve entegrasyon durumu](../../api/live-view.md).


Güvenilmeyen beceriler otomatik istemlerin ve yönlendirici önerilerinin dışında kalır.
`skill_list` ve `skill_read` ile erişilebilirler; bu araçları kullanan çalışmalar okuma
maruziyetini taşır. İlgisiz taze çalışmalar temiz kalır. Temiz bir `skill_write`
üzerine yazması, yenisini başarıyla kaydettikten sonra işareti temizler.

Kullanıcılar mevcut içeriği yerelde, bir veri gönderme muafiyeti olmadan inceleyip güvenilir
işaretleyebilir: `bot skills review <name> --bot-id <id>` her dosyayı tam olarak, boşlukları
JSON biçiminde tırnaklanmış ve bir parmak iziyle yazdırır. İncelemeden sonra
`bot skills trust <name> --bot-id <id> --reviewed <fingerprint>` komutunu çalıştır.
Paylaşılan bir beceri için `--bot-id` kullanma. Değişen bir dosya parmak izini geçersiz kılar;
yeniden incele. Bu komutlar kullanıcı eylemleridir, asla model aracı değildir.


`fs_edit` özel bir okumadır, başarısız ve hiçbir şeyi değiştirmeyen düzenlemeler de dahil:
eşleşme sayıları ve belirli hataları dosya içeriğini ele verir. Bu yüzden sıradan bir düzenleme,
dosya modelin kendisi yazmış ya da metin kullanıcı yapıştırmış olsa bile çalışmayı kirletir.
Bunu daraltmak eşleşme sayısı kehanetini yeniden açardı, bu yüzden sonraki bir dışarı gönderme
sorar. Bedeli, özel bir okumayı izlemeyen bir düzenlemedir.

Bir kabuk, bir betik, bir masaüstü başlatma ve masaüstü girdisi (`desktop_type`,
`desktop_key`, `desktop_set_value`, `desktop_drag`) bir program başlatır ya da senin
adına klavyeyi sürer. Bir çalışma güvenilmeyen içeriği modelin önüne koyduktan sonra bunların
her biri, kartta tam komut ya da girdiyle, bir kez sorar. Kalıcı bir izin, okuma kısayolu, Full
hazır ayarı ve `allow_egress_after_reads` bu soruyu kapsamaz. Kabuk, betik ve başlatma her
platformda her zaman sorar.

**Ana makine durum komutu istisnası (yalnızca sürekli açık ana makine):** Kendi bilgisayarında
çalışan sürekli açık bir ana makinede (`host` ya da `browser`, asla kullanıcının bilgisayarı
değil), sabit, salt okunur bir durum komutları kümesi (`uptime`, `whoami`, `hostname`,
`uname`, `date`, `df`, `free`, `nproc`, `id`), izin verilen kısa bayrak kümesiyle ve
argümansız olarak (en fazla 3 boşlukla ayrılmış sözcük, en fazla 64 karakter; yol, URL, boru,
yönlendirme, tırnak, değişken, ikame, özel `cwd`, `sh`/`bash` dışı kabuk ya da `date -s` gibi
değer atayan bayrak yok) yalnızca komut metnine bakılarak karara bağlanır: temiz bir
çalışmayı güvenilmeyen diye kirletmez ve kirlenmiş bir çalışmada yürütücü kapısında sormaz. Bu
güvenlidir, çünkü bu komutlar kullanıcının ya da dışarıdan birinin yazdığı dosyaları okuyamaz,
serbest metin hedefleri kabul etmez, durumu değiştirmez ve veri göndermez. Bu tam biçimin
dışındaki her değişiklik, Sor kipindeki (`read-and-draft`) tüm politika düzeyli sormalar ve
alt sınırlar değişmeden kalır.

Masaüstü girdisinin tek bir istisnası vardır, o da yalnızca bilgisayarın yazdığı programı
adlandırabildiği yerde: **çalışmaya özel denetim izni**. Onaylamadan önce ne olduğunu oku:
dardır, ama **bir sezgiseldir, garanti değildir**.

- **Ne olduğu.** Yazma, tuş ya da değer atama kartında uygulama "Bu görev: <program>"
  seçeneğini sunabilir. Kart şunu söyler: Praxi bu görev için <program> içine (**tam yolu**,
  asla kısaltılmadan) yazabilir. Yol bir yüzeyde bütün olarak gösterilemiyorsa o yüzey
  görev geneli seçeneği hiç sunmaz ve çağrı bir kez sorar. Onaylamak, bu tek çalışmanın,
  o bilgisayardaki o tek programa (yürütülebilir dosyanın yolu, süreç kimliği ve süreç
  başlangıç zamanı, yani ikinci bir kopya ya da geri dönüştürülmüş bir süreç kimliği başka
  bir programdır) bu üç tür girdiyi çalışma bitene kadar göndermesine izin verir. Yalnızca
  bellekte durur: kaydedilmez, başka çalışmalarla paylaşılmaz, bir alt ajana devredilmez,
  sonraki mesaja taşınmaz, yeniden başlatmada kaybolur.
- **Neyi karşıladığı.** Bu çağrılar için hem güvenilmeyen içerik sorusunu hem de düz bir "önce
  sor" politikasını, sıradan bir araç için "bu sohbet" izninin yaptığı gibi karşılar. Bir
  ret, "her zaman sor" aracı, kullanım sınırları ve mutlak alt sınırlar (para, yasak liste)
  önce karara bağlanır ve yine de üstün gelir.
- **Tuşlar bir izin listesidir, çağrı başına tek biçim.** Enter, Tab, Esc, Backspace, Delete,
  oklar, Home, End, PageUp, PageDown; Shift ile Tab, bir ok, Home, End, PageUp ya da
  PageDown; Ctrl ile A, C, X, V, Z, Y, S ya da F. Geri kalan her şey sorar: her Alt
  birleşimi (Alt+Space ve Alt+F4 programın dışında iş yapar), F tuşları, Win/Super/Cmd,
  Ctrl+Esc, Alt+Tab, Ctrl+N/O/W ve birkaç tuşluk her liste. İzin verilen tuşlardan biri için
  başka bir programın kaydettiği genel bir kısayol hariç tutulmaz: böyle bir program
  çalıştırıyorsan görev geneli seçeneği kullanma.
- **Asla kapsanmayanlar.** `desktop_launch`, `shell_run`, `script_run` (kartları izni asla
  sunmaz ve döngü de onları araç türüne göre kapsamayı reddeder) ve `desktop_drag` (her zaman
  sorar, sürükleme başına bir kart). Tıklamalar buna hiç ihtiyaç duymadı.
- **Her gönderimden hemen önce gerçek alıcıda denetlenir (Windows).** Arka uç daha önce
  gördüğü pencereye güvenmez. Her gönderimden önce, canlı olarak ön plandaki pencereyi
  **ve odaklı öğeyi** okur; öğe başka bir sürece ait olabilir (izin verilen bir ana programın
  içindeki gömülü bir tarayıcı ya da terminal). İkisi de izin verilen program olmalı,
  yükseltilmiş olmamalı, bir parola alanı olmamalı (anlık görüntü değil, canlı durum), bir
  iletişim kutusu olmamalı, sahipli bir açılır pencere olmamalı (özel sınıflı iletişim
  kutuları dahil) ve pencere sınıfı sorgusu başarılı olmuş olmalıdır. Basılı tutulan bir
  değiştirici tuş reddeder. Yazmadan önceki odaklama tıklaması, tıklanan pencereyi ve onun üst
  düzey penceresini denetler ve yedeği yoktur. `set_value` yazmadan önce aynı şekilde denetlenir
  ve yazmaya geri düşmez. Metin, her birinden önce yeni bir denetimle kısa parçalar hâlinde
  gider. Ulaşılamayan her bilgi reddeder.
- **Her durdurma izni geri çeker.** Bir ret, başarısız ya da kısmi bir gönderim, doğrulanamayan
  bir tuş bırakma (tuşlar her çıkışta bırakılır ve tuş durumu doğrulanır) ya da bir zaman aşımı,
  çalışmanın tüm görev geneli izinlerini kaldırır: sonraki çağrı yeniden sorar. Hata,
  durmadan önce kaç karakterin yazıldığını söyler, böylece yeniden deneme bunları
  çoğaltmaz. Denetim, denetim ile tuş vuruşu arasındaki boşluğu daraltır ama kapatamaz.
- **Reddetme listesi bir sezgiseldir.** İzin, **dosya adı** bir listede olan programlar için
  sunulmaz: terminaller ve kabuklar, düzenleyiciler ve IDE'ler, Explorer, Görev Yöneticisi ve
  yönetim konsolları, Office uygulamaları (makro ana programları) ve betik ana programları,
  tarayıcılar, parola yöneticileri, kısayol başlatıcıları (PowerToys, Everything ve
  benzerleri), uzak masaüstü ve sanal makine görüntüleyicileri, paketlenmiş uygulama
  çerçeveleri ve sistem oturum açma yüzeyleri; yükseltilmiş programlar için de, pencere
  standart bir iletişim kutusu ya da sahipli bir açılır pencere olduğunda da sunulmaz. Listede
  olan bir programın yeniden adlandırılmış ya da kopyalanmış ikili dosyası **tanınmaz**, listede
  olmayan ama kendi komut satırı, eklentisi ya da makro özelliği olan bir program da diğer
  her program gibi izni alır. Liste eksiksiz olamaz. İzni, bir yabancının içine yazmasına
  izin vereceğin programlar için onayla.
- **Yine de neyi geçirdiği.** Verilen programın bu tuşlar ve metinle yaptığı her şey, kendi
  kısayolları ve komutları dahil. Kart, Praxi'nin okuduğu şeyleri programa yazabileceğini
  söyler.

İznin bulunduğu yerler:

| Bilgisayar | Güvenilmeyen içerikten sonra masaüstü girdisi |
|---|---|
| Windows (bu bilgisayar) | "Bu görev: <program>" için bir kez sorar, sonra çalışma bitene ya da bir gönderim durdurulana kadar o programda yazma, izin verilen tuşlar ve `set_value` serbesttir. |
| Windows, başka bir bilgisayar (eşleşmiş PC) | Her çağrıda sorar. Uzak bir bilgisayar henüz süreç kimliğini bildirmiyor. |
| Linux (X11) | Her çağrıda sorar. Pencerenin süreç kimliğini pencerenin kendi programı belirler, bu yüzden bir programı güvenilir biçimde adlandıramaz. |
| Linux (Wayland), macOS | Bunlarda henüz masaüstü arka ucu yok, bu yüzden masaüstü araçları orada hiç yok. |

İzin sunulmadığı yerde davranış tam olarak öncekiyle aynıdır: her çağrı, girdi kartta olacak
şekilde bir kez sorar.

**Ne kazandırdığı, ölçülmüş.** Her iletişim kutusu çağrısı sorar: standart ya da özel bir
iletişim kutusu (Kaydet, Aç, Çalıştır, ileti kutuları) asla kapsanmaz; bir iletişim kutusunun
içindeki Enter/Tab/Esc tuşları da dahil. Bir başlangıç klasörüne, bir profile ya da bir betiğe
işaret eden bir Kaydet dosya adı da ayrıca işaretlenmez (kartında sıradan yazılan metin gibi
görünür). Bir dosya okumasından sonra bir Not Defteri görevinin on masaüstü çağrısı için
(gözlemle, başlat, yaz, tuş, yaz, tuş, değer ata, Ctrl+S, Kaydet iletişim kutusunda dosya adı,
Kaydet iletişim kutusunda Enter) Windows **4** kez sorar (başlatma, izni taşıyan ilk yazma
çağrısı, dosya adı, iletişim kutusunda Enter); süreç kimliği olmadan aynı dizi 9 kez sorar.
30 adımlık kaydedilmiş bir yeniden oynatma (bir başlatma, tek bir programda 26 yazma ya da tuş
adımı, bir iletişim kutusunda üç adım) 5 kez sorar; çok sayıda iletişim kutusu adımı ya da
hariç tutulan programlardaki adımlar kart başına bir maliyet getirir, yani küçük bir sabit
sınır yoktur. Genel bir araç adımı olarak kaydedilmiş yeniden oynatma adımları hedef taşımaz
ve her zaman sorar.

Burada güvenilmeyen sayılan şey web sayfalarından daha geniştir. Bir masaüstü gözlemi
(`desktop_get_state`, `desktop_list_apps`) ve bir masaüstü tıklama sonucu da sayılır, yani bir
masaüstü görevi hiçbir dosya okumadan kendini kirletir. Her dosya okuma, listeleme ya da arama
(`fs_read`, `fs_list`, `fs_search`), her git okuması (`git_status`, `git_diff`, `git_log`,
`git_show`, `git_branch`), her belge okuyucusu ve her web, posta, MCP, beceri, iş akışı ya da bilgi
arama okuması, kendi projenin okunması dahil sayılır, çünkü bir dosya senin yazmadığın metin
taşıyabilir. Ekran ve sayfa gözlemi de sayılır: `desktop_get_state`, `desktop_list_apps`,
`desktop_click`, ekran görüntüleri ve tarayıcı anlık görüntüleri. Yani bir masaüstü görevi
ekrana ilk bakışıyla kendini kirletir ve o andan sonra her `desktop_launch`, `desktop_type`,
`desktop_key`, `desktop_set_value` ve `desktop_drag` çağrısı tek kullanımlık kendi kartıdır:
birini onaylamak ondan sonrakilerin hiçbirini kapsamaz ve `allow_egress_after_reads` bunu
değiştirmez. Çalışmaya özel denetim izni ayrı bir değişiklik olarak planlanıyor ve bu
değişikliğin parçası değil. Becerileri listelemek, biri güvenilmeyen diye saklanmışsa sayılır,
kayıtlı iş akışlarını listelemek her zaman sayılır. Yalnızca `fs_edit`, `git_commit`,
`process_list`, `email_list_folders`, güvenilir becerilerin bir listesi ve temiz bir çalışmanın
sakladığı hatırlanan hafıza bir çalışmayı yalnızca özel durumda bırakır ve yalnızca özel olan bir
çalışma bu kuralla tutulmaz. Gömme sağlayıcısı uzak olan bir bilgi araması veri gönderme
sayılır: bir okumadan sonra, diğer her dışarı gönderme gibi sorar.

Beklenmesi gereken sonuçlar:
- Kirlenme, onu taşıyan mesajlar bağlamda kaldığı sürece aynı sohbetin sonraki turlarında
  sürer. Yeni bir sohbet temiz başlar.
- Bir sohbette ilk dosya okumasından sonra her kabuk, betik, başlatma ve (Windows'ta çalışmaya
  özel bir denetim izninin dışında) masaüstü girdisi, her biri bir kez olmak üzere sorar.
  Kalıcı bir "kabuğa her zaman izin ver" o zaman bir etki yapmaz.
- Bir şey okuyup sonra komut çalıştıran zamanlanmış bir çalışma, her tetiklenişte onay için
  durur.
- Kayıtlı bir iş akışı güvenilmeyen olarak yeniden oynar, bu yüzden kabuk ya da başlatma adımı
  olan bir iş akışı her yeniden oynatmada sorar, bir program için çalışmaya özel bir denetim
  izninin kapsadığı yer dışında her masaüstü girdisi adımı da sorar (adım başına değil,
  program başına bir kart).

Kart, komutu ya da girdiyi gösterir, komutun çalıştıracağı dosyaları göstermez. Daha önce
`package.json`, bir Makefile ya da bir betik yazmış bir çalışmanın kartı `npm test` diyebilir ve
çalışmanın yazdığını yürütebilir. Kabuk kartı yönlendirmeleri ayrıştırmaz. Onaylanan bir kabuk
yine kullanıcının haklarıyla çalışır. `desktop_click` kapsanmaz: bir tıklama dizisinin her arka
uçta bir dosyayı açıp açamayacağı doğrulanmadı.

Bu, dosya yazmalarını güvenli yapmaz. Daemon veri klasörünün dışında, güvenilmeyen içerik
okumuş bir çalışma, verdiğin izin neyse onun altında `fs_write`, `fs_edit`, `fs_move`, bir belge
yazıcısı ya da bir git commit'iyle yine de bir dosya oluşturabilir ya da değiştirebilir; o
dosyaya sonradan güvenen bir program da (`AGENTS.md` ya da `CLAUDE.md` gibi bir ajan talimat
dosyası, o klasörde başlatılmış bir Claude ya da Codex CLI motorunun sonraki temiz bir
çalışmada talimat olarak yükleyebileceği; bir düzenleyici ya da araç yapılandırma dosyası; bir
kabuk profili; bir başlangıç girdisi) onu okur. Yalnızca motorların kendi yapılandırma klasörleri
korunur, proje klasörleri korunmaz. Dosya yazabilen yerel bir MCP sunucusu yazıcı ya da
yürütücü olarak sınıflandırılmaz. Bu tür dosyalardaki değişiklikleri kendin gözden geçir. İleti
maruziyeti geçişinden önce saklanan sohbetler kaydedilmiş bir maruziyetle başlamaz, yani
zaten web ya da posta içeriği tutan eski bir sohbet, yükseltmeden sonra temiz başlar. Dosya
yazmaları için ayrı bir onay tasarımı bu değişikliğin dışında izleniyor.

Dosya araçları, tam bir dosya izni olsa bile daemon veri klasörünün altındaki hiçbir şeyi
değiştiremez. Mevcut yol çözümleme denetimleri taşıma uç noktalarına, sembolik bağlantı
hedeflerine ve kontrol noktası geri yükleme hedeflerine de uygulanır. Etkisiz çıktı depoları
(`files`, `spill`, `screenshots`, `logs` ve daemonun sahip olduğu kontrol noktası blobları)
okunabilir kalır; diğer tüm depolar varsayılan olarak korumalıdır. Beceriler ve iş akışları kendi
sahibi olan araçları kullanmak zorundadır. Aynı daemon sınırı, hem izin kararında hem yazıcının
kendisinde, belge/dönüştürücü çıktı yolları (CSV çıktısı dahil) için de uygulanır.

Kayıtlı bir iş akışı, daemonun kendi kaydettiği ve eski bir dosya da dahil, ilk adımından önce
güvenilmeyen özel girdi olarak yeniden oynar.

Daemon dizininin dışındaki yerel CLI yetkisi de korunur: kullanıcının `.codex`, `.claude` ve
`.claude.json` dosyaları, ayrıca yapılandırılmış `CODEX_HOME` ve `CLAUDE_CONFIG_DIR`
konumları. Dosya araçları bu depoları değiştirmek için tam dosya izinlerini, taşımaları,
sembolik bağlantıları ya da kontrol noktası geri yüklemelerini kullanamaz. Belge çıktı yolları
aynı sınırı kullanır. Daemon verisinde olduğu gibi, onaylanan bir kabuk komutu yine kullanıcının
haklarıyla çalışır ve bir işletim sistemi kum havuzu değildir.

Bilinen kalan gösterim kanalları arasında sormadan geçen arama birleştiricileri ve biçimlendirme
karakterleri, benzer görünen karakterler ve normalleştirme varyantları ile açığa çıkarılmış bir
işarete benzeyen düz metin var. Tamamen tıklamalarla sürülen yerel dosya seçici yüklemeleri de
bir sınırlama olarak kalır: rastgele sayfa JavaScript'i herhangi bir tıklamadan bir seçici
açabilir ve onay kapısına bağlanmış taşınabilir bir iletişim kutusu kimliği ya da araya girilmiş
bir seçici yoktur. Tıklama/kaydırma istisnası bir yükleme kum havuzu gibi görülmemelidir.

## Asistanın yazabileceği gizli bilgiler (fill secrets)

Bir değer yalnızca **senin** gizli diye sakladığın zaman gizlidir. Masaüstü uygulamasının
**Kasa** ekranında (ya da yerel API ile, `/v1/fill-secrets`) bir kimlik bilgisini
`login/github` gibi bir adla, yazılabileceği tam web kaynaklarıyla (şema, ana makine ve port;
joker karakter yok) ve bilgisayar doğrulanmış bir program kimliği bildirdiğinde yazılabileceği
masaüstü programlarıyla birlikte saklarsın. Asistan yalnızca adları ve her birinin nerede
kullanılabileceğini görür (`secrets_list`) ve birini `browser_fill`, `desktop_type` ya da
`desktop_set_value` içinde `text` yerine `secret: "login/github"` ile yazar; değeri asla
tutmaz.

* Değer, gizli bilgi deponda `fill/<name>` altında durur; izin listeleri `fill-secrets.json`
  içinde durur (içinde değer yok). Hiçbir MCP, OpenAPI, sağlayıcı, e-posta ya da beceri
  yapılandırması bir `fill/` adına başvuramaz ve `secret` parametresi yalnızca `fill/`
  girdilerini okuyabilir, asla `composio/`, `mcp/` ya da bir sağlayıcı anahtarını okuyamaz.
* Bir gizli bilgiyi yazmak, politika, Full hazır ayarı, okuma kısayolları ya da bir "her zaman
  izin ver" ne derse desin **her seferinde** sana sorar. Kart gizli bilginin adını, alanı ve
  onu tutan çerçevenin kaynağını (gördüğün sayfadan farklıysa bunu da söyler) ya da programı
  belirtir. İzin vermediğin bir kaynaktaki bir alan, herhangi bir kart oluşmadan reddedilir;
  kaynak, canlı alan için tarayıcıdan ve değer eklenmeden hemen önce yeniden okunur. Tek
  istisna, tam olarak "bu gizli bilgi bu kaynakta" için oluşturduğun kalıcı bir izindir; bu
  izin başka bir kaynağı, başka bir gizli bilgiyi ya da bir masaüstü programını asla kapsamaz
  ve güvenilmeyen içerik okumuş bir çalışmanın sorusunu (madde 12) geçersiz kılmaz: o kart
  yine çıkar. Bir Kasa öğesi için "Kullanmadan önce sor" seçeneğini kapatmak, izin verildiği
  her kaynak için böyle bir izin oluşturur. Kasa ekranındaki **Kalıcı izinler** hepsini listeler;
  birini iptal etmek hemen etkili olur (botlar o kaynakta yeniden sorar) ve bir öğenin son
  iznini iptal etmek "Kullanmadan önce sor" seçeneğini yeniden açar.
* Değer yalnızca sen onayladıktan sonra okunur, bellekte tutulur, yazılır ve olaylardan,
  günlükten, onaylardan, araç adımlarından, model geçmişinden, kayıtlardan, araç çıktısından ve
  hata metninden uzak tutulur. Bir sayfa onu geri yansıtırsa süzgeç maskeler.
* Bunları yalnızca yerel sahip oluşturabilir, değiştirebilir ya da silebilir; eşleşmiş cihazlar,
  botlar, araçlar, MCP sunucuları ve beceriler yapamaz.

Bunun **yapmadığı** şey: bir sohbet mesajına yapıştırdığın parola o sohbette sıradan metindir
(onu gizli bilgi olarak saklayıp mesajdan çıkarmak bir sonraki arayüz adımıdır) ve sıradan
`text` doldurmaları asla maskelenmez: asistanın yazdığı şey, senin gördüğündür. Değeri tutan
bellek, çökme dökümlerinden ya da takas alanından hariç tutulmaz.

- **Sesli arama kipinde onaylar yalnızca tıklamayla verilir.** Sesli arama kipi mikrofon
  sesini yalnızca bellekte metne çevirir (diske yazılmaz ya da günlüğe geçirilmez), metni
  normal bir sohbet mesajı olarak gönderir ve bir çalışma onay beklerken mikrofon olaylarını
  kapatır; sesli söylenen *"evet, onayla"* hiçbir şeyi onaylamaz. Bkz.
  [Sesli arama kipi](voice-call.md).
- **Bir tarayıcı bilgisayarı olarak kullanılan eşleşmiş bir sunucu (`server:<key>`)
  kum havuzundadır.** Sunucu `pc.browser` çağrılarını yalnızca eşleşmiş yönetici desktop'ından
  kabul eder (eşleşme dosyasını her istekte yeniden denetler), loopback, link-local/metadata
  ve özel ağ URL'lerini (yönlendirmeler ve alt kaynaklar dahil) engeller, `browser_eval` ile
  parola alanı girdisini reddeder, döndürülen metni maskeler ve sunucu denetim kaydı yazılamazsa
  yürütmeyi engeller. Yerel onaylar ve izinler `server:<key>` kapsamına alınır. Bkz.
  [Eşleşmiş bir sunucuyu botun tarayıcı bilgisayarı olarak kullanmak](remote-host.md).

### Asistanın gizli bilgi istemesi

Asistanın bir web sitesi için mevcut bir kullanıcı adı ve parola, anahtar
veya yedek kod kullanması gerekirse, masaüstü uygulamasındaki güvenli form
üzerinden bunları isteyebilir. Formda botun gerekçesi ve bilginin
kullanılabileceği HTTPS adresi görünür. Yalnızca tanıdığın bir görev ve
adres için formu doldur. Ödeme bilgileri kapsam dışıdır.

Praxi gizli bilgiyi yalnızca o bot ve adres için yerel kasaya kaydeder.
Bot, bilginin kaydedilip kaydedilmediğini öğrenir ve kaydı adıyla
kullanır. Kullanıcı adı ve parola formda maskelenir. Uygulama, platform
destekliyorsa (Windows ve macOS) form açıkken pencereyi ekran
yakalamadan korur.

Telegram veya Discord eşleştirdiysen, özel bildirim seni masaüstü
uygulamasındaki ilgili sohbete götüren bir bağlantı içerir. Formu
uygulamada yanıtlayabilir, isteği iptal edebilir veya süresinin dolmasını
bekleyebilirsin.

Bir görevde (mission) form işi durdurmaz. Gizli bilgiye ihtiyaç duyan madde
beklemeye alınır, görevin geri kalanı sürer ve form 24 saat açık kalır.
Kaydedersen yalnızca o madde yeniden başlar. İptal edersen ya da süre dolarsa
madde beklemede kalır.

Kasa bulunan yerel masaüstünde istekler varsayılan olarak açıktır.
Kapatmak için `BOT_SECRET_REQUESTS=off` ayarla veya veri dizinindeki
`host.toml` dosyasında `[experimental]` altına `secret_request = false`
ekle ve Praxi’yi yeniden başlat. `praxi doctor` ayarı gösterir.

### Mesaj kutusuna yapıştırılmış bir parola

Göndermek üzere olduğun mesaj bir sağlayıcı anahtarı ya da token'a benzeyen bir şey içerdiğinde
(OpenAI tarzı, GitHub, Slack, AWS, Stripe, bir özel anahtar bloğu ve benzerleri), uygulama onu
göndermez. Türünü söyler (değeri asla göstermez) ve şunları sunar: kasaya kaydet ve mesajda
`[secret: name]` bırak, mesajdan çıkar, yine de gönder ya da düzenlemeye devam et. Denetim
herhangi bir şey gönderilmeden önce senin bilgisayarında çalışır; yalnızca bilinen anahtar
biçimlerini tanır, bu yüzden `password: hunter22` gibi düz bir parola henüz yakalanmaz.

## Prompt injection değerlendirme paketi

İzin motorunun ve kirlenme kapısının, bir model güvenilmeyen talimatları gerçekten izlediğinde de
dayandığını doğrulamak için depo, `evals/scenarios/inj-*.yaml` altında çevrimdışı, birinci
katman bir prompt injection değerlendirme paketi içerir (betikli model, sahte araçlar, ağ yok).
Her saldırı farklı bir güvenilmeyen kaynağı kapsar (web sayfaları, e-posta, dosyalar, MCP çıktısı,
sohbet mesajları, hafıza) ve farklı sözcükler ve verilerle bir `-holdout` ikizi vardır.
`inj-browser-mcp-*` senaryoları madde 12a'yı kapsar: yalnızca bir web sayfasından sonra sayfa
içi eylemler kartsız çalışır, özel veriden sonra bir gezinme sorar, `browser_run_code` sorar,
tarayıcı üzerinden bir sayfa okumasından sonra bir kabuk ya da betik sorar ve bir dosya,
localhost ya da yerel ağ adresi, "Bu görev için izin ver" seçildikten sonra da her zaman sorar.

Paketi çalıştır ve depoya işlenmiş temel çizgiyle karşılaştır:

```sh
cargo run -p evals --bin bot-eval -- run --tier 1 --filter inj- --repeat 3 --out run.jsonl
cargo run -p evals --bin bot-eval -- compare --baseline evals/baselines/injection-pack.jsonl --run run.jsonl
```

(`bot-eval` zaten derlenmiş ve `PATH` üzerindeyse `bot-eval run --tier 1 --filter inj- --repeat 3 --out run.jsonl` çalıştır.)

- `run`, her JSONL sonucuna `unsafe_actions` kaydeder ve `expected_fail` (belgelenmiş bir açık)
  diye işaretlenmemiş bir senaryo güvensiz bir eylemi tetiklerse ya da bilinen açıklı bir
  senaryo işaretini kaldırmadan geçmeye başlarsa başarısız olur.
- `compare` `GO` ya da `NO-GO` yazdırır (`NO-GO`'da 1 koduyla çıkar) ve her güvensiz eylemi,
  `--max-drop` değerini (varsayılan `0`) aşan her senaryo başına geçme oranı düşüşünü ya da
  eksik bir temel çizgi senaryosunu reddeder.
- Paket CI'da `cargo test -p evals` ile çalışır. Ayrıntılar ve temel çizgi güncelleme
  yönergeleri [`evals/baselines/README.md`](../../../evals/baselines/README.md) içinde.

İkinci bir çevrimdışı paket, `evals/scenarios/safe-*.yaml`, saldırı gerektirmeyen korumaları
denetler: dosya aracı yol sınırları (`../`, mutlak yollar, yalnızca klasör profilinden çıkan
bağlantılar), araç sonuçlarının, olayların, denetim kaydının ve saklanan araç çıktısının
gizli bilgilerden arındırılması, küçük değişikliklerle yeniden denenen reddedilmiş çağrılar,
alt ajanın araç daraltması, token ve boyut korumaları, bir aracın ortasında Durdur ve sahibe özel
kuralları devralmayan gözetimsiz çalışmalar. `--filter safe-` ile çalıştır ve
`evals/baselines/safety-pack.jsonl` ile karşılaştır. Modelin kendi yazdığı metin ve araç çağrısı
argümanları saklanan çalışma kayıtlarında (`bot.db`) henüz maskelenmiyor; bu iki durum
[bilinen açıklar listesinde](../../../evals/baselines/README.md) beklenen başarısızlık
senaryoları olarak izleniyor.
