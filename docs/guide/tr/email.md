# E-posta entegrasyonu

Praxi, botlarının e-postaları okumasını, aramasını, taslak hazırlamasını ve göndermesini
sağlayan IMAP ve SMTP araçlarıyla gelir.

## E-postayı nasıl açarsın

E-posta desteği iki şey ister:
1. Posta kutusunun parolasını (ya da uygulamaya özel parolasını) Praxi'nin gizli bilgi deposuna koy.
2. E-posta hesabını `<data_dir>/email.toml` dosyasında (ya da `providers.toml` içindeki `[email]` bölümünde) ayarla.

### 1. Parolanı kaydet

Posta parolaları düz metin yapılandırma dosyalarında asla saklanmaz. Bunlara `SecretRef` ile adıyla
başvurulur ve yalnızca bağlanırken gizli bilgi deposundan alınır:

```sh
bot secrets set email/work/password
```

İstendiğinde parolanı yaz ya da yapıştır.

### 2. Hesapları ayarla

Praxi veri klasöründe `email.toml` dosyasını oluştur ya da düzenle (kullanıcı başına varsayılan
veri klasörü ya da `--data-dir` / `BOT_DATA_DIR` ile verdiğin yol):

```toml
[[accounts]]
name = "work"
address = "alice@example.com"
username = "alice@example.com"      # isteğe bağlı: varsayılanı address değeridir
password = "email/work/password"   # 1. adımda kaydedilen SecretRef tutamacı

[accounts.imap]
host = "imap.example.com"
port = 993                        # varsayılan: 993 (örtük TLS)

[accounts.smtp]
host = "smtp.example.com"
port = 465                        # varsayılan: 465 (örtük TLS) ya da 587 (STARTTLS)
security = "implicit"             # "implicit" ya da "start_tls"
```

Birbirinden farklı `name` değerleriyle birden fazla hesap ayarlayabilirsin.

İstersen bir `[email]` bölümünü doğrudan `<data_dir>/providers.toml` içine de koyabilirsin:

```toml
[email]
[[email.accounts]]
name = "work"
address = "alice@example.com"
password = "email/work/password"

[email.accounts.imap]
host = "imap.example.com"

[email.accounts.smtp]
host = "smtp.example.com"
```

### 3. Daemonu başlat

Daemonu `bot serve` ile başlat:

```sh
bot serve
```

Gizli bilgi deposu kullanılabilir olduğunda daemon e-posta araçlarını bir kez kaydeder ve örneğin
şunu yazar:
```
email: 6 tool(s) registered (1 account(s))
```

Bir bota e-posta araçları yalnızca o botun kullanabildiği en az bir hesap varsa sunulur (aşağıya
bak). Yani hiç hesap ayarlanmadan araçları kaydetmek gözle görülür bir şey yapmaz.

## Dosya düzenlemeden hesapları yönetme

Daemon çalışırken hesapları masaüstü uygulamasında **Ayarlar, Posta ve web** bölümünden ya da
komut satırından ekleyebilir, değiştirebilir, deneyebilir ve kaldırabilirsin:

```sh
bot email add work alice@gmail.com --preset gmail --bot Atlas --password-env MAIL_PW
bot email list
bot email test work        # IMAP girişi + SMTP kimlik doğrulaması, hiçbir şey gönderilmez
bot email rm work
```

Yeni bir posta kutusunun varsayılan bir sahibi yoktur. `--bot <ad-ya-da-kimlik>` ver (yalnızca o bot
kullanabilir) ya da `--all-bots` ver (e-posta araçları olan her bot, sormadan okuyabilir).
Uygulama da aynı şeyi sorar: form “Yalnızca bir bot” ile açılır ve hiçbir bot seçili değildir,
“E-postası açık her bot”u seçersen bir uyarı görünür.

`gmail`, `outlook`, `yahoo` ve `icloud` için hazır ayarlar vardır. Başka her şey için
`--imap-host`, `--smtp-host` ve gerekirse `--imap-port`, `--smtp-port`,
`--smtp-security start_tls` ver. Parola, `--password-env` ile adı verilen değişkenden ya da
gizli bir istemden okunur, bayrakla asla alınmaz. Çoğu sağlayıcı bir uygulama parolası ister.

Değişiklikler `email.toml` dosyasına yazılır ve bir sonraki araç çağrısında etkili olur, yeniden
başlatmak gerekmez. Parola gizli bilgi deposuna `email/<account>/password` adıyla gider ve
API, CLI ya da uygulama onu asla geri vermez (hesap listesi yalnızca bir parola saklanıp
saklanmadığını söyler). Bir hesabın sunucularını ya da girişini değiştirmek parolayı yeniden
girmeni gerektirir. Böylece saklanan bir parola başka bir sunucuya yönlendirilemez.

Bot panelinde (sağ tarafta, “Kitaplık” sekmesi) seçili bot için bir **E-posta** bloğu bulunur:
botun kullanabileceği posta kutuları, bir “Bağlantıyı denetle” düğmesi (denetim başarısız olursa
bir sonraki adımı gösterir) ve bir “<bot> e-posta kullanabilsin” anahtarı. Anahtar, botun
`[tools] allow` / `exclude` listelerine `email_*` ekler ya da çıkarır ([Botlar ve profiller](bots-and-profiles.md)
sayfasında anlatılan isteğe bağlı açma); başka hiçbir şeye dokunmaz ve göndermek yine botun izin
kipine uyar.

O bloktaki her posta kutusu ayrıca **son postaları gösterebilir** (en yeni on başlık: konu,
gönderen, tarih; salt okunur, hiçbir şey okundu işaretlenmez ve hiçbir gövde çekilmez) ve
**deneme postası gönderebilir**. Deneme postası, posta kutusundan kendi adresine giden tek
bir sabit iletidir ve başka birine yöneltilemez. İkisi de sahip anahtarı isteyen
`GET /v1/email/accounts/{name}/messages` ve
`POST /v1/email/accounts/{name}/send-test` yollarını kullanır (masaüstü uygulamasında bu anahtar
vardır, çıplak bir API token'ında yoktur). Eşleştirilmiş bir ana makinede başlıkları yalnızca bir
masaüstü okuyabilir ve deneme postası ana makinenin kendisinden gönderilir.

### Bot başına bir posta kutusu

Bir hesap tek bir bota ait olabilir:

```toml
[[accounts]]
name = "atlas"
bot = "6f1c2a4e-...-uuid-of-the-bot"      # botun kimliği (`bot bots list`)
allowed_senders = ["alice@example.com", "@corp.example"]   # e-posta kanalı üzerinden bota kimler ulaşabilir
```

- Bağlanmış bir hesabı yalnızca o bot kullanabilir ve o botun e-posta araçları, `account`
  verilmediğinde onu varsayılan olarak kullanır. Onu adıyla isteyen başka bir bot, var olmayan bir
  ad için aldığı “bilinmeyen hesap” hatasının aynısını alır.
- `bot` alanı olmayan hesaplar, e-posta araçları açık her bot için eskisi gibi çalışmaya devam eder.
  Botlara e-posta araçları varsayılan olarak verilmez.
- Uygulamada bir hesabı bir bota bağladığında (`bot`, `shared_with` ya da `default_for`) bot
  e-posta araçlarını kendiliğinden alır: `email_*` onun `[tools] allow` listesine eklenir (ya da
  `exclude` listesinden çıkarılır) ve değişiklik profile `[tools.mail_auto]` olarak, ayrıca denetim
  günlüğüne kaydedilir. Ayarlar, Posta ve web bölümü “<bot> artık bu gelen kutusunu okuyup
  arayabilir; göndermeden önce her zaman sana sorar” yazısını ve bir “Kapat” bağlantısını gösterir.
  Son bağlamayı ya da hesabı kaldırmak, tam olarak eklenenleri geri alır, senin kendi eklediğin
  bir aracı asla. Araçları elle kapattıysan bir hesap düzenlemesi onları yeniden açmaz. Bir hesabı
  yalnızca uygulama (sahip anahtarıyla) bağlayabilir, bu yüzden bir bot kendine e-posta araçları
  veremez. Göndermede değişen bir şey yok: kimliğin gönderim kipi ve onay kartı geçerli.
- Bir botu silmek, ona bağlı hesapları daemonun onlar için sakladığı parolalarla birlikte
  **kaldırır**. Bu hesaplar asla paylaşılan hesaplara çevrilmez, çünkü bu her bota o posta
  kutusuna erişim verirdi. Saklanan bir parola o anda silinemezse adı
  `email-secret-cleanup.json` dosyasında tutulur ve bir sonraki açılışta silinir.
- “Tüm gizli bilgiler” seçeneğiyle alınan paket dışa aktarmaları `email/<account>/password`
  gizli bilgilerini içermez. Bir posta kutusunun parolası yalnızca adını verirsen dışa aktarılır.
- `allowed_senders`, e-posta kanalının kimin postasını bota iletebileceğini belirler. Liste
  varsayılan olarak boştur ve boş bir liste **hiçbir şeyi** iletmez (“herhangi bir gönderen” diye
  bir seçenek yoktur): en az kendi adresini yaz. Başka birinden gelen posta yok sayılır, sayılır ve
  metni olmadan günlüğe yazılır. Bota verilemeyen bir posta üç kez yeniden denenir, sonra
  atlanır (sayılır), böylece sonraki postaları engelleyemez. `max_per_poll` değerini aşan postalar
  okunmamış kalır ve bir sonraki denetimde gelir. Bu kuraldan önce açılmış ve listesi olmayan bir
  kanal ayarlarını korur ama hiçbir şey iletmez. Sen bir gönderen ekleyene kadar kanal kartında
  bir uyarı görünür. `email` türündeki bir zamanlama tetikleyicisi de aynı kurala uyar (ortak
  tek bir eşleştirici): `@` içeren bir giriş tam bir adrestir. `corp.com` ya da `@corp.com` o alan
  adı ya da onun bir alt alan adıdır (`mail.corp.com`, asla `corp.com.evil.test` ya da
  `evilcorp.com` değil). Yalnızca çıplak adres sayılır, görünen adın bir parçası asla sayılmaz ve
  “içerir” kipi yoktur. From başlığı doğrulanmaz (DMARC denetimi yok), bu yüzden gönderenin alan
  adı DMARC uygulamıyorsa sahtelenmiş bir sahip adresi geçer.
- “Bağlantıyı dene” eylemi, ayarlandığı gibi TLS üzerinden giriş yapar ve sunucu başına tek bir
  durum bildirir (`ok`, `unreachable`, `tls`, `auth_failed`, `protocol`, `timeout`). Hiçbir
  posta göndermez ya da okumaz ve sunucunun ne dediğini asla göstermez.

## Kullanılabilir araçlar

Açıldığında sekiz araç `Messaging` izin kategorisi altında kaydedilir (yedisi bağlayıcıdan,
`email_wait_for` ise yerleşik araçlardan):

| Araç | Etki | Açıklama |
| --- | --- | --- |
| `email_identities` | Okuma | Botun kullanabileceği posta kimliklerini (adresleri), varsayılanını ve her gönderim kipini listeler. Gizli bilgi içermez |
| `email_list_folders` | Okuma | IMAP LIST ile klasörleri ve özelliklerini listeler |
| `email_search` | Okuma | Gönderen/konu/ileti metni/tarih/okunmamış süzgeçleriyle iletileri arar, UID özetleri döndürür. API posta kutularında `include_junk` ile istenmeyen postalar da gelir |
| `email_read` | Okuma | BODY.PEEK kullanarak UID ile ileti içeriğini ve ek meta verisini okur |
| `email_mark_read` | Yazma | Bir iletide Seen bayrağını koyar ya da kaldırır |
| `email_download_attachment` | Yazma | Bir iletinin tek bir ekini (`email_read`in listelediği `id` ile) daemon'un ek klasörüne (`email-attachments/<hesap>/`) kaydeder. Yolu, boyutu, türünü ve sade bir dosya adını döndürür; metnini ise yalnızca baytları gerçekten metinse verir (HTML sayfası, bir insanın gördüğü metne indirgenir). Dosya bir yabancıdan gelir: güvenilmez sayılır ve bir ek kaydetmiş bir çalıştırma, yorumlayıcılar ve derleme araçları için Tam düzeyin kabuk muafiyetini artık almaz. Eki asla açmaz ya da çalıştırmaz |
| `email_draft` | Yazma | İletiyi taslaklar klasörüne taslak olarak kaydeder (SMTP'ye asla bağlanmaz) |
| `email_send` | Yıkıcı | Bir e-postayı SMTP ile hemen gönderir (**insan onayı gerektirir**). `reply_to_uid` ile (API posta kutularında `reply_to_message_id`) aynı konuşmaya yanıt verir: alıcı, `Re:` konusu ve `In-Reply-To`/`References` başlıkları asıl iletiden alınır, onay kartı tam olarak bunu gösterir; asıl ileti, sana gösterilen ileti değilse hiçbir şey gönderilmez. İletide göndericiden farklı bir `Reply-To` varsa `email_read` bunu gösterir ve yanıt oraya gider |
| `email_wait_for` | Okuma | Botun **kendi** gelen kutusunda ve istenmeyen postalar klasöründe beklenen TEK bir postayı (bir doğrulama postası) bekler. Gönderen, konu, zaman, kodlar ve izleyebileceği bağlantıları döndürür. Aşağıya bak |

## Doğrulama postasını beklemek

`email_wait_for`, botun bir kaydı bitirme yoludur: formu gönderdikten hemen sonra siteden gelen postayı ister ve açabileceği kodu ve bağlantıyı alır.

- **Çalışmayı tutmaz.** Bekleme arka planda en fazla 10 dakika sürer (`timeout_minutes`), ama bir çağrı en çok 30 saniye bekletir (`block_seconds`, en fazla 90). Henüz bir şey gelmediyse yanıt, bir `wait_id` ile `pending` olur: bot başka işe geçer (sıradaki site) ve sonra bu kimlikle yeniden sorar. Önceden gelmiş posta hemen bulunur. Durdur, çağrıyı ve arka plandaki beklemeyi bitirir. Daemon yeniden başlarsa bekleme kimlikleri unutulur; yeniden sormak, önceden gelmiş postayı yine bulur.
- **Bir bekleme, başlatan çalıştırmaya aittir.** `wait_id` (rastgele bir kimlik) yalnızca onu başlatan çalıştırmada, aynı bot ve aynı posta hesabı için işe yarar; aynı botun başka bir çalıştırması (örneğin zamanlanmış biri) onu ne katılabilir, ne alabilir, ne de durdurabilir. Bekleme, çalıştırmasıyla birlikte biter; bitmiş bir bekleme bir daha kullanılmaz: yeniden sormak baştan tarar.
- **İstenmeyen postalar klasörü de taranır.** IMAP'te sunucunun istenmeyenler klasörü (`\Junk` işaretli ya da adı Junk, Spam, Bulk, Önemsiz, Gereksiz olan) gelen kutusuyla birlikte kontrol edilir. AgentMail'de spam ve kimliği doğrulanmamış postalar da dahildir. İstenmeyenler klasöründeki postayı taklit etmek kolaydır; bu yüzden yanıt, postanın hangi `folder` içinde olduğunu ve `from_junk` olup olmadığını söyler (klasörü olmayan posta kutularında `null`).
- **İzleme bağlantılarının içine bir adım bakılır.** SendGrid, Mailgun, Mailchimp gibi hizmetlerin postaları gerçek bağlantıyı bir tıklama izleme yönlendirmesine sarar. Bilinen bu adresler için (yalnızca standart bağlantı noktasında https; posta başına en çok üç bağlantı) daemon tek bir düz `GET` isteği yapar (web getirmeleriyle aynı adres korumasından ve aynı yasak listelerinden geçer: özel, yerel ya da metadata adresi yok, vekil sunucu yok) ve yönlendirmenin nereye gittiğini okur, hedefi açmaz. Durdur bunu hemen bitirir. Onay kartı bunu söyler. Hedef sonra diğer bağlantılar gibi değerlendirilir: bota yalnızca tarayıcının bulunduğu sitedeki (ya da senin listelediğin bir alan adındaki) https bağlantı verilir.
- Yanıttaki her şey postadan gelir ve güvenilmeyen veridir.

## Botun araçlarının çalıştığını denetle

Bottan **`tool_self_check`** çalıştırmasını iste (“hangi araçların çalışıyor?”). Güvenli bir örnek çağrısı olan her araç için sabit, salt okunur bir çağrı yapar, ama **yalnızca botun sana sormadan çağırabileceği araçlar için** (botun göremediği, izinlerinin yasakladığı ya da onay gerektirecek bir araç `skipped` olur, hiç çağrılmaz ve onay kartı açılmaz). `ok`, `failed` (sade bir neden ve çözüm ipucuyla) ya da `skipped` (nedeniyle) bildirir; ayrıca bilgisayar hakkında evet/hayır bilgisi verir: veri klasörü var mı ve salt okunur değil mi, bir ev klasörü var mı, PATH'te bir kabuk var mı. Bir şeyi değiştiren aracı asla çağırmaz, web araması yapmaz, diske hiçbir şey yazmaz ve yol döndürmez. Yanıttaki her cümle uygulamanın sabit metnidir: bir posta sunucusunun, bir dosyanın ya da bir sayfanın bir hata hakkında söylediği hiçbir şey ona kopyalanmaz, posta ya da dosya içeriği de döndürülmez. Tarayıcı ve masaüstü denetimleri ile yazma sınamaları `bot doctor` içindedir.

## Birden fazla posta kimliği

**Ayarlar > Posta ve web** bölümünde birden fazla posta kutusu ekleyebilirsin (kendi IMAP/SMTP posta
kutun, botun için bir AgentMail adresi ve benzeri). Her biri için şunları seçersin:

- **Kimler kullanabilir**: herhangi bir bot ya da yalnızca seçtiğin botlar.
- **Varsayılan gönderici olduğu botlar**: hiçbir adres belirtmediklerinde ondan gönderen botlar. Bir botun tek bir varsayılanı olur.
- **Gönderim**: *Her seferinde bana sor* (varsayılan) ya da *Yalnızca taslak*. Yalnızca taslak olan bir adres, botun göndermek istediğini taslak olarak kaydeder ve asla göndermez. Bu her yolda geçerlidir: posta kanalı ondan yanıt kuyruğa almaz, bir onay ondan gönderim yaptıramaz ve onun için **Deneme postası gönder** reddedilir (deneme gerçek bir ileti gönderirdi).

Bot, kimliklerini `email_identities` ile listeler, görevden bir tanesini seçer (“iş adresimden gönder”)
ve hangisini kullandığını söyler. Onay kartı her zaman **From**, To, konu ve metni gösterir. Kartta
başka bir adres seçersen bottan aynı iletiyi o adresten göndermesi istenir. Böylece hiçbir şey
çıkmadan önce yeni kartı tam olarak okursun. Parolalar ve API anahtarları gizli bilgi deposunda
kalır ve modele asla gösterilmez.

Paylaşılan `agentmail.to` alan adı soğuk posta için uygun değildir. Tanıtım yazışmaları için kendi
alan adındaki bir adresi kullan.

## Kayıtlar: kendi adresi olan bir bot

Kendine ait bir posta kimliği olan bir bota (ona bağlanmış ya da onunla paylaşılan bir adres, örneğin
bir AgentMail adresi) her çalışmanın başında bu söylenir: adresi, kayıtlar için doğrulama postalarının
o gelen kutusuna geleceği ve hangi posta araçlarını kullanabileceği (blok, uygulamanın ayarlarından
kurduğu sabit bir metindir, asla bir sayfadan ya da postadan gelmez). Kendine ait kimliği olmayan bir
bot böyle bir blok almaz ve başka bir botun adresini asla görmez.

“Şu sitelere kaydol” görevinde bot şunları kullanır:

1. `account_register`: uygulama parolayı üretir (bot onu asla görmez), o site ve o bot için kasada
   saklar, botun adresiyle kaydolur (site bir kullanıcı adı istemedikçe giriş adı adrestir) ve hesabın
   **hangi posta kimliğiyle** kaydedildiğini not eder.
2. `email_wait_for`: form gönderildikten sonra bot doğrulama postasını bekler (en fazla 10 dakika;
   Durdur anında iptal eder). Botun verdiği süzgeçler (gönderen alan adı, konu, anahtar sözcük),
   gelen kutusunun döndürdüğü şeye uygulamanın içinde uygulanır: botun yazdığı hiçbir şey posta
   sunucusuna gönderilmez. Yanıt kısadır ve güvenilmeyen diye etiketlenir: gönderen, konu, zaman,
   tek kullanımlık kodlar ve bağlantılar. Bekleme, başladığı anda tarayıcının bulunduğu sayfaya
   bağlıdır; bu yüzden bot beklerken bir sonraki siteye geçebilir. Çalışma bitse bile bekleme saklanır
   (uzun bir iş oturumlara bölünür) ve botun sonraki oturumu onu yeniden alır. Yalnızca bekleme
   başlamadan kısa süre öncesinden gelen postalar sayılır; bir sitenin beklemesine verilmiş posta
   başka bir sitenin beklemesine verilmez, böylece tek gelen kutusuyla çok sayıda kayıt yapan bot
   postaları karıştırmaz.
3. Bir bağlantı, yalnızca `https` ise, içinde kullanıcı adı ya da IP adresi yoksa ve bekleme
   başladığında tarayıcının gerçekten bulunduğu sayfayla aynı kayıtlı alan adındaysa (tarayıcıdan sorulur, sayfadan ya da
   modelden değil) ya da Ayarlar > Posta ve web > Güvenli gönderim bölümünde listelediğin bir alan
   adındaysa (“Doğrulama e-postasının bağlantı verebileceği ek siteler”) **izlenebilir** olarak
   döndürülür. Diğer her bağlantı yalnızca ana makine adıyla “dış bağlantı, izlenmedi” diye listelenir.
   Düz `http` reddedilir.
4. Bot izlenebilir bir bağlantıyı `browser_open` ile açar, oturumun açıldığını doğrular ve bildirir.
   CAPTCHA, telefon ya da iki adımlı doğrulama adımları sana **Kontrolü devral** ile gelir. İzlenen tarif
   yerleşik `signup-to-site` becerisidir.

İzinler: botun **kendi** gelen kutusunu okumak (`email_search`, `email_read`, `email_list_folders`,
`email_wait_for`) ve `email_wait_for`'un izlenebilir diye döndürdüğü bir bağlantıyı açmak, **kartsız**
yalnızca **senin elle başlattığın** bir çalışmada ve **Tam** düzeyindeki bir botta çalışır. Posta,
webhook, zamanlama, kanal ya da içe aktarılmış bir sohbetle başlayan bir çalışma eskisi gibi sormaya
devam eder. Her botun kullanabildiği bir gelen kutusu (bağlanmamış bir hesap) ve kişisel posta kutun
asla “botun kendisininki” sayılmaz. Gönderme değişmez: `email_send` her zaman sorar ve yalnızca taslak
olan bir adres sadece taslak kaydeder.

## Onaylar ve güvenlik

- **Göndermek onay ister**: `email_send`, `Effect::Destructive` olarak işaretlenmiş dış bir etkidir.
  Dış alıcılara e-posta gönderir ve geri alınamaz. Standart izin hazır ayarlarında
  (`approve-destructive`, `read-and-draft`), motor postayı göndermeden önce her zaman açık bir onay ister.
- **Zorunlu TLS**: IMAP örtük TLS ister (993 kapısı). SMTP örtük TLS (465 kapısı) ya da zorunlu
  STARTTLS (587 kapısı) ister. Ana makine adı ve Mozilla kök sertifikalarına karşı sertifika
  doğrulaması her zaman uygulanır. Düz metin bağlantılar ve güvensiz sertifikalar reddedilir.
- **Güvenli kimlik bilgileri**: Parolalar asla ekrana yazılmaz ya da günlüklere yazılmaz. Kimlik
  bilgileri, bağlantı kurulurken gizli bilgi deposundan tembel biçimde çözülür.
- **Taslaklar**: `email_draft`, doğrudan IMAP taslaklar klasörüne kaydeder ve SMTP'ye asla bağlanmaz.

## Güvenli gönderim (gönderim günlüğü, çıkış listesi, sınırlar)

Bu korumalar modelde değil, kodda durur. Ayarlar > Posta ve web > **Güvenli gönderim** onları gösterir
ve düzenler. Yollar (`/v1/email/guard/*`) sahip anahtarı ister.

- **Gönderim günlüğü**: gerçekten gönderilen her e-posta kaydedilir (zaman, bot, çalışma, hesap,
  alıcılar, konunun başı, ileti kimliği, çıkış satırının eklenip eklenmediği, “Yine de gönder”i seçip
  seçmediğin). Uygulamada açabilir ve CSV olarak dışa aktarabilirsin. `<data dir>/email-guard/send-log.jsonl`
  içinde saklanır.
- **Çıkış listesi**: yazmayacağın adresler ya da alan adlarının tamamı (`email-guard/opt-out.json`).
- **Çıkış satırı**: uygulama bunu her `email_send` gövdesinin sonuna ekler (varsayılan metin Türkçe ve
  İngilizcedir, düzenlenebilir, `{sender}` = gönderen adres). Bot onu yazmaz ve kaldıramaz. Bir bot onu
  dışarıda bırakmayı isteyebilir (`opt_out_line: false`). Bu yalnızca onay kartının bir uyarı göstermesini sağlar.
- **Sınırlar**: hesap başına 24 saatte 20 e-postanın üstünde ve bir kişi son 14 gün içinde o hesaptan posta
  aldıysa uyarı. İki sayı da ayardır (0 uyarıyı kapatır).
- **Uyarılar engel değildir.** Bir gönderim çıkış listesine, bir sınıra ya da eksik çıkış satırına
  takıldığında onay kartı uyarıları listeler (ve bir kez, ticari postada çıkış isteğini yok saymanın
  yasa dışı olabileceği notunu, Türkiye'de 6563 sayılı Kanun, ETK) ve düğmesi **Yine de gönder**
  yazar. Ona yalnızca sen tıklayabilirsin ve bu kaydedilir. Bir model uyarıyı aşamaz: araç, bir
  uyarının ötesinde göndermeyi yalnızca, sana gösterilmiş bir kartla tam olarak eşleşiyorsa ve bir
  kez kabul eder.
- `email_send`, izin hazır ayarı ya da verilmiş izinler ne olursa olsun her zaman sorar.

## Bota kendi adresini ver (AgentMail)

Kendi posta kutunun yanı sıra bir bot, **senin kendi** AgentMail hesabın ve API anahtarınla
[AgentMail](https://agentmail.to) üzerinde oluşturulan `name@agentmail.to` gibi bir adrese sahip
olabilir. Praxi-Bot'un kendi anahtarı yoktur ve bir aktarım sunucusu çalıştırmaz.

- **Ayarlar, Posta ve web** bölümünde “Bu bota bir adres ver (AgentMail)” seçeneğini seç, API
  anahtarını yapıştır, botu seç (ya da açıkça herhangi bir botu) ve istersen adres için bir ad ver.
  Komut satırından:
  `bot email add --provider agentmail --name helper --bot Atlas --api-key-env AGENTMAIL_KEY`.
- Tek bir gelen kutusuna ait bir anahtar (`am_us_inbox_...`) gelen kutusu oluşturamaz. Aynı formda
  “Mevcut bir gelen kutusunu bağla”yı seç, o anahtarı ve gelen kutusunun adresini yapıştır: anahtar,
  o gelen kutusunun okunmasıyla denetlenir (`GET /v0/inboxes/{id}`), hiçbir şey oluşturulmaz ve hesap
  diğer her AgentMail hesabı gibi çalışır. API'de bu, `"inbox": "praxi@agentmail.to"` ile
  `POST /v1/email/accounts` çağrısıdır.
- Anahtar yalnızca yazılır: gizli bilgi deposuna `email/<account>/api_key` olarak gider (adını
  vermedikçe durum paketlerinin dışında kalır) ve yalnızca `Authorization` başlığında, HTTPS
  üzerinden `api.agentmail.to` adresine gönderilir. Yönlendirmeler asla izlenmez.
- `email.toml` içinde `kind = "agentmail"` ve `inbox_id` saklanır. `kind` alanı olmayan eski dosyalar
  eskisi gibi IMAP/SMTP hesaplarıdır.
- Aynı `email_*` araçları çalışır. Bir AgentMail hesabında iletiler IMAP `uid` yerine
  `message_id` ile tanımlanır (`email_search` gösterir). Klasörler ve taslaklar desteklenmez ve bunu söyler.
- Bir hesabı kaldırmak (ya da bir botu silmek) onu yalnızca yerelde unutturur. AgentMail'deki gelen
  kutusu yalnızca sen istediğinde silinir: `bot email rm <name> --delete-inbox` ya da uygulamadaki onay kutusu.
- Bu yazının hazırlandığı sırada ücretsiz plan 3 gelen kutusuna, ayda 3.000 e-postaya ve günde 100
  e-postaya izin veriyor. Bkz. [fiyatlandırma](https://agentmail.to/pricing). Paylaşılan alan adında
  soğuk tanıtım yazışması yasaktır.
