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
| `/status` | Durumu gösterir |
| `/lang tr` ya da `/lang en` | Bu sohbetin dilini Türkçe ya da İngilizce yapar (tek başına `/lang` şu anki dili gösterir) |
| başka her şey | Seçili bota mesaj olarak gönderilir |

Bir bot onay istediğinde ya da bir soru sorduğunda, düğmeli bir mesaj alırsın. Çalışma, birine dokunana kadar
bekler (zamanlanmış çalışmalarda ise 30 dakikalık zaman aşımı onu duraklatana kadar).

## Dil

Kanalın kendi komutları, onay kartları, düğmeleri ve bildirimleri İngilizce ya da Türkçedir. Her sohbetin kendi
dili vardır ve bunu yeniden başlatmalarda korur; `/lang` istediğin an değiştirir. Botlarının yazdıkları, araç
çıktıları ve daemon mesajları çevrilmez.

- Sohbet dilleri gelmeden **önce** eşleştirilmiş bir sohbet ya da yapılandırmada listelenen bir kullanıcı,
  o sohbetteki biri `/lang` gönderene kadar İngilizce kalır. Bir güncelleme, bir onay kartının dilini asla kendiliğinden değiştirmez.
- Bundan sonra `/pair` ile eşleştirilen bir sohbet, Telegram uygulamasının diline göre Türkçe ya da İngilizce
  başlar; başka her uygulama dili İngilizceye döner. (Kanal masaüstü uygulamasının diline de dönebilir, ancak
  masaüstü uygulaması bunu henüz daemon'a iletmiyor.)
- Dille yalnızca düğme etiketleri değişir. Bir düğmenin ne yaptığı, karttaki bilgiler ve nelerin maskelendiği aynı kalır.
- Bazı sabit metinler (eşleştirme yanıtları, çalışma bitti bildirimleri) hâlâ yalnızca İngilizcedir.

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
