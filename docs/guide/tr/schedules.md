# Zamanlanmış işler, sade anlatımla

**Zamanlanmış iş**, bir botun senin seçtiğin zamanlarda kendiliğinden bir şey yapmasını sağlar:
"hafta içi her gün 9:00'da okunmamış e-postalarımı özetle". Bir kez kurarsın, sonra kendi kendine çalışır.
(Teknik başvuru: [Zamanlanmış görevler](../scheduled-tasks.md).)

## İlk işini oluştur

1. **Zamanlamalar**'ı aç ve **Yeni**'ye bas.
2. Hızlı başlamak için bir **örnek** seç (Günlük özet, Haftalık değerlendirme, Gelen kutusu ayıklama,
   Fiyat takibi, Yedek hatırlatması) ya da formu boş bırak. Örneğin doldurduğu her şeyi değiştirebilirsin.
3. **Ne sıklıkla** çalışacağını seç: *Bir kez*, *Her gün*, *Hafta içi*, *Her hafta*, *Her ay* ya da *Diğer*.
4. **Saati** seç (istenirse günü de). Seçicinin altında hangi saat diliminin kullanıldığını görürsün.
   Onun da altında işin çalışacağı **sonraki 5 zamanı** görürsün; böylece istediğin şey olup olmadığını kontrol edebilirsin.
5. Botun ne yapacağını kendi sözlerinle yaz.
6. Bunun hakkında ne zaman **haber verilmesini** istediğini seç, sonra **Oluştur**'a bas.

### Sözcüklerle yazmak

Zamanı kendin anlatmak için **Diğer**'i seç. İngilizce ve Türkçe çalışır; örneğin `every weekday at 9`,
`her gün 09:00`, `hafta içi her sabah 8`, `in 30 minutes`. Hiçbir şey bir yapay zekâ modeline tahmin ettirilmez:
sabit bir kurallar kümesi sözcüklerini okur. Neyin anlaşıldığını ve sonraki çalışma zamanlarını her zaman önce görürsün;
onaylamak için **Oluştur**'a yeniden bas. Sözcükler anlaşılmazsa bunu söyleyen bir mesaj alırsın; o zaman seçenek düğmelerini kullan.

### Gelişmiş (kapalı)

*Gelişmiş* bölümünde, cron bilenler için ham **cron ifadesi** ve **kaçırılan çalışma** ayarı bulunur.
Çoğu kişinin buna hiç ihtiyacı olmaz.

## Bilgisayar kapalıysa

İşler yalnızca Praxi-Bot uyanık bir bilgisayarda çalışırken çalışır. Bilgisayar kapalıyken ya da uykudayken
geçen zamanlar için şunlardan birini seçersin:

- **Atla** (varsayılan): bir sonraki planlı zamanı bekle.
- **Bilgisayar dönünce bir kez çalıştır**: kaç tane kaçırılmış olursa olsun tek bir telafi çalışması.
- **Telafi et**: kaçırılan her zamanı çalıştır, en çok 10.

Kaçırılan zamanlar işin geçmişinde gösterilir. Yeniden başlatma, aynı zamanı asla iki kez çalıştırmaz.

## Şimdi çalıştır, duraklat, düzenle, sil

- **Şimdi çalıştır** işi hemen başlatır; planlı zamanlar değişmez.
- **Duraklat** işi kaybetmeden durdurur; **Devam et** yeniden açar.
- **Düzenle** zamanlamayı, metni ya da bildirim seçimini değiştirir.
- **Sil** önce onay ister. Bir botu silmek, onun işlerini de siler.

## Geçmiş ve sorunlar

Her iş, son çalışmalarını sade bir durumla (bitti, başarısız, çalışıyor, kaçırıldı) ve çalıştığı sohbete giden bir
bağlantıyla listeler. Başarısız ya da kaçırılmış bir çalışmada **Yeniden dene** düğmesi vardır.
Ne olduğunu görmek için çalışmayı aç; hiç gelmeyen bir onayı bekliyorsa oradan devam ettirebilirsin.

## Sana haber vermek

Her iş için: **Her seferinde**, **Yalnızca bir şey ters giderse** (varsayılan) ya da **Hiçbir zaman**.
Bu, kullandığın bilgisayarda saklanır ve yalnızca uygulamanın neyi duyuracağını belirler. Bir iş senin onayına
ihtiyaç duyarsa her zaman haberdar edilirsin ve istek **Seni bekleyenler** bölümünde bekler.

## Güvenlik

Bir iş, botunun izinlerine tam olarak sahiptir, asla fazlasına değil. Onayına ihtiyaç duyan her şey devam etmek yerine
seni bekler; kimse yanıt vermezse güvenli bir şekilde zaman aşımına uğrar. İşin okuduğu web sayfaları, e-postalar ve
diğer dış metinler bilgi olarak ele alınır, asla emir olarak değil. Aylık harcama sınırı (Ayarlar) zamanlanmış
çalışmalara da uygulanır. İş oluşturmak, değiştirmek ve silmek yalnızca sahibine açıktır.
