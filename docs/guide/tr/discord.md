# Discord

Botlarınızla Discord'dan konuşun: kendi bot hesabınızla özel mesajda ya da izin
verdiğiniz tek bir sunucu kanalında. Botu yalnızca **sizin** Discord hesabınız
kullanabilir: diğer herkes yok sayılır (özel mesajda tek bir nazik satır alır).

## Bağlayın (yaklaşık 5 dakika)

```sh
bot channels discord setup     # adımları anlatır, belirteci (gizli) ister, sonra sınar
bot serve                      # kanal, arka plan hizmetiyle birlikte başlar
bot channels discord pair      # tek kullanımlık 6 haneli kod verir (10 dakika geçerli)
```

1. <https://discord.com/developers/applications> adresini açın ve **New Application**'a basın.
2. **Bot** bölümünü açın, **Reset Token**'a basın ve belirteci kopyalayın.
3. Aynı sayfada **Privileged Gateway Intents** altında **Message Content Intent**'i açın. Kapalıysa Discord bota boş mesaj gönderir.
4. `bot channels discord setup` komutunu çalıştırıp belirteci yapıştırın. Gizli bilgi deponuza kaydedilir ve bir daha gösterilmez.
5. Kurulum bir bağlantı sınaması ve davet bağlantısıyla biter. Bu bağlantıyla botu yönettiğiniz bir sunucuya ekleyin; ardından üye listesinden profilini açıp **Mesaj** seçeneğini seçin.
6. Discord'da botunuzla **özel mesaj** açın ve `!pair 123456` (kendi kodunuz) yazın.

Kurulum adımlarını ve varsayılan cevapları Türkçe görmek için `bot channels discord setup --lang tr` kullanın. Daha sonra `!lang` ile değiştirebilirsiniz.

`bot channels discord test` bağlantı sınamasını istediğiniz zaman tekrarlar.

## Bota kimler yazabilir

- Eşleştirme yalnızca bot ile özel mesajda ve yalnızca bilgisayarınızda oluşturduğunuz kodla çalışır. Sunucuya yazılan kod hiçbir işe yaramaz; yenisini oluşturun.
- Eşleşen hesap **sahiptir**. İzin verdiğiniz sunucu kanalında bile başka hiç kimseye hizmet verilmez. Başka bir hesaptan gelen geçerli yeni kod sahibi değiştirir ve izinli sunucu kanallarını unutturur.
- Botlar ve diğer uygulamalar yok sayılır.
- Yanlış kodlar reddedilir; birkaç yanlıştan sonra bir süre kilitlenir.

## Sohbette

Komutlar `!` ile başlar (`/` işareti de düz metin olarak gönderilirse çalışır).

| Mesaj | Ne yapar |
| --- | --- |
| `!bots` | Botlarınızı listeler |
| `!use <ad>` | Bu sohbetin konuşacağı botu seçer |
| `!stop` | Çalışan görevi durdurur |
| `!gorevler`, `!gorev <id>`, `!duraklat <id>`, `!surdur <id>`, `!durdur <id>`, `!gorev yeni <metin>` | Görevlerinizi yönetin ([aşağıya bakın](#görevleri-telefondan-yönetin)); İngilizce adları da çalışır: `!missions`, `!mission`, `!pause`, `!resume`, `!stop`, `!mission new` |
| `!status` ya da `!durum` | Sunucu sağlığı, botlarınız, çalışan görevler, duruma göre görevler ve onayınızı bekleyenler |
| `!lang en`, `!lang tr`, `!lang de` | Botun kendi mesajlarının dili |
| `!allow` / `!disallow` | Sunucu kanalında: botun orada size cevap vermesine izin ver / vazgeç (yalnızca sahip) |
| `!help` | Kısa yardım |
| diğer her şey | Seçili bota gönderilir |

## Görevleri telefondan yönetin

Botla olan **özel sohbetinizde** görevleri yalnızca siz listeleyebilir, okuyabilir, duraklatabilir, sürdürebilir,
durdurabilir ve yeni görev başlatabilirsiniz. Komutlar [Telegram](telegram.md#görevleri-telefondan-yönet) ile aynıdır,
başına `!` gelir:

| Mesaj | Ne yapar |
| --- | --- |
| `!gorevler` | Görevlerinizi 8 karakterlik id'leri, durumları ve `biten/toplam` ile listeler |
| `!gorev <id>` | Ayrıntılar ve son rapor |
| `!duraklat <id>`, `!surdur <id>` | Duraklatır ya da sürdürür |
| `!durdur <id>` | Görevi kalıcı olarak durdurur; **Durdur** düğmesiyle (ya da **Devam etsin** ile) onaylarsınız |
| `!gorev yeni <metin>` | Bu sohbetin kullandığı botta görev başlatır; botun Tam (Full) düzeyde olması gerekir |
| `!durum` | Sağlık, botlar, çalışanlar, görevler |

İngilizce adları da çalışır: `!missions`, `!mission <id>`, `!pause`, `!resume`, `!stop <id>`, `!mission new <text>`,
`!status`. Tek başına `!stop` çalışan görevi durdurmaya devam eder.

Bu komutlar izin verdiğiniz bir **sunucu kanalında çalışmaz** (özel sohbeti kullanmanız söylenir) ve Discord'da
iletilen mesajlar yok sayılır. Dakikada en fazla 20 komut ve 10 dakikada 3 yeni görev. Discord'dan başlattığınız bir
hedef, uygulamada yazılmış gibi değil, bir Discord sohbet mesajı gibi sayılır: içinde andığınız siteler önceden
onaylanmış olmaz. Bir görevin yazdıkları, Discord biçimlendirmesi, bağlantıları ve anmaları zararsız hâle
getirilerek gösterilir.

İzin verilen bir sunucu kanalında yazdığınız **her** mesaj bota gider; bu yüzden
yalnızca bunun için ayırdığınız bir kanal seçin.

## Onaylar

Bir bot onayınıza ihtiyaç duyunca **Bir kez onayla** ve **Reddet** düğmeleri olan
bir mesaj gelir. Düğmelere yalnızca siz, yalnızca kartın göründüğü sohbette, bir
kez ve 10 dakika içinde basabilirsiniz.

- Düğmeler yalnızca tam metni mesaja sığan düşük riskli istekler için görünür. Geri kalanı (riskli işlemler, gizli bilgi içeren istekler, çok uzun istekler) bunu söyler ve bilgisayarınızdaki uygulamayı bekler.
- Burada "her zaman izin ver" yoktur. Kalıcı izinler uygulamada verilir.

## Mesajlar ve ekler

- Uzun cevaplar birkaç mesaja bölünür ve yaklaşık saniyede bir güncellenir. Son tamamlanmamış sözcük, gizli bilgileri güvenle maskeleyebilmek için bekletilir.
- Bot kimseyi etiketlemez: cevaptaki `@everyone`, `@here` ile kullanıcı ve rol etiketleri etkisiz hale getirilir.
- Ekler **indirilmez ve açılmaz**. Bot yalnızca belirli adda ve türde bir dosya gönderildiğini öğrenir ve bu güvenilmez olarak işaretlenir. Bunun yerine metni gönderin.
- Discord'a giden her şey, bildiği gizli bilgileri (bot belirteci dahil) gizleyen filtreden geçer.
- Bot Discord'un hız sınırlarına uyar ve Discord isteyince bekler.

## Bilmekte fayda var

- Discord uygulamasını kendiniz oluşturursunuz, belirteç size aittir. Bot bilgisayarınızdan dışarı bağlanır; açık port gerekmez.
- Discord mesajları Discord sunucularından geçer. Discord üzerinden göndermeyeceğiniz hiçbir şey için kullanmayın.
- `bot channels discord status` kanalın kurulu, açık ve çalışıyor olup olmadığını ve bir sahibin eşleşip eşleşmediğini gösterir. `bot channels discord disable` sonraki `bot serve`'de kapatır; belirteç saklı kalır.
- Bot hiçbir şey söylemiyorsa: `bot channels discord test` ile belirteci ve Message Content Intent'i kontrol edin, kurulumdan sonra `bot serve`'i yeniden başlattığınızdan emin olun.

- Sunucu kanalını okuyabilen herkes botun cevaplarını da görebilir. Özel bir kanal seçin.
- `bot serve` hizmetini açık tutun. Kapalıyken gönderilen mesajlar sonradan alınmaz.
