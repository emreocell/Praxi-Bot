# Zamanlanmış görevler

Bir **zamanlama**, botun kendi kendine bir şey yapmasını sağlar: hafta içi her sabah, her 15
dakikada bir, iki saat sonra bir kez. Zaman gelince daemon, bir sohbette normal bir çalışma
başlatır (aynı olaylar, aynı onaylar, aynı geçmiş). Zamanlamaların çalışması için daemonun açık
olması gerekir (`bot serve` ya da masaüstü uygulaması).

## Bir tane oluştur

```sh
bot schedules add <bot-id> "every weekday at 9" "Summarize my unread notifications"
bot schedules add <bot-id> "her gün 09:00" "Günlük raporu hazırla" --tz Europe/Istanbul
bot schedules add <bot-id> "in 2 hours" "Remind me to stand up" --name stretch
bot schedules list
bot schedules run-now <schedule-id>     # fire it now; the timer is unchanged
bot schedules rm <schedule-id>
```

`add` komutunun argümanları: bot kimliği, bir **ne zaman** ifadesi ve botun her seferinde
çalıştıracağı **istem**. Seçenekler: `--name` (varsayılan: istemin başı), `--tz` (`Europe/Istanbul`
gibi bir IANA saat dilimi, varsayılan UTC) ve `--chat` (çalışmaları bu sohbete gönder, verilmezse
zamanlamanın adını taşıyan bir sohbet oluşturulur ve hep o kullanılır).

Masaüstü uygulamasında aynı işi yapan bir Zamanlamalar ekranı vardır.

## Ne zaman ifadeleri

İngilizce ve Türkçe ifadeler anlaşılır. Bunları daemon çözer (`POST /v1/schedules/parse` sonucu
önceden gösterir). Çözücünün tablolarından alınmış örnekler:

| Tür | Örnekler |
| --- | --- |
| Aralık | `every minute`, `every hour`, `every 15 minutes`, `saat basi` |
| Günlük ve haftalık | `every day`, `every morning` (09:00), `every evening` (18:00), `every night` (22:00), `at midnight`, `at noon`, `every weekday`, `every weekend`, `every week`, `every monday at 8`, `her gün 09:00` |
| Aylık | `every month` (ayın 1'i, 09:00) |
| Tek seferlik, göreli | `in a minute`, `in half an hour`, `in 30 minutes`, `2 saat sonra`, `yarim saat sonra` |

Bir ifade anlaşılmazsa komut tahmin yürütmez, hata verir. API ayrıca 5 alanlı ham **cron**
ifadelerini (örneğin `0 9 * * 1-5`, istersen bir saat dilimiyle), saniye cinsinden sabit
aralıkları ve tam tek seferlik zaman damgalarını da kabul eder.

Takvim ifadeleri, `--tz` ile verdiğin saat dilimini kullanır. Vermezsen UTC kullanılır.

## Bir zamanlama ne çalıştırabilir

CLI, bota bir **istem** gönderen zamanlamalar oluşturur. API üzerinden (dolayısıyla masaüstü
uygulamasında da) bir zamanlamanın yükü şunlar da olabilir:

- bir **iş akışı** (modelsiz yeniden oynatılır, bkz. [İş akışları](workflows.md)) ya da
- doğrudan bir **araç** çağrısı.

Bunların hepsi normal izin kapısından geçer.

## Kalıcı talimatlar: yalnızca sana bir şey gerektiğinde konuşan kontrol

**Kalıcı talimat**, yükü bir kontrol listesi olan bir zamanlamadır ("yedek diski %90'ın altında
mı? bir müşteriden yanıtsız e-posta var mı?"). Her çalışma listeyi sırayla yürütür ve tam olarak
şu son satırla biter:

- `OK`: sana bir şey gerekmiyor. **Hiçbir bildirim oluşmaz**, ne uygulamada ne de bir sohbet
  kanalında; çalışmanın kendisi çalışma geçmişinde kalır.
- `NEEDS_OWNER(<neden>)`: sana bir şey gerekiyor. Nedenle birlikte bir "Senden bir şey gerekiyor"
  bildirimi oluşur ve iletilir (Telegram, Discord ve diğer kanallara; sahip bildirim ayarlarına ve
  sessiz saatlere göre).

Yalnızca son satır sayılır; raporun ortasındaki bir "OK" hiçbir şeyi değiştirmez. Açık bir `OK`
ya da `NEEDS_OWNER(...)` satırı olmadan biten çalışma da bildirilir ("Kontrol açık bir OK ya da
NEEDS_OWNER yanıtı olmadan bitti"): sessizliğin hak edilmesi gerekir. Neden, botun özel olarak
okuduğu bir şeyden (e-posta, dosya, hesap) geldiyse bildirim, onu bir sohbet servisinde
tekrarlamak yerine uygulamada açmanı söyler. Aynı neden bir saat içinde yeniden iletilmez.
Başarısız olan bir çalışma her zamanki gibi bildirilir.

```json
{
  "kind": "standing_order",
  "checklist": "Yedek diski %90'ın altında mı? Bir müşteriden yanıtsız e-posta var mı?",
  "active_hours": { "start": "08:00", "end": "22:00", "timezone": "Europe/Istanbul" },
  "max_runs_per_day": 12
}
```

Bunu herhangi bir `spec` ile (örneğin `{"every": {"seconds": 1800}}`) `POST /v1/schedules`
çağrısının `payload` alanı olarak gönder.

- `checklist` (zorunlu, en fazla 4.000 karakter, gizli karakter yok).
- `active_hours` (isteğe bağlı): bu günlük aralığın (`HH:MM` ile `HH:MM` arası, gece yarısını
  geçebilir, bitiş dahil değil, verilen IANA saat diliminde) dışındaki tetiklemeler çalışma
  başlatılmadan atlanır. Verilmezse talimat gün boyu çalışabilir.
- `max_runs_per_day` (varsayılan 24, 1 ile 288): **günlük maliyet sınırı**. Talimat bir günde
  (etkin saatlerin saat diliminde, yoksa UTC) bu kadar çalışma başlattıysa, o günün sonraki
  tetiklemeleri ertesi güne kadar atlanır. Atlanan tetiklemeler çalışma başlatmaz ve model
  çağırmaz, yani hiçbir maliyeti yoktur. Botun kendi harcama sınırları ayrıca geçerlidir.

Her çalışma kendi sohbetini alır (uzun bir geçmiş bir sonraki kontrolü pahalılaştırmasın diye) ve
kalıcı talimata soru sormaması söylenir: başında kimse yok. Onaylar, başında kimse olmayan her
çalışmada olduğu gibi işler.

## Webhooklar

Bir zamanlamanın, dışarıdan bir şeyin onu tetikleyebilmesi için bir webhook tetikleyicisi olabilir:
`POST /v1/webhooks/{id}` ve şu başlık:
`X-Signature-256: sha256=<hex HMAC-SHA256 of the body with the schedule's webhook secret>`.
Burada taşıyıcı token kullanılmaz, imza kimlik doğrulamanın kendisidir. Gövde (en çok 256 KB),
zamanlamanın istemindeki `{input}` yerine geçer. Bu yol, API'nin geri kalanı gibi yine yalnızca
loopback üzerindedir. Yani makinenin dışından erişmek için kendi kurduğun bir tünel ya da ters
vekil sunucu gerekir. Webhook gizli anahtarları API üzerinden ayarlanır (oluştururken ya da
güncellerken `webhook_secret`), CLI'de bunun için bir bayrak yoktur.

Zamanlamalar bir dosya değişikliğinde (`file_watch`) ya da eşleşen yeni bir e-postada (`email`)
da tetiklenebilir ve bir çalışma başlamadan önce deterministik bir ön denetim yapabilir. Bunlar
API üzerinden ayarlanır. Bkz. [Zamanlama tetikleyicileri ve deterministik ön
denetimler](../../schedule-triggers-prechecks.md).

## Kaçırılan çalışmalar

Bir zamanlamanın vakti geldiğinde daemon durmuşsa ya da bilgisayar uykudaysa, zamanlamanın
**kaçırma politikası** ne olacağını belirler: `skip` (varsayılan, kaçırılanları yok sayar),
`run_once` (kaçırılan aralık için bir kez çalıştırır) ya da `run_all` (kaçırılan her zamanı
çalıştırır, en çok 10). Formda bunlar “Atla ve bir sonraki zamanı bekle”, “Bilgisayar açılınca bir kez
çalıştır” ve “Yetiş: kaçan her zamanı çalıştır (en çok 10)” olarak görünür.

`run_once` ya da `run_all` ile bir **telafi penceresi** de ayarlayabilirsin (1 saat, 6 saat, 1
gün, 7 gün ya da sınırsız, API alanı `catchup_window_secs`). Pencereden daha uzun süre önce
kaçırılan bir çalışma geç başlatılmaz: günlük 08:00 işi 22:00'de çalıştırılmaz. Çalışma
**kaçırıldı** olarak kaydedilir, zamanlamanın geçmişinde **Şimdi çalıştır** düğmesiyle birlikte gösterilir
ve sessizce asla silinmez. Varsayılan sınırsızdır, bu yüzden mevcut zamanlamalar eskisi gibi
davranır.

## Kısa seri işler için zamanlama değil, tek bir çalışma kullan

Bir bot, 5 dakikadan sık tetiklenen bir zamanlama oluşturamaz (bu sınır botun oluşturduğu
zamanlamalar içindir, kendi zamanlamaların için sınırı sen belirlersin). “5 saniyede bir, 4 kez”
gibi bir iş için bot, eylemi tek bir çalışma içinde tekrarlar ve turlar arasında `wait` aracını
çağırır (çağrı başına 1 ile 300 saniye, bir çalışmada toplam 30 dakika; **Durdur** bir beklemeyi
hemen bitirir).

## Başında kimse yokken onaylar

Zamanlanmış bir çalışma, botun kuralında ayrı bir `schedule` ayarı yoksa izinler konusunda senin
gibi davranır. Onay gerekirse ve 30 dakika içinde yanıtlamazsan çalışma duraklar. Para harcayan ve “asla”
listesindeki eylemler her zaman sorar. İpucu: başında kimse yokken çalışacak zamanlamalara,
politikası yalnızca işin gerektirdiği şeylere izin veren bir bot ver.

Bir zamanlama aynı anda tek çalışma yürütür. Önceki tetikleme hâlâ sürüyorsa (örneğin bir onay
bekliyorsa) yeni tetikleme atlanır.

## Botun kendi zamanlamalarını yönetmesine izin vermek

Profili `schedule_*` araçlarına izin veren botlar (Genel hazır ayarı verir, mevcut profiller
değişmez) sohbette zamanlama isteyebilir (“hafta içi her gün 10:30'da bana hatırlat”). Araçlar
`schedule_list`, `schedule_create`, `schedule_update`, `schedule_pause`, `schedule_resume` ve
`schedule_delete`. Bir bot yalnızca kendi zamanlamalarını görür ve yalnızca kendi oluşturduklarını
değiştirebilir (güncelleyebilir, duraklatabilir, devam ettirebilir, silebilir). Senin uygulamada,
CLI'de ya da API'de oluşturduğun zamanlamalar onun için salt okunurdur (bir bot oluşturduysa
listede “Bot oluşturdu” olarak görünür). Bir bot, webhook tetikleyicisi olan bir zamanlamayı asla düzenleyemez
ya da devam ettiremez. Yeni bir zamanlama, istendiği sohbete yazar.

Zamanlama, sonrası için elinden verdiğin bir yetkidir. Bu yüzden **oluşturma, güncelleme ve devam
ettirme her zaman sana sorar**, politika ne derse desin (bunları reddeden bir politika yine
reddeder). Kart tam istemi, zamanlamayı, saat diliminde sonraki üç tetikleme zamanını ve botun başında
kimse yokken ne yapabileceğini gösterir (güncellemede eski ve yeni yan yana). Onayladığın şey
tam olarak saklanan şeydir: “2 saat sonra” gibi bir zaman, kart gösterildiği anda sabitlenir ve
bu arada zamanlama değiştiyse değişiklik reddedilir. İstemler 4.000 karakterle, adlar 80
karakterle sınırlıdır. Kontrol karakterleri, metin yönü geçersiz kılıcıları ve görünmez
karakterler (sıfır genişlikli karakterler, yumuşak tire, satır ve paragraf ayırıcıları, varyasyon
seçiciler, etiket karakterleri) reddedilir. Böylece başında kimse yokken çalışan bir isteme
gizli bir metin sızamaz. **Benzer görünen harfler (homoglifler) süzülmez**, istemi kartta oku.
“Her zaman izin ver” ve “bu sohbet” izinleri bu üç işlem için asla geçerli olmaz ve daemon, bu
tür bir isteği hangi istemciden gelirse gelsin (masaüstü, HTTP, Telegram, CLI) reddeder. Gizli
bilgiye benzeyen bir şey içeren istek (bir anahtar ya da token) reddedilir, çünkü kart, zamanlamanın
saklayacağı metni maskelemiş olurdu. Telegram ve CLI de ayrıntıların tamamını gösterir (istem
başlangıç ve bitiş satırları arasında alıntılanır, ardından daemonun yazdığı bir özet satırı
gelir). Bir istemci bunları gösteremiyorsa onayı masaüstü uygulamasında ver.

Bir botun oluşturduğu zamanlamayı duraklatmak ve silmek, normal onay kurallarına uyar. Bot
tarafından oluşturulmuş bir zamanlamayı uygulamada düzenlersen o artık senin olur: bundan sonra bot
onu okuyabilir ama değiştiremez. Bot, kendi oluşturduğu ve senin yeniden yazmadığın zamanlamaları
yine duraklatabilir ya da silebilir.

Sınırlar (`<data dir>/schedule.toml` içinde, hepsi isteğe bağlı):

```toml
max_per_bot = 20          # schedules per bot, whoever made them
min_interval_secs = 300   # shortest gap between firings of a bot-made schedule
timezone = "Europe/Istanbul"   # default for "9:00"; else the computer's, else UTC
```

`max_per_bot` 1 ile 1000, `min_interval_secs` 1 ile 604800 arasında olmalıdır. Dosya bozuksa ya
da anlamsız bir değer içeriyorsa zamanlama araçları kapatılır (varsayılanlara asla dönmezler) ve
`bot doctor` bunu bildirir.

Bir botun yaptığı her oluşturma, güncelleme, duraklatma, devam ettirme ve silme işlemi, bot
kimliği ve çalışma kimliğiyle denetim günlüğüne yazılır (güncellemeler eski istemin uzunluğunu ve
SHA-256 özetini de kaydeder). Kayıt, değişiklikten önce yazılır. Değişiklik sonra başarısız olursa
arkasından bir `schedule.failed` kaydı gelir. Denetim kaydının yazılması başarısız olursa hiçbir şey
değiştirilmez.
