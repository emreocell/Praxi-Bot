# Sohbet ve onaylar

## Bir botla konuşmak

Her konuşma, tek bir botla yapılan bir **sohbettir**. Sohbetler saklanır, yani sonra geri dönebilirsin. Bir mesaj bir **çalışma** başlatır (buna tur da denir): bot düşünür, araçları çağırır ve yanıtını akıtarak getirir. Bunu masaüstü uygulamasında, bir terminalde ya da Telegram'da izleyebilirsin.

Terminalden:

```sh
bot chat <bot-id> "Summarize the PDF files in ~/Downloads"
bot chat <bot-id> "and now rename them by date" --chat <chat-id>   # continue
```

`bot chat`, `--chat` vermezsen yeni bir sohbet açar. Yanıtı akarken yazdırır. Araç çağrıları, onay istekleri ve yönlendirme kararları stderr'e, yanıt metni stdout'a yazılır (böylece `bot chat ... > answer.txt` çalışır). Genel `--json` bayrağıyla stdout NDJSON olur, her satırda bir olay bulunur.

### Bot meşgulken mesaj göndermek

Sohbette bir tur zaten çalışıyorsa ne olacağını `--mode` belirler:

- `queue` (varsayılan): mesaj bekler ve bir sonraki tur olarak çalışır.
- `steer`: mesaj çalışan tura eklenir, böylece botu yönlendirebilirsin.

### Dosya ve resim eklemek

`POST /v1/chats/{id}/attachments` ile bir dosya yükle (ham gövde, görünen ad için `?name=`) ve mesajı gönderirken dönen kimlikleri `attachment_ids` olarak ver. Bu durumda metin boş olabilir. Dosya türü adından değil içeriğinden anlaşılır:

- Resimler (PNG, JPEG, GIF, WebP, en çok 5 MB) modele resim olarak gider. Görme desteği olmayan bir model atlanır ya da tur, hiçbir modelin bunu desteklemediğini bildirir.
- PDF, Word (`docx`), Excel (`xlsx`) ve PowerPoint (`pptx`) dosyaları ile düz metin dosyaları (en çok 15 MB) bir kez okunur, gizli bilgiler maskelenir ve metin (en fazla yaklaşık 60 KB) modele talimat olarak değil veri olarak verilir.
- Ses, video, arşiv, eski Office dosyaları ve diğer ikili dosyalar açık bir iletiyle reddedilir. Konuşmayı yazıya çeviren bir özellik henüz yok.

Bir mesaj en çok 6 dosya taşır. Dosyalar veri klasöründe `attachments/` altında saklanır.

### Sesli arama (masaüstü uygulaması)

Eller serbest konuşmak için masaüstü yazı alanındaki telefon düğmesine (**Sesli sohbeti başlat**) tıkla. Uygulama her turu bellekte kaydeder, **Ayarlar → Genel** içinde belirlediğin dikte uç noktasıyla yazıya çevirir, sıradan bir sohbet mesajı olarak gönderir ve yanıtı cümle cümle sesli okur. Onaylar yine yalnızca sohbetteki karta tıklanarak verilir. [Sesli arama kipi](voice-call.md) sayfasına bak.

### Eğik çizgi komutları

Yönlendirici açıkken `/stop` (geçerli çalışmayı durdur), `/new` (yeni sohbet başlat) ve `/workflow <name> key=value ...` (kayıtlı bir iş akışını doğrudan çalıştır, bkz. [İş akışları](workflows.md)) modele ulaşmadan doğrudan işlenir.

Masaüstü yazı alanında bir mesajın başına `/` yazmak, uygulamanın kendi işlediği (bota hiç gönderilmeyen) yerel komutları da içeren bir komut menüsü açar: `/export` (açık sohbeti Markdown olarak indir) ve uygulama kısayolları `/help`, `/settings`, `/models`, `/apps` ile `/schedules`. Telegram'da ek komutlar var, bkz. [Telegram](telegram.md).

### Sohbetleri yönetmek (masaüstü uygulaması)

Masaüstü uygulamasında **sol şerit yalnızca botları listeler** (tek tek sohbetleri değil). Bir bota tıklarsan en son arşivlenmemiş sohbeti açılır, henüz sohbeti yoksa boş ekranı açılır.

Her botun sohbetleri sağdaki **bot panelinde** durur: paneli açmak için başlık çubuğundaki bot simgesine tıkla, sonra **Sohbetler** altındaki **Kitaplık** sekmesine geç. Bir botun 6 ya da daha fazla sohbeti olunca listenin üstünde bir **Sohbetlerde ara** kutusu çıkar.

Her sohbet satırında, `PATCH /v1/chats/{id}` ve `DELETE /v1/chats/{id}` ile çalışan bir taşma (`...`) menüsü var:

- **Yeniden adlandır**: sohbet başlığını yerinde düzenler (1 ile 120 karakter arası; `Enter` kaydeder, `Esc` vazgeçirir).
- **Sabitle / Sabitlemeyi kaldır**: sabitlenen sohbetler bir iğne simgesiyle listenin en üstüne çıkar.
- **Arşivle / Arşivden çıkar**: arşivlenen sohbetler listenin altındaki, açılıp kapanan **Arşivlenenler (N)** grubuna taşınır (arama yaparken, arşivdeki sohbetler eşleşirse grup kendiliğinden açılır).
- **Sil**: onay ister ve sohbeti mesajlarıyla birlikte kalıcı olarak siler. O sohbette bir çalışma etkinken silme engellenir, önce çalışmayı durdur.

### Praxi bağlantılarını açma (masaüstü uygulaması)

Bir Praxi bağlantısı, bir tarayıcıdan ya da başka bir uygulamadan bir botu, sohbeti ya da çalışmayı açabilir. Bağlantı, zaten var olan bir şeyin kimliğini taşır. Kimliği küçük harfli, tireli haliyle şu biçimlerden birinde kullan.

| Bağlantı | Ne açılır |
| --- | --- |
| `praxi://bot/<id>` | O botu sol şeritten seçmekle aynı görünüm |
| `praxi://chat/<id>` | O sohbet |
| `praxi://run/<id>` | O çalışmanın adım listesi ve durumu |

Bağlantı, Praxi'de o an seçili olan bilgisayarda arar. Öğe silinmişse ya da başka bir bilgisayardaysa Praxi bir ileti gösterir. Bir bağlantıyı açmak mesaj göndermez, çalışma başlatmaz ve hiçbir onayı yanıtlamaz. Hangi sistemlerde desteklendiğini [Kurulum ve ilk çalıştırma](install.md) sayfasında bul.

### Mesajları kopyalama ve Markdown dışa aktarma

- **Bir mesajı kopyala**: herhangi bir kullanıcı ya da asistan mesajının üstüne gelince ya da odaklanınca balonun altında, **Mesajı kopyala** düğmesi olan bir eylem satırı çıkar (kısa bir **Kopyalandı** onayı gösterir). Yalnızca kullanıcının gördüğü metni (düz metin ya da Markdown) kopyalar, iç ek işaretlerini ayıklar ve ham iç araç JSON'unu asla kopyalamaz. Asistan yanıtlarındaki kod bloklarının da kendi kopyalama düğmesi vardır.
- **Sohbeti Markdown olarak dışa aktar**: açık sohbeti, adını sohbet başlığından (başlıksızsa bot adından) alan bir `.md` dosyası olarak indirmek için masaüstü yazı alanına `/export` yaz. Dosyada kullanıcı ve asistan mesajları UTC zaman damgalarıyla yer alır. Bota hiçbir şey gönderilmez.

### Okunmamış işaretleri

Daemon okunma durumunu saklamaz. Masaüstü uygulaması sunucu başına ve sohbet başına okunma zamanlarını yerelde tutar (uygulama bir sunucuyu ilk gördüğünde bir başlangıç noktası kaydedilir, böylece var olan sohbetlerin hepsi okunmamış olarak başlamaz):

- **Sol şeritte okunmamış noktası**: bir botun sohbetlerinden birinde, son okunma zamanından yeni bitmiş bir çalışma ya da bekleyen bir karar varsa bot okunmamış noktası gösterir.
- **“Yeni mesajlar” ayracı**: henüz görülmemiş asistan yanıtları olan bir sohbeti açınca, ilk görülmemiş öğenin üstüne bir **Yeni mesajlar** ayracı konur. Ayracı geçecek kadar kaydırınca ayraç kalkar ve pencere görünür ve odaktayken sohbet okunmuş sayılır.

### Klavye kısayolları ve “Yenilikler”

- **Klavye kısayolları penceresi**: hiçbir metin alanı odakta değilken herhangi bir yerde **`?`** tuşuna bas (ya da `Ctrl/Cmd+K` ile açtığın komut paletinden **Klavye kısayolları**'nı seç, hesap menüsünden aç ya da yazı alanına `/help` yaz). Pencere şunları listeler: genel kısayollar (`Ctrl/Cmd+K` komut paleti, `?` kısayollar, `Ctrl/Cmd+,` ayarlar, `Ctrl/Cmd+F` sohbette ara, `Esc` yan paneli kapat, `Shift+Esc` canlı görünümün klavye odağından çık ve senin belirlediğin durdurma kısayolu), yazı alanı kısayolları (belirlediğin gönderme ve yeni satır tuşları, `Up`/`Down` ile mesaj geçmişini geri çağırma, eğik çizgi/bahsetme menüsünde gezinme), yazı alanı eğik çizgi komutları ve pencere gezinme tuşları.
- **“Yenilikler” kartı**: bir uygulama güncellemesinden sonra masaüstü uygulaması, kurulu her sürüm için bir kez bir **Yenilikler** penceresi gösterir. Notlar uygulamanın içinde gelir (İngilizce, Türkçe ve Almanca), ağdan hiçbir şey indirilmez. İlk karşılamayı henüz tamamlamamış yepyeni bir kurulum, kartı göstermek yerine sürümü sessizce kaydeder.

### Sunucu çevrimdışıyken göndermek

- **Bu bilgisayar (yerel daemon)**: bir mesaj gönderilirken yerel daemon'a bağlantı reddedilirse ya da ulaşılamazsa masaüstü uygulaması, servisin gerçekten kapalı olduğunu doğrulamak için `/v1/health` adresini yoklar (yavaş bir yanıt ya da zaman aşımı asla yeniden denemeyi tetiklemez, yani canlı bir daemon hiçbir zaman çift mesaj almaz), *“Bu bilgisayardaki Praxi servisi başlatılıyor…”* yazar, yerel daemon'ı kendiliğinden başlatır ve hazır olunca mesajı gönderir.
- **Eşleşmiş uzak sunucu**: masaüstü uygulaması uzak bir sunucuyu açamaz ya da başlatamaz. Yazdığın metin yazı alanında taslak olarak kalır ve sunucu yeniden çevrimiçi olunca uygulama **“Sunucu çevrimdışı”** yazısını bir **Yeniden dene** düğmesiyle gösterir.

### Sorular

Bir bot sana bir soru kartıyla (`ask_user`) bir şey sorabilir. CLI'da numaralı seçeneklerle `[question]` görürsün ve şöyle yanıtlarsın:

```sh
bot answer <call-id> "the second one"
```

## Onaylar

Bir bot, politikasına göre senin iznini gerektiren bir şey yapmak istediğinde çalışma **bekler** ve bir onay isteği çıkar: masaüstü uygulamasında bir kart, Telegram'da satır içi düğmeler, CLI'da bir `[approval needed]` satırı.

```sh
bot status                              # how many approvals are pending
bot approve <tool-call-id>              # approve this one call
bot approve <tool-call-id> --scope this_chat
bot approve <tool-call-id> --deny --reason "not that folder"
```

Reddederken verdiğin neden modele iletilir, böylece o başka bir yol deneyebilir.

### Bir onay ne kadar geçerli olur

| Kapsam | Anlamı |
| --- | --- |
| `once` (varsayılan) | Yalnızca bu tek çağrı |
| `this_chat` | Bu araç, bu sohbetin geri kalanı için |
| `always` | Bu araç, bu bot için, kalıcı olarak hatırlanır |
| `always_category` | Aracın bütün kategorisi (örneğin her `fs` aracı), bu bot için, kalıcı olarak hatırlanır |
| `run_control` | Yalnızca “Bu görev: <program>” seçeneğini sunan bir masaüstü girdi kartında: çalışma bitene kadar o tek programdaki yazma, tuşlar, değerler ve sürükleme (bkz. [güvenlik](security.md)) |

“Her zaman” seçimleri **izinler (grants)** olarak saklanır. Onları gözden geçir ve kaldır:

```sh
bot grants list <bot-id>
bot grants revoke <bot-id> --tool fs_write
bot grants revoke <bot-id> --category fs
```

Bir izni geri aldıktan sonra eşleşen bir sonraki çağrı yeniden sorar. Geri almalar denetim günlüğüne yazılır. İzinler, aşağıdaki mutlak tabanları asla kaldırmaz.

## İzin seviyeleri

Her araç bir **kategoriye** aittir ve her kategorinin her bot için bir **seviyesi** vardır.

| Seviye | Ne olur |
| --- | --- |
| Deny | Çağrı reddedilir, bota söylenir |
| Ask | Sen onaylayana ya da reddedene kadar çalışma duraklar |
| Allow | Çağrı doğrudan çalışır |
| Allow with limits | “Sınırlar içinde izinli” anlamında düşünülmüştür. Şu anki çözücü bunu Allow gibi işler |

Kategoriler: `fs` (dosyalar), `shell` (komutlar), `desktop` (uygulamalarda tıklama ve yazma), `browser`, `accounts` (bağlayıcı hesapları), `messaging` (e-posta, sohbet), `money` (ödemeler, satın almalar), `system` (ayarlar, yazılım kurma), `remote` (başka bir bilgisayar) ve `script` (kod çalıştırma). İç `meta` kategorisi (sana bir soru sormak, yapılacaklar listesi tutmak) her zaman izinlidir.

### Dört hazır ayar

| Hazır ayar | Sade sözlerle |
| --- | --- |
| `approve-destructive` (varsayılan, önerilen) | Her kategori Ask'a ayarlanır, ama **okuyan araçlar muaftır** (aşağıya bak). Dosyaları değiştiren, uygulamalarda tıklayıp yazan, komut çalıştıran ya da mesaj gönderen her şey önce sorar |
| `read-and-draft` | Varsayılan gibi. Dosyalar ve tarayıcı açıkça Ask'tır. Göndermek sorar |
| `read-only` | Her kategori Deny'dır. Bot yalnızca izin sisteminden hiçbir şey istemeyen araçları kullanabilir, örneğin sana soru sormayı |
| `full` | Her kategori Allow'dur: aşağıdaki tabanlar dışında hiç soru yok. Yalnızca güvendiğin bir korumalı alandaki botlar için kullan |

“Okuyan araçlar muaftır”: daemon, yalnızca okuyan araçların (örneğin `fs_read`, `fs_list`, `browser_snapshot`, `desktop_get_state`) politika düzeyindeki bir “Ask”ı atlamasına izin verir. Bu, `read-only`/Deny için geçerli **değildir** ve tabanlar için asla geçerli değildir.

> Bu kılavuzdaki hazır ayar anlatımı koda uyar. `read-and-draft` ve `approve-destructive` hazır ayar adları bugün yalnızca küçük farklarla ayrışır. İkisi de dosya ve tarayıcı işlemlerinde sorar.

### Mutlak tabanlar

Bunlar, hazır ayar, izin ya da profil ne derse desin geçerlidir:

1. **Para her zaman sorar.** Bir `money` işlemi asla kendiliğinden izinli olmaz.
2. **Asla listesi her zaman sorar.** Yıkıcı ve geri alınamaz kabuk komutları, komutun ayrıştırılmasıyla tanınır (gevşek metin eşleştirmesiyle değil). Bu `sh -c`, `powershell -Command`, `sudo` ve boru hatları içindekileri de kapsar: disk biçimlendirme, `diskpart clean`, `mkfs`, bir aygıta `dd`, `bcdedit`, önyükleyici kurucuları ve korunan bir konumu hedefleyen yinelemeli silmeler (dosya sistemi ya da sürücü kökü, ana klasörün, `C:\Windows`, `Program Files`, `/etc`, `/usr` gibi sistem klasörleri). `/tmp/x` klasörünü yinelemeli silmek listede değildir.
3. **Alt ajanlar asla üst botlarını aşamaz.**

İzinler (“her zaman izin ver”) bir tabanı kaldıramaz.

### Okumadan sonra dışarı gönderme

Bir çalışma güvenilmeyen ya da özel içerik okuduktan sonra, veriyi makineden dışarı gönderebilen bir araç, bir izin ya da okuma kısayolu izin verecek olsa bile sorar. Masaüstünde yazma, tıklama ve program başlatma, tarayıcıda kaydırma ve sekme değiştirme, posta kutusunda okuma, arama ve okundu işaretleme bu gruptadır. Kart neyin okunduğunu söyler ve giden sorguyu, adresi ya da gövdeyi (her alıcı dahil, sonu kaydırarak görülebilir biçimde) ve hedef sunucuyu tam olarak gösterir. O kartı onaylamak yalnızca o tek gönderime izin verir, kalıcı bir izin oluşturmaz. Aynı sohbette sonraki bir mesaj, önceki yanıt modelin gördüğü konuşmada durdukça yine sorar. Tıklama, kaydırma ya da sürükleme veriyi makineden dışarı göndermez, bu yüzden okumadan sonra da izinli kalır. Yazma, yapıştırma, program başlatma ve bir sayfayı doldurma ise yine sorar. Bir dosyayı yazmak ya da taşımak, özel veri okumak sayılmaz. Çalışma yalnızca güvenilmeyen içerik okuduysa ve özel veri okumadıysa, ayarlanmış sağlayıcıya yapılan arama soru sormadan yapılır. Bot formunda, o bot için bu soruyu atlayan, varsayılanı kapalı, açık bir ayar vardır. Reddetme kuralları ve yukarıdaki tabanlar yine geçerlidir. Hiçbir şey okumamış bir çalışma eskisi gibi davranır.

### Tam seviye: okumadan sonra web'de çalışmak (sahibin başlattığı çalışmalar)

**Tam** hazır ayarı (“Tam”), botun her sayfa için sormak yerine işi yapmasını isteyenler içindir. **Kendi başlattığın** bir çalışmada (masaüstü uygulamasında yazdığın ya da kendi yönetici masaüstünden gönderdiğin), web sayfalarını okumak botu, yönetilen tarayıcıdaki **ya da herkese açık web okuma araçlarındaki** (`web_search`, `web_extract`, `web_feed`, `web_sitemap`, `web_metadata` ve isteğe bağlı kazıma eklentisi kuruluysa `web_scrape`) her yeni adımda yeniden sormaya zorlamaz: yeni bir sitedeki arama, sayfa, besleme ya da site haritası, o siteyi tarayıcıda açmakla aynı kurala bağlıdır. Bir sayfayı açmak zaten “güvenilmeyen bir şey okudu” sayılır. Botun devam edip edemeyeceğini belirleyen şey, çalışmanın ayrıca **özel verini** tutup tutmadığıdır: dosyaların, postaların, hafızan, ekranların ya da diğer botların verisi, aynı sohbetin önceki bir mesajından ya da başka bir botun yanıtından gelenler de dahil. Botun **hafızadan hatırladığı**, **bilgi tabanında** bulduğu ya da **önceki bir yanıtta** söylediği şeyler de özeldir, ama onların sözcükleri çalışmaya (tuzlu özetler olarak) bilinir, bu yüzden yalnızca o sözcüklerden ya da öbeklerden birini **kopyalayan** bir çağrı sorar. Metni karşılaştırılamayan özel veri (oturum açılmış bir tarayıcı, bir pencere ya da ekran, eklenmiş bir dosya, başka bir botun yanıtı, bir MCP aracının sonucu ya da saklanamayacak kadar büyük bir sonuç) her yeni sitenin sormasına yol açar. Adını verdiğin bir tarayıcı profilinde görülen sayfalar da özeldir, çünkü o profilde hesaplarında oturum açık olabilir. Botun kendi tarayıcısındaki sayfalar da, onu sen devraldıktan (örneğin giriş yapmak için) ya da kayıtlı gizli bilgilerinden birini içine yazdıktan sonra özel sayılır. Bu bilgisayarda ya da ağında açılan her şey de öyledir (`localhost`, `file:`, bir yerel ağ adresi). Botun `account_register` ile kendisi için yaptığı girişler sayılmaz, böylece kayıt işleri serbest kalır. Görev için yazdığın bilgiler (bir şirket adı, e-posta adresin) bu bakımdan özel değildir. Onları, bot formlara yazabilsin diye verdin.

| Durum | Sorar mı? |
| --- | --- |
| Çalışmada özel veri yok: herhangi bir herkese açık siteyi (yenilerini de), derin sayfaları, sorgu dizilerini açar, geri gider, sekme değiştirir, tıklar, kaydırır, verdiğin bilgileri yazar, seçenek seçer, tuşlara basar, form gönderir | Hayır |
| Çalışmada özel veri var ve bir web sayfası okudu: **adını verdiğin** (ya da önceki bir kartta onayladığın) bir sitenin derin sayfaları, alt alan adları ve formları | Hayır |
| Çalışmada özel veri var ve bir web sayfası okudu: yeni bir site, bir arama sorgusu ya da senin olmayan bir sayfadaki form, **adres ya da metin özel verinin 6 ya da daha fazla harfli bir sözcüğünü (ya da art arda üç sözcüğünü) içeriyorsa, tirelerle ya da noktalarla bölünmüş, yüzde ile kodlanmış ya da farklı aksanlı olsa bile**, bir anahtara, erişim koduna, özete ya da kodlanmış bir diziye benziyorsa, uzunsa (adres 120 karakterden, form metni 39 karakterden, arama sorgusu 160 karakterden uzun), bir `#fragment` taşıyorsa ya da çalışma bu tür yerlere zaten yaklaşık 1500 karakterden fazla gönderdiyse | **Evet** |
| Çalışmada özel veri var ve bir web sayfası okudu, ama adreste ya da metinde özel veriden gelen hiçbir şey yok | Hayır |
| Bir sayfaya, herhangi bir çalışmada, kart numarasına benzeyen ya da anahtar biçiminde bir şey yazar | **Evet** |
| Sahibin kayıtlı bir gizli bilgisini yazar (`secret` ile `browser_fill`) | **Evet**, her seferinde |
| Para harcar, asla listesindeki bir şeyi siler, posta araçlarıyla e-posta ya da mesaj gönderir, yayımlar, bir sayfa betiği çalıştırır (`browser_eval`), güvenilmeyen bir sayfayı okuduktan sonra bir program ya da kabuk komutu başlatır, adlandırılmış bir tarayıcı profili kullanır, yerel ya da özel ağ adresi açar | **Evet** |

Bunun neden yeterince güvenli olduğu: bir site, bot oraya gitti diye asla “senin” listene eklenmez, yalnızca sen adını verdiysen ya da onayladıysan eklenir. Özel verisi olmayan bir çalışmanın, düşmanca bir sayfanın bota taşıtabileceği hiçbir şeyi yoktur. Özel verisi olan bir çalışma, okuduğu özel sözcüklerin bellekte bir kaydını tutar (tuzlu özetler, asla metnin kendisi değil, çalışmayla birlikte silinir) ve bunlardan birini tekrarlayan bir çağrı sorar. Tabanlar, her zaman soran araçlar (aralarında posta göndermek de var: yazılı posta politikası oluşana kadar sormaya devam eder) ve engelleme kuralları bu kuraldan önce karara bağlanır ve dokunulmaz. **Senin başlatmadığın** bir çalışma (bir zamanlama, bir webhook, gelen bir e-posta, bir kanal, yönetici masaüstün olmayan eşleşmiş bir cihaz, bir alt ajan ya da yardımcı bot, harici bir ajan) eski kuralı ve tek seferlik kartlarını korur. Normal seviye ve özel seviyeler değişmez.

Durduramadığı şeyler: harf harf hecelemeyi (`h-a-n-n-e-l-o-r-e`) ve punycode sunucu adlarını, 6 harften kısa özel sözcükleri (4 haneli bir PIN, kısa bir ilk ad) ve üç sözcükten kısa her şeyi asla tanımaz. Düşmanca bir sayfa modeli, özel verini **yeniden kodlamaya ya da başka sözcüklerle ifade etmeye** (örneğin çevirmeye ya da farklı kelimelerle hecelemeye) ve yeni bir siteye göndermeye ikna edebilir. Ayrıca modelin bildiği ama bu çalışmanın hiçbir araç sonucundan gelmemiş özel veriyi de tanımaz. **Önceki bir turdan** gelen özel veri, özgün posta ya da dosya üzerinden değil, hâlâ sohbette duran o turun yanıtı üzerinden karşılaştırılır. Birkaç sözcük uzunluğundaki bir arama sorgusu arama sağlayıcısına gider, dolayısıyla içindeki özel verinin başka sözcüklerle ifadesi de tanınmaz (hacim yukarıdaki bütçeyle sınırlanır). Bu kuralın geçirdiği her çağrı denetim günlüğüne `mode.full.owner_browse` olarak yazılır (web okuma araçları da).

Tam seviyenin, senin başlattığın bir çalışma ya da görevin için kartsız yaptığı bir şey daha var: botun **kendi kayıtları** ([e-posta kılavuzundaki “Kayıtlar” bölümüne](email.md) bak). `account_register`, botun kendi açtığı bir hesabın parolasını `browser_fill` `secret` ile yazmak ve `browser_capture_secret`, çalışma sayfaları ya da postanı okumuş olsa bile sormadan çalışır; çünkü parolayı uygulama üretir, bot onu asla görmez ve yalnızca kaydedildiği tam siteye yazılır. Kendi sakladığın gizli bilgiler ve para yine sorar. Bu çağrılar denetim günlüğüne `mode.full.own_signup` olarak yazılır.

**Bota ne söylenir.** Tam seviyede botun temel talimatları şunu der: görevi, “başlayayım mı?” diye ya da onay istemeden sonuna kadar götür. Mantıklı varsayılanları (hangi siteler, adlar, ifadeler) kendin seç ve neyi seçtiğini söyle. Yalnızca senin bildiğin bir şeyi (bir parola, kişisel bir tercih) ya da kesin bir durma noktasında (para harcamak, çalışma alanının dışında silmek, gizli bilgiler, üçüncü kişilere mesaj göndermek, bir CAPTCHA ya da 2FA devri) sor. Sonunda “Tamam:” yaz ve geriye ne kaldığını söyle. Küçük bir model yine de durup sorabilir. **Ajan işleri, “lite” bir katmandan daha güçlü bir model ister** (örneğin Flash-Lite, mini, nano ya da Haiku modelleri): onlar uzun, çok adımlı talimatları daha az güvenilir izler. Tam seviyedeki bir bot böyle bir modelle çalışıyorsa bot paneli bir ipucu gösterir.

**Tam seviyede hâlâ soranlar.** Çalışma bir web sayfası okuduktan sonra bir program ya da kabuk komutu başlatmak (bir sayfa komutu seçmemelidir) her zaman sorar, yukarıdaki kesin durma noktaları da öyle. **Telegram** (ya da herhangi bir kanal, yerel API ya da yönetici masaüstün olmayan eşleşmiş bir cihaz) üzerinden gelen çalışma bu kurala göre sahibin başlattığı sayılmaz: onun web ve tarayıcı adımları tek seferlik kartlarını korur. Bu, şu anki durumdur, gelecek için bir söz değildir.

### Tam seviye: kendi zamanlamaların ve tetikleyicilerin

**Tam** seviyedeki bir bot, para harcamak ve kesin durma noktaları dışında hiçbir şey için sormaz. Bu, **senin yazdığın** otomasyon için de geçerlidir: bu bilgisayarda (ya da yönetici masaüstünde) masaüstü uygulamasında oluşturduğun bir zamanlanmış görev, e-posta kuralı, webhook kuralı ya da dosya izleyici Tam şeritte, kart çıkmadan çalışır. Böyle bir çalışmanın alt ajanı da aynısını yapar (derinlik sınırı her zamanki gibi).

Karar veren şey çalışmayı neyin başlattığı değil, **talimatı kimin yazdığıdır**. Daemon, her zamanlamayı ve kuralı kaydederken bir **yazar** not eder:

| Yazar | Kim | Tam seviyede kartsız çalışır mı? |
| --- | --- | --- |
| `owner` | Sen, bu bilgisayardaki uygulamada ya da yönetici masaüstünde | **Evet** |
| `bot` | Bir bot, zamanlama araçlarıyla (kartı onayladıktan sonra) | Hayır: tek seferlik kartlarını korur |
| `unknown` | Yazar kaydedilmeye başlamadan önce yapılmış, eşleşmiş bir telefonda ya da başka bir cihazda yapılmış ya da başka bir bilgisayardan buraya taşınmış | Hayır, sen onaylayana kadar |

Bir zamanlamayı ya da kuralı yeniden kaydetmek **seni** yazarı yapar (kayıt eşleşmiş bir telefondan geldiyse yine `unknown` olur). Böylece daha önce onayladığın bir şeye kimse yeni metin sokamaz. Kendi zamanlamasını düzenleyen bir bot onu `bot` zamanlaması yapar.

**Tek tıkla onayla.** Tam seviyedeki bir botun bot panelindeki **Otomasyon** sekmesi, yazarı bilinmeyen her zamanlamanın ya da kuralın altında **Bu benim** düğmesini gösterir. Bir kez bas, kayıt o andan itibaren senin sayılır (daemon denetim kaydına yazar). Düğme baktığın sürümü onaylar: metin bu arada değiştiyse (telefondan, bir bottan) hiçbir şey onaylanmaz, önce yeniden okursun. Bir botun yaptığı zamanlama böyle onaylanmaz: aç, metni kontrol et ve kaydet, böylece onu devralmış olursun. Senin olduğunu kanıtlayamayan bir çağrı (yalnızca API anahtarı, sahip anahtarı yok) asla sen sayılmaz: kaydettiği şey `unknown` olur.

**E-posta ve webhook kanalları.** Kurala bağlı olmayan, senin kurduğun e-posta kanalından ya da imzalı gelen webhook'tan başlayan bir koşu, o kanal ya da webhook hâlâ varsa ve aynı bota aitse kartsız çalışır; aksi halde kartları sürer.

**Alt ajanlar.** Şeridin hizmet verdiği bir çalışmanın (senin yazdığın bir çalışma, ya da senin yazdığın bir zamanlama veya tetikleyici) alt ajanları, her zamanki derinlik sınırı içinde onu devralır. Başka bir botun çağırdığı yardımcı bot hiçbir zaman devralmaz.

**Değişmeyenler.** Yalnızca “kim başlattı” kısıtı kalkar, o da yalnızca senin yazdığın otomasyon için. Bir e-postanın, webhook'un ya da dosyanın metni **güvenilmeyen veri** olarak kalır (alıntılanmış bir blok, çalışma güvenilmeyen işaretiyle başlar). **Para** her zaman sorar, yasak listesi, güvenilmeyen metinden üretilen kalıcı izinler, kendi hesapların için koruma kuralları ve gizli bilgi kuralları da öyle. Tam seviyenin altındaki bir bot değişmez: zamanlamaları ve tetikleyicileri her araç için bir kez sorar.

### Her zaman açık bir sunucuda sunucu durumu komutları

Normalde `shell_run` çıktısı çalışmayı güvenilmez (“komut çıktısı”) olarak işaretler, bu da sonraki her `shell_run` çağrısının yürütücü kapısında sormasına yol açar, `full` hazır ayarında bile. Her zaman açık bir sunucuda (`bot serve --host`), bir çalışma botun kendi sunucu bilgisayarını (`host` ya da `browser`, asla kullanıcının bilgisayarı değil) hedeflediğinde, sabit ve salt okunur durum komutlarından oluşan dar bir küme çalışmayı ne kirletir ne de güvenilmeyen içerik yürütücü kapısını tetikler:

- **İzinli komutlar ve kısa bayraklar** (yalnızca `command`, isteğe bağlı `timeout` ve isteğe bağlı `shell` = `sh` ya da `bash` ile `shell_run`):
  - `uptime` (`-p`, `-s`)
  - `whoami` (bayraksız)
  - `hostname` (`-f`, `-s`)
  - `uname` (`-a`, `-s`, `-n`, `-r`, `-v`, `-m`, `-p`, `-i`, `-o`)
  - `date` (`-u`, `-R`)
  - `df` (`-h`, `-H`, `-k`, `-m`, `-T`, `-P`, `-l`)
  - `free` (`-h`, `-b`, `-k`, `-m`, `-g`, `-t`, `-l`, `-w`)
  - `nproc` (bayraksız)
  - `id` (`-u`, `-g`, `-G`, `-n`, `-r`)
- **Neden güvenli**: Denetim yalnızca komut metnine bakar (en çok 3 boşlukla ayrılmış parça, en çok 64 karakter, yalnızca ASCII harfler, rakamlar, boşluklar ve `-`). Bu komutlar yol, URL ya da serbest metin argümanı almaz, hiçbir şeyi değiştirmez ve sabit makine durumu yazdırır. Yani bunlardan çalışmaya dışarıdan birinin yazdığı hiçbir metin giremez. Değer ayarlayan bayraklar (`date -s`, `free -s`, `df -t`, `hostname -F`) dışarıda bırakılmıştır.
- **Hâlâ soranlar**: Yol içeren (`df -h /etc`), boru, yönlendirme, zincirleme (`;`, `&&`), yerine koyma, tırnak, değişken, bilinmeyen bayrak, özel `cwd`, `sh`/`bash` dışı kabuk (PowerShell gibi) olan ya da sunucu olmayan bir daemon'da ya da kullanıcının bilgisayarında çalışan her komut, tam kirletmeyi ve yürütücü kapılarını korur. Ask kipi (`read-and-draft`) ve mutlak tabanlar değişmez.

### Zamanlamayla başlayan çalışmalar

Zamanlanmış bir çalışma ([Zamanlanmış görevler](scheduled-tasks.md) sayfasına bak), kuralı aksini söylemedikçe senin gibi davranır. Her tetiklenme kendi isteminden başlar: aynı zamanlamanın önceki tetiklenmeleri isteme geri yüklenmez, bu yüzden dünkü yanıt bugünkü çalışmanın sormasına yol açmaz. O sohbette bir insan konuşması ise yol açar. Çalışma bir onaya ihtiyaç duyarsa ve **30 dakika** içinde kimse yanıtlamazsa çalışma **duraklar** (tahmin yürütmez). Para ve asla listesi işlemleri, zamanlanmış çalışmalarda asla kendiliğinden onaylanmaz.

## Masaüstünü denetlemek

Masaüstü araçları (Windows'ta, Linux'ta ise X11/Xvfb altında) gerçek ekranda iş yapar. Masaüstü uygulaması, bir bot ekranı kullanırken ekranda bir katman gösterir ve denetimi devralmana izin verir:

- Denetimi alırsan (`human_driving`) ya da botu duraklatırsan masaüstü işlemleri hemen reddedilir ve bota yeniden denememesi söylenir.
- Genel durdurma kısayolu (varsayılan `Ctrl+Alt+Esc`) botu istediğin an durdurur. **Durdur**, bir duraklatma ya da devralmanın arkasında bekleyen çalışmaları da iptal eder.
- Bir ekran devralma botları duraklatılmış bıraktığı sürece ana ekranda **Duraklatmayı kaldır**, **Ekranı aç** ve **Durdur** düğmeli bir **“Bot duraklatıldı, seni bekliyor”** şeridi görünür kalır. Eşleşmiş bir uzak sunucuda, sunucunun **yönetici masaüstü** (ekran girdi iznini hâlâ taşıdığı sürece), kendi ekran girdi iznini yitirmeden bu şeritten `POST /v1/control/recover` ile duraklatılmış bir devralmayı kaldırabilir. Bkz. [Canlı ekran gizliliği ve insan denetimi](security.md).

Tarayıcı araçları varsayılan olarak ayrı, yalıtılmış bir tarayıcı profili kullanır, yani normal gezinme oturumlarına dokunmaz.

## Dosya değişikliklerini geri almak

Dosya araçları her değişiklikten önce bir **kontrol noktası** (etkilenen yolların yedeği) alır. Bot kontrol noktalarını listeleyebilir ve geri yükleyebilir (`checkpoint_list`, `checkpoint_restore`). Geri yükleme önce mevcut durumun bir kontrol noktasını alır, böylece geri yüklemenin kendisi de geri alınabilir. Kabuk komutları ve masaüstü işlemleri kontrol noktalarının kapsamında değildir.

Bir çalışmanın tamamını (ya da birbirine bağlı bir dosya kümesini) çalışmanın değişiklik incelemesinden geri yüklemek iki aşamada gerçekleşir. Önce daemon bir denetim satırı (`checkpoint.restore_started`) yazar ve dosyaların şu anki halinin bir güvenlik kontrol noktasını alır. Ancak o kontrol noktası tamamlandıktan sonra her dosyayı hedefinin yanında hazırlar ve hazırlanan kopyaları yeniden adlandırmalarla yerine geçirir. Bir adım başarısız olursa daemon yer değiştirmiş dosyaları geri almaya çalışır. Bu geri alma elinden gelenin en iyisidir ve kendisi de başarısız olabilir. O durumda sonuç bunu bildirir ve dosyalar karışık bir durumda kalabilir. Daemon'ın düzgün durdurulması, çalışan bir geri yüklemenin bitmesini bekler. Temiz tamamlanan bir geri yüklemeden sonra dosyalar incelenmiş sayılır. Biri sonradan düzenlenirse “sonradan değişti” olarak görünür ve onu yeniden geri yüklemek onay ister. Geri yüklenmesi atlanan bir yol (örneğin yeniden oluşturulamayan bir bağlantı) incelenmiş olarak işaretlenmez. Yarıda başarısız olan bir geri yüklemeden sonra hiçbir şey incelenmiş işaretlenmez, bu yüzden sonraki deneme “doğrulanamadı” gösterir ve onaylamanı ister.

Daemon işlemi bir geri yüklemenin ortasında sert biçimde sonlandırılırsa (elektrik kesintisi, çökme, `kill -9`) hiçbir tamamlanma kaydı yazılmaz. Geriye ne kaldığı, anın nerede olduğuna bağlıdır:

- Güvenlik kontrol noktası hâlâ kopyalanırken sonlandırıldı: bir `checkpoint.restore_started` denetim satırı vardır, kullanılabilir güvenlik kontrol noktası yoktur ve yer değiştirmiş dosya yoktur. Geri almayı yeniden çalıştır.
- Yer değiştirmeler başladıktan sonra sonlandırıldı: denetim satırı, geri yüklemeden hemen önceki dosyaları tutan eksiksiz bir güvenlik kontrol noktası (işlem etiketi `pre-restore:...`, kurtarma kimliği `cp-...`, `GET /v1/runs/{id}/changes` içinde `safety_checkpoint_ids` olarak listelenir), geri yüklenmiş ve henüz geri yüklenmemiş dosyaların bir karışımı olabilir ve hedeflerin yanında `<filename>.praxi-restore-<uuid>`, `<filename>.praxi-restore-old-<uuid>` ya da `<filename>.praxi-restore-aborted-<uuid>` adlı geçici dosyalar kalır.

İkinci durumdan kurtulmak için o güvenlik kontrol noktasını geri yükle (çalışmanın değişiklik incelemesinden ya da `cp-...` kimliğiyle `checkpoint_restore` ile). Önizlemesi “doğrulanamadı” gösterebilir, bu durumda üzerine yazmayı onaylaman gerekir. Kalan geçici dosyalar varsa sil, sonra geri almayı yeniden çalıştır. Açılışta hiçbir şey kendiliğinden onarılmaz. Kontrol noktası bildirimi diske eşitlenir, ama kopyalanan dosya içerikleri yer değiştirmelerden önce tek tek eşitlenmez. Bu yüzden elektrik kesintisi bir güvenlik kopyasını yine de yitirebilir, düz bir işlem sonlandırması yitirtmez.

Hâlâ etkin olan diğer çalışmaların yolları, geri yüklemeden önce yalnızca yol metnine bakılarak karşılaştırılır: daemon bunu yapmak için hiçbir dosyaya ya da klasöre dokunmaz, yani denetlerken bir bağlantıyı izleyemez ya da bir ağ paylaşımına erişemez. Karşılaştırma Windows ve macOS'ta büyük/küçük harfi yok sayar, `.` ve `..` parçalarını sadeleştirir ve bir klasörü ile içindeki her şeyi çakışan sayar. Ret iletisi, engelleyen çalışmayı ve çakışan iki yolu adlandırır. Algılamadıkları: aynı dosyayı farklı yazımla gösteren bir sembolik bağlantı, junction, Windows 8.3 kısa adı ya da eşlenmiş sürücü, sabit bağlantılar ve bind mount'lar. Daemon'ın kendi dosya araçları hâlâ özel bir kilitle sıraya sokulur, ama çalışan başka bir botun kabuk komutları ve masaüstü işlemleri bu denetimin dışındadır. Bu yüzden çalışan başka bir botun üzerinde uğraştığı dosyaları geri almaktan kaçın.

## Tur başına sınırlar

Kontrolden çıkan çalışmaları durdurmak için her tur varsayılan olarak 40 model adımı, 2 milyon token, araç çağrısı başına 120 saniye ve model çağrısı başına 300 saniye ile sınırlıdır. Aynı araç çağrısını art arda birkaç kez tekrarlarsa da durur. Çok uzun araç çıktısı bir dosyaya kaydedilir ve modele yalnızca kısa bir önizleme gösterilir.

## Bildirimler ve onaylar

Pencere odakta değilken ve bir çalışma onayına ihtiyaç duyduğunda masaüstü uygulaması, sade sözlerle kısa bir bildirim gösterir (“Praxi, Playwright'ı kullanmak istiyor (tarayıcıda değerlendirme)”). Ham çağrıyı, argümanlarını ya da bilgisayar kimliklerini asla göstermez. “Özel bildirimler” açıkken yalnızca genel bir metin gösterilir.

**Windows.** Bildirimin düğmeleri vardır: **İzin ver** (yalnızca bu çağrı), **Her zaman izin ver** ve **Reddet**. Bir düğme, sohbetteki kartla aynı onay yolunu çalıştırır: o anda bekleyen onaya karşı denetlenir, “Her zaman izin ver” yalnızca kart onu sunacaksa görünür ve kart düğmesiyle aynı sahip yetkisini ister. Önce okuman gereken bir şey taşıyan onaylar (incelenecek bir yük, bir zamanlama, bir bot profili, bir bilgisayar denetimi izni, mesaj ya da e-posta gönderen her şey) düğmesiz bir bildirim alır: ona tıklamak uygulamayı kartın üstünde açar. Bildirimin kendisine (bir düğmeye değil) tıklamak uygulamayı her zaman öne getirir.

Sınırlar: düğmeler yalnızca masaüstü uygulaması çalışırken işler (uygulama bir tepsi uygulamasıdır, yani pencereyi kapatmak sorun değildir, uygulamadan çıkmak sorundur). Onay yanıtlandıktan, süresi dolduktan ya da başka yerde karara bağlandıktan sonra Eylem Merkezi'nde bıraktığın bir bildirim zararsızdır: uygulama yeniden denetler ve onun yerine onaylar listesini açar. Windows küçük bildirimlerde düğmeleri bir menüde toplayabilir ve Odak Yardımı bildirimleri tutabilir.

**macOS ve Linux.** Bildirim eklentisi orada eylem düğmeleri gösteremez, bu yüzden bildirim düz metindir. Kartta yanıtlamak için uygulamayı aç.

**Sessiz saatler ve geçmiş.** Ayarlar > Genel > Bildirimler içinde sessiz saatler belirleyebilirsin (günler, başlangıç/bitiş, saat dilimi). O saatlerde hiçbir bildirim gösterilmez. Bildirim, kenar çubuğundaki **Bildirimler** listesinde sessizce saklanır (ne, ne zaman, hangi bot, gösterildi mi tutuldu mu, tıklayınca sohbet açılır). 22:00-07:00 gibi geceyi aşan bir pencere, başladığı güne aittir. Varsayılan olarak, bir botun takılı kaldığı onay ya da soru yine de araya girer. Her şeyi susturmak için bunu kapat. Bekleyen onay her zaman “Senden bekleyenler” altında durur. Liste son 100 kaydı 30 gün tutar, “Tümünü okundu say” ve “Geçmişi temizle” düğmeleri vardır ve yalnızca bildirimin söylediğini saklar (özel bildirimler açıkken yalnızca bot adı ve genel bir metin).

**Tek bir botu susturmak.** Bir botun panelindeki zil düğmesi, o botun “uzun çalışma bitti” ve “çalışma başarısız oldu” bildirimlerini her saatte susturur. Yine de **Bildirimler** listesine, “Tutuldu (bot sessizde)” işaretiyle düşerler. Bir susturma hiçbir zaman bir onayı ya da soruyu susturmaz: onlar yukarıdaki sessiz saat kuralına uyar ve her zaman “Senden bekleyenler” altında kalır. Sesi açmak için zile yeniden bas.

## Sohbetteki adım listesi

Bir çalışmanın araç çağrıları, sohbet sakin kalsın diye katlanır: aynı uygulamanın yinelenen çağrıları tek satır olur (“Playwright ×9”), biten bir çalışma tüm zaman çizelgesini açan tek bir özet satırı gösterir (“16 adım · Playwright · 2 dakika”) ve başarısız ya da reddedilen adımlar ile şu anda çalışan adım her zaman görünür. Bir satırı açınca tek tek adımlar zamanlarıyla görünür.

### Bir adıma kim izin verdi

Bir karar gerektiren adım, açılan satırın adım çizgisinde kararı kimin ya da neyin verdiğini söyler:

| Satır | Anlamı |
| --- | --- |
| Sen onayladın · çalışma başladıktan 3 sn sonra | Tam bu çağrının onay kartını sen yanıtladın. |
| Tam erişim — soru gerekmedi | Bot Tam erişimde ve çalışmayı sen başlattın, bu yüzden çağrı kartsız çalıştı. |
| Tam erişim, zamanlanmış çalışman — soru gerekmedi | Aynısı, senin kurduğun bir zamanlama, kural, webhook ya da postanın başlattığı çalışmada. |
| Tam erişim, görevin — soru gerekmedi | Aynısı, görevinin gözetimsiz bir oturumunda. |
| Dengeli düzey — soru gerekmedi | Dengeli düzey bu tür çağrıya izin verir. |
| Kalıcı iznin | Senin oluşturduğun bir kalıcı izin tam bu çağrıyı kapsadı. |
| Hatırlanan: … için her zaman izin ver | Daha önce bir kartta yaptığın seçim (her zaman, bu sohbet, bu görev) çağrıyı kapsadı. Satır aracı ya da kategoriyi söyler. |
| Site kuralın izin verdi | Site ya da uygulama başına kurallarından biri izin verdi. |

Satırı yalnızca aksi halde soracak çağrılar taşır: düzeyin zaten izin verdiği bir okuma hiçbir şey göstermez. Satır, daemon'un karara dair kendi kaydından gelir, bir sayfanın, postanın ya da aracın söylediği hiçbir şeyden gelmez. Bu kayıt var olmadan önce onaylanan bir adım düz “Onaylandı” gösterir. Daemon'un olay günlüğünde otomatik bir iznin onaylayanı hepsi sıfır olan sistem kullanıcı kimliğidir (`00000000-0000-0000-0000-000000000000`), asla senin kimliğin değildir; böylece `granted_by` alanını bilmeden günlüğü okuyan bir araç bunu senin tıklaman sanamaz. Bir kodlama motorunun kendi adımlarındaki çağrılar ile bir betikten gelen çağrıların satır iliştirilecek bir adımı yoktur.

### Tanılama zaman çizelgesini kaydetmek

Biten bir çalışmanın zaman çizelgesi metin (Markdown), web sayfası (HTML) ya da JSON dosyası olarak kaydedilebilir. Bir rapor kaydetmek yerel bir dosya indirir, çalışmayı hiçbir yere göndermez.

#### JSON biçimi, sürüm 4

```json
{
  "schema_version": 4,
  "outcome": "done",
  "duration_ms": 1400,
  "steps": [
    {
      "id": "step-1",
      "kind": "tool",
      "offset_ms": 100,
      "duration_ms": 250,
      "outcome": "done",
      "depends_on": null
    }
  ],
  "approval_events": [
    {
      "step_id": "step-1",
      "kind": "waiting_for_approval",
      "offset_ms": null,
      "outcome": null
    },
    {
      "step_id": "step-1",
      "kind": "approval_recorded",
      "offset_ms": 150,
      "outcome": "approved",
      "granted_by": "owner_card",
      "level": "balanced",
      "rule": null
    },
    {
      "step_id": "step-1",
      "kind": "tool_outcome",
      "offset_ms": 400,
      "outcome": "done"
    }
  ]
}
```

- Çalışmanın sonucu `done`, `failed`, `stopped` ya da `unfinished` olur. Çalışma süresi milisaniye cinsindendir, bilinmiyorsa `null`.
- Adım kimlikleri çalışmadan alınmaz, dışa aktarılan dosya için üretilir. Adımlar istendikleri zamana göre sıralanır, yalnızca sıra bir bağımlılık anlamına gelmez.
- Adım türleri `tool`, `question` ve `secret`'tır. Araç sonuçları `requested`, `awaiting_approval`, `running`, `denied`, `done`, `error` ya da `expired` olur. Sorular `answered` ya da `unanswered` kullanır, kimlik bilgisi istekleri `stored`, `cancelled` ya da `unanswered` kullanır. Tanınmayan bir araç sonucu `null` olur.
- `offset_ms`, çalışmanın başlangıcından itibaren ölçülür. İki adım süresi de milisaniye cinsindendir, yoksa ya da geçersizse `null`.
- `depends_on`, bir bağımlılık biliniyorsa dışa aktarmaya özgü adım kimliklerinin listesi için ayrılmıştır, bilinmiyorsa `null` olur. Şu anki zaman çizelgesi nedensel bağları tutmaz, bu yüzden alan `null`'dır. Bu, hiç bağımlılık olmadığı iddiası değildir.
- `approval_events`, dışa aktarmaya özgü `step_id` ile gruplanır. Gözlenen bir bekleyen kart, `null` sonuçlu `waiting_for_approval` verir. Sürüm 3 ve sonrası, kaydedilen bir kararı araç bittikten sonra bile `approval_recorded` (`approved` ya da `denied`) olarak tutar. Tamamlanan, kapıdan geçmiş bir çağrı ayrıca `tool_outcome` (`done` ya da `error`) verir. Karar zamanları, birinin tıkladığı anı değil, daemon olayının çalışmanın başlangıcına göre zamanını kullanır. Yoksa ya da geçersizse `null` olur.
- Sürüm 4 üç alan ekler. Daemon söylemediyse her biri `null` olur: `granted_by` (`owner_card`, `full_access`, `owner_schedule`, `owner_mission`, `balanced_level`, `standing_grant`, `remembered_choice` ya da `site_rule`), `level` (`ask`, `balanced` ya da `full`, çağrıya karar verildiği andaki botun düzeyi) ve `rule` (çağrıyı kapsayan hatırlanmış kural, `<kapsam>:<ad>`; kapsam `chat`, `always`, `category`, `task` ya da `standing`, ad `A-Z a-z 0-9 _ . -` karakterlerinden oluşan en çok 64 karakterlik bir araç ya da kategori adıdır, asla bir yol, adres ya da komut değildir; dışa aktarma bu biçimi yeniden denetler, uymayan her şey `null` olur). Alanlar `approval_recorded` olayındadır; o olay sürüm 3'teki anlamını korur: **bir kartı sen yanıtladın** (`granted_by` değeri `owner_card`'dır, ya da sürüm 4'ten önce kaydedilen bir onayda `null`: bir onaydı ama kaynağı iddia edilmez).
- Sürüm 4 ayrıca, kartın yerine otomatik bir kuralın izin verdiği çağrı için (Tam erişim, Dengeli düzey, kalıcı izin, hatırlanmış seçim, site kuralı) `automatic_grant` olay türünü ekler. `outcome` değeri `null`'dır, aynı üç alanı taşır (`granted_by` asla `owner_card` olmaz) ve öncesinde `waiting_for_approval` yoktur, çünkü kart yoktu. Bilerek ayrı bir türdür: sürüm 3 için yazılmış bir okuyucu `approval_recorded` olaylarını senin yanıtların sayar ve bunu doğru yapmaya devam eder. Böyle bir okuyucu tanımadığı olay türlerini yok saymalıdır; o zaman otomatik izinleri yalnızca görmez. Sürüm 3'ün hiç onay olayı vermediği bir adım (kart gösterilmemişti), daemon kaydettiyse artık bir `automatic_grant` da verir.
- Tanımadığı tür ve alanları yok sayan bir okuyucu için yalnızca `schema_version` değişti, yani bir sürüm 3 okuyucusu sürüm 4 dosyasında çalışmaya devam eder. Sürüm 3 dosyaları geçerli sürüm 3 dosyaları olarak kalır: yeni alanlar ve tür hiçbir zaman zorunlu değildir.
- Bir ret, yalnızca ilgili kapı gözlendiyse karar olarak bildirilir. Süre dolması ve politika vetosu sahibin reddi sayılmaz. Daemon nedeni metin olarak gönderir, bu yüzden sahibi süre dolmasına benzeyen ya da ayrılmış politika vetosu nedenine uyan bir neden yazarsa dışa aktarma ikisini ayırt edemez ve kararı atlar. Eksik bir olay, tutulmuş kanıt olmadığı anlamına gelir, olmadığının kanıtı değildir. Sürüm 3'te `approval_recorded` vardı ama üç kaynak alanı yoktu. Sürüm 2'de onay olayları vardı ama tamamlandıktan sonra bir onay tutulmuyordu. Sürüm 1'de `approval_events` yoktu.

JSON, bot adları, istemler, araç argümanları ve sonuçları, ek URL'leri ve hata iletileri dahil serbest metni ve iç kimlikleri içermez. Okunabilir adımlara ve temizlenmiş hata özetlerine ihtiyacın olduğunda metin ya da web sayfası raporunu kullan.
