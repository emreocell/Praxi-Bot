# Telegram

Botlarınla telefonundan konuş. Daemon bir Telegram kanalı çalıştırır: botuna mesaj yazarsın, yanıtın akışını
izlersin ve izin gerektiren işlemleri satır içi düğmelerle onaylar ya da reddedersin. Yalnızca **eşleştirdiğin**
Telegram hesapları bunu kullanabilir; diğer herkes geri çevrilir.

Eksiksiz kurulum başvurusu [docs/telegram.md](../../telegram.md) dosyasındadır (İngilizce). Kısaca:

```sh
bot channels telegram setup     # @BotFather'dan aldığın belirteci yapıştır (girdi gizli)
bot serve                       # kanal daemon ile birlikte başlar
bot channels telegram pair      # tek kullanımlık 6 haneli kod yazdırır (10 dakika geçerli)
```

Sonra Telegram'da botunu aç ve `/pair 123456` gönder (kendi kodunla).

## Sohbette

| Mesaj | Ne yapar |
| --- | --- |
| `/bots` | Botlarını listeler |
| `/use <bot adı ya da kimliği>` | Bu sohbetin hangi botla konuşacağını seçer |
| `/stop` | Şu anki çalışmayı durdurur |
| `/gorevler`, `/gorev <id>`, `/duraklat <id>`, `/surdur <id>`, `/durdur <id>`, `/gorev yeni <metin>` | Görevlerini telefondan yönet ([aşağıya bak](#görevleri-telefondan-yönet)); İngilizce adları da çalışır: `/missions`, `/mission`, `/pause`, `/resume`, `/stop`, `/mission new` |
| `/status` ya da `/durum` | Tek bakışta: sunucunun sağlığı, botların, şu an çalışanlar ve duruma göre görevler |
| `/help` ya da `/yardim` | Komutlar, İngilizce ve Türkçe |
| `/lang tr` ya da `/lang en` | Bu sohbetin dilini Türkçe ya da İngilizce yapar (tek başına `/lang` şu anki dili gösterir) |
| başka her şey | Seçili bota mesaj olarak gönderilir |

Bir bot onay istediğinde ya da bir soru sorduğunda, düğmeli bir mesaj alırsın. Çalışma, birine dokunana kadar
bekler (zamanlanmış çalışmalarda ise 30 dakikalık zaman aşımı onu duraklatana kadar).

## Görevleri telefondan yönet

Senin için gece gündüz çalışan bir bota uzaktan kumanda gerekir. Bu komutlar, botla olan kendi özel
sohbetinde, senin yazdığın mesajlarda çalışır:

| Mesaj | Ne yapar |
| --- | --- |
| `/gorevler` | Görevlerini listeler: açık olanlar önce; her birinin 8 karakterlik id'si, durumu ve `biten/toplam` sayısı |
| `/gorev <id>` | Tek görev: durum, sayılar, seni bekleyenler ve son rapor (rapor yoksa en son sonuçlar) |
| `/duraklat <id>` | Duraklatır: çalışan oturum durur, yenisi başlamaz |
| `/surdur <id>` | Duraklamış ya da seni bekleyen görevi sürdürür (botun hâlâ Tam (Full) düzeyde olması gerekir) |
| `/durdur <id>` | Görevi kalıcı olarak durdurur. Bot **önce sorar**; **Durdur** düğmesine (ya da **Devam etsin**'e) dokunana kadar hiçbir şey olmaz. Soru 5 dakikada geçer ve bir kez çalışır |
| `/gorev yeni <metin>` | `<metin>` hedefiyle yeni görev başlatır; bu sohbetin konuştuğu botta (bkz. `/bots`, `/use`). Her görev gibi bot **Tam (Full) düzeyde** olmalı |
| `/durum` | Sunucu sağlığı, botların, şu an çalışan sayısı; çalışan, seni bekleyen ve duraklatılmış görevler |

İngilizce adları da çalışır: `/missions`, `/mission <id>`, `/pause`, `/resume`, `/stop <id>`,
`/mission new <text>`, `/status`, `/help`. Tek başına `/durdur` (ya da `/stop`) bu sohbetin çalışmasını durdurmaya
devam eder; `/durdur <id>` görev komutudur.

**id**, `/gorevler` listesindeki 8 karakterdir (görev id'sinin sonu; başı zaman damgası olduğundan aynı anda
başlayan görevler birbirine benzer). Büyük/küçük harf ve tireler önemli değil; id'nin daha uzun bir parçası ya da
tamamı da olur. İki göreve uyuyorsa daha fazla karakter istenir.

Nasıl güvende tutulur:

- **Yalnızca sen.** Komut, eşleştirdiğin Telegram hesabından ve onun özel sohbetinden gelmeli. Başkasından, bir
  gruptan ya da başka yerden **iletilmiş** mesajlar geri çevrilir (iletilen mesaj, başkasının sözünü senin adınla
  taşır). Bir düğme, ancak bu botun az önce sorduğu bir soruda senin dokunuşunla geçerli olur.
- **Tek başına komut hiçbir şeyi durdurmaz.** Durdurma geri alınamaz, bu yüzden düğme ister. Duraklatma ve sürdürme
  geri alınabilir olduğundan hemen çalışır.
- **Sınırlar.** Dakikada en fazla 20 komut ve 10 dakikada 3 yeni görev; fazlasında bot bunu bir kez söyler, süre
  dolana kadar gerisini yok sayar.
- **Yeni görev, başka sohbet metinleri gibidir.** Hedefi, eşleşmiş sahip tarafından Telegram'da yazılmış diye
  etiketlenir (buradan gelen sohbet mesajıyla aynı konum; uygulamada başlatılan görevin bir altında), yapıştırdığın
  bir gizli bilgi gönderilmeden önce kesilir ve görev botun kendi sınırları, bütçeleri ve onay kurallarıyla çalışır.
  Para, e-posta gönderme ve gizli bilgiler yine **uygulamada** sorar (bir ödemeyi PIN'inle
  [telefondan da onaylayabilirsin](#bir-ödemeyi-telefondan-onaylamak)).
- **Kaçış.** Görev başlıkları, nedenler ve sonuçlar botun (ve onun üzerinden web sayfalarının) sözleridir. Bağlantılar,
  anmalar ve `/komutlar` zararsız hâle getirilerek alıntı olarak gösterilir.
- Yanıtlar daemon'ın kendi hata metnini tekrarlamaz; ne yapacağını söyleyen kısa bir satır alırsın.

## Bir ödemeyi telefondan onaylamak

Ödeme, Tam seviyede botun hâlâ sorduğu tek şeydir. Sohbetteki ödeme kartında hiçbir zaman düğme olmaz:
**para tek başına sohbetten asla onaylanamaz**, kendi eşleştirdiğin hesaptan bile. Masaüstünden uzaktaysan
yine de telefonundan karar verebilirsin; bunun için Telegram'dan hiç geçmeyen ikinci bir doğrulama gerekir.

Bunu **Ayarlar > Bildirimler > Ödemeleri telefondan onayla** bölümünden aç:

1. 6 ile 12 rakamlık bir **PIN** seç (telefonunun kilit kodu olmasın). Daemon yalnızca özetini saklar.
2. Bu bilgisayardaki daemon'a giden bir **tünelin herkese açık https adresini** gir;
   [gelen webhook'lardaki](incoming-webhooks.md) gibi bir tünel (örneğin Cloudflare Tunnel). Daemon yine
   yalnızca `127.0.0.1` üzerinde dinler ve yalnızca `Host` değeri yerel bir ad olan isteklere yanıt verir:
   tüneli `Host: 127.0.0.1` gönderecek şekilde ayarla (Cloudflare Tunnel için `httpHostHeader: 127.0.0.1`).

Bundan sonra sohbetteki ödeme kartında düğme yerine bir bağlantı olur:

- Bağlantı **bir kez**, **10 dakika** boyunca ve yalnızca o ödeme için çalışır. Aynı ödeme için yeni bir kart
  eski bağlantının yerini alır.
- Sayfa önce satıcıyı, siteyi, tutarı ve botun ne istediğini gösterir, sonra **PIN**'ini sorar. Ödeme
  sayfasının ekran görüntüsünü ve **Bu ödemeyi onayla** ile **Reddet** düğmelerini ancak doğru PIN'den sonra
  gösterir. Onay yalnızca bu tek ödemeye izin verir; asla kalıcı bir izin değildir.
- Satıcı, tutar ve ekran görüntüsü ödeme sayfasının kendi gösterdiğidir ve bir sayfa yalan söyleyebilir.
  **Site** ise botun tarayıcısının açık olduğu adrestir: onu kontrol et.
- Üç yanlış PIN bir bağlantıyı yakar. 15 dakika içinde beş yanlış PIN, telefondan onayı 15 dakika kilitler
  (ayarlar ne zamana kadar olduğunu gösterir).
- Bağlantı kullanıldığında, süresi dolduğunda, uygulamada karar verdiğinde, PIN'i değiştirdiğinde ya da
  telefondan onayı kapattığında çalışmaz olur.
- **PIN'i yazmadan önce adresi kontrol et.** Sohbeti ele geçiren biri benzer görünen bir sayfaya bağlantı
  gönderebilir. Onayı veren, gerçek sayfaya yazdığın PIN'dir.

PIN yoksa (varsayılan) ödemeler eskisi gibi yalnızca masaüstü uygulamasında onaylanır. Tasarım ve tehdit
modeli [telefondan onay ADR'sinde](../../design/phone-money-approval-adr.md) (İngilizce).

## Dil

Kanalın kendi komutları, onay kartları, düğmeleri ve bildirimleri İngilizce ya da Türkçedir. Her sohbetin kendi
dili vardır ve bunu yeniden başlatmalarda korur; `/lang` istediğin an değiştirir. Botlarının yazdıkları, araç
çıktıları ve daemon mesajları çevrilmez.

- Sohbet dilleri gelmeden **önce** eşleştirilmiş bir sohbet ya da yapılandırmada listelenen bir kullanıcı,
  o sohbetteki biri `/lang` gönderene kadar İngilizce kalır. Bir güncelleme, bir onay kartının dilini asla kendiliğinden değiştirmez.
- Bundan sonra `/pair` ile eşleştirilen bir sohbet, Telegram uygulamasının diline göre Türkçe ya da İngilizce
  başlar; başka her uygulama dili İngilizceye döner. (Kanal masaüstü uygulamasının diline de dönebilir, ancak
  masaüstü uygulaması bunu henüz daemon'a iletmiyor.)
- Kanalın kendi sözleri dile göre değişir: yanıtlar, düğme etiketleri, kart başlıkları ve bildirimler. Bir düğmenin ne yaptığı, karttaki bilgiler ve nelerin maskelendiği aynı kalır.
- Onay kartının ayrıntı bölümünü (tam olarak ne olacağı; `DAEMON SUMMARY:` ile başlayan satırla biter) daemon İngilizce yazar ve henüz çevrilmez. Anlayamıyorsan kararı masaüstü uygulamasında ver.
- Eşleştirilmemiş hesaplara giden ret metni ve `/pair` ile eşleşmeden önceki hatalar bilerek İngilizcedir: kanal henüz kimin dilini bilmiyor.

## Bilmen gerekenler

- Kendi Telegram botunu BotFather ile oluştur; belirteç senindir. Praxi, Telegram'ı kendi makinenden sorgular,
  bu yüzden gelen port ya da herkese açık adres gerekmez.
- Yanlış eşleştirme kodları reddedilir ve birkaç yanlış denemeden sonra eşleştirme bir süre kilitlenir.
  Eşleşmemiş hesaplardan gelen mesajlar sabit bir metinle reddedilir ve hız sınırına tabidir.
- Telegram'a gönderilen her şey, bildiği gizli değerleri (bot belirtecin dahil) maskeleyen gizli bilgi
  temizleyicisinden geçer.
- `bot channels telegram status`, kanalın yapılandırılıp yapılandırılmadığını, etkin ve çalışır olup olmadığını
  ve kaç kullanıcının eşleştirildiğini gösterir. `bot channels telegram
  disable` onu bir sonraki `bot serve`'de kapatır; belirteç saklanmaya devam eder.
- Telegram'dan başlatılan çalışmalar, seçili botun sıradan çalışmalarıdır ve masaüstünde başlattığın
  çalışmalar gibi o botun izin kurallarına uyar. (Politika modelinde ayrı bir `channel` başlatıcısı var, ancak
  henüz Telegram çalışmalarına atanmıyor.)
- Telegram mesajları Telegram'ın sunucularından geçer. Telegram üzerinden göndermeyeceğin hiçbir şey için kullanma.
