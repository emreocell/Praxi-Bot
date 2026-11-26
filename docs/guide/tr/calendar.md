# Takvim (CalDAV)

Botlar bir CalDAV takviminde (Nextcloud, Radicale, Fastmail, iCloud, Synology ...) etkinlik okuyabilir,
ekleyebilir, değiştirebilir ve silebilir, takvimler arasında boş zaman bulabilir, yinelenen etkinlik
oluşturabilir ve insanları davet edebilir. Her çağrı, diğer araçlar gibi izin motorundan geçer.

## Kurulum

**Ayarlar > Posta ve web > Takvim** bölümünü aç:

1. Hizmetini seç (iCloud, Fastmail, Nextcloud, Google ya da Diğer). Adres senin için doldurulur.
2. Kullanıcı adını ve bir **uygulama parolasını** yaz (normal parolanı değil). Her hazır ayar, parolanın
   nereden oluşturulacağını anlatır.
3. **Bağlantıyı dene**'ye bas. Bulunan takvimler listelenir ve hizmetin davetleri kendisinin gönderip
   göndermediği söylenir. Botlarının kullanacağı takvimi seç.
4. **Takvimi ekle**'ye bas.

Parola gizli bilgi deposuna gider; hiçbir dosyaya yazılmaz ve bir daha gösterilmez. Araçlar bir sonraki
çağrıdan itibaren çalışır, yeniden başlatmak gerekmez. Bir takvimi yanındaki çöp kutusu simgesiyle
kaldırabilirsin; saklanan parolası da silinir.

Google, takvimler için uygulama parolalarını yalnızca bazı hesaplarda kabul eder. Bir Google hesabında
deneme başarısız olursa muhtemel neden budur.

Takvimleri `<veri klasörü>/calendar.toml` ile elle de ayarlayabilirsin (`name`, `url`, `username` ve gizli
bilgi deposu başvurusu olarak `password`, örneğin `calendar/home/password`); Ayarlar sayfası aynı dosyayı düzenler.

## Araçlar

| Araç | Etkisi | Notlar |
| --- | --- | --- |
| `calendar_list_events` | okuma | Bir aralıkla çakışan etkinlikler; yinelenen etkinlikler her tekrarıyla bir kez listelenir. Başlıklar ve notlar güvenilmeyen metindir. |
| `calendar_free_busy` | okuma | Tüm (ya da adı verilen) takvimlerde boş zamanlar, istenirse çalışma saatleri içinde. Okunamayan bir takvimi gizlemez, bildirir. |
| `calendar_create_event` | yazma | Etkinlik ekler; istenirse yinelenen (`recurrence`, bir RRULE) ve `attendees` ile. |
| `calendar_update_event` | yazma | Etkinliği yeniden adlandırır ya da taşır; yerini, notlarını, tekrar kuralını veya katılımcıları değiştirir. Yalnızca verdiğin alanlar değişir. |
| `calendar_delete_event` | yıkıcı | `uid` ile bir etkinliği siler; her zaman sorar. |

Tarihler `YYYY-MM-DD` (tam gün) ya da saat farkıyla RFC 3339 biçimindedir.

### Davetler

Katılımcı eklemek, katılımcısı olan bir etkinliği değiştirmek ya da birini çıkarmak, izin düzeyin ne olursa
olsun önce her zaman sana sorar. Takvim sunucusu CalDAV zamanlamasını destekliyorsa davetleri kendisi
gönderir. Desteklemiyorsa bot, e-posta hesabın üzerinden `.ics` ekli bir e-posta gönderir (Ayarlar >
Posta ve web > E-posta bölümünden bir hesap kur). Katılımcısı olmayan bir etkinlik kimseyi bilgilendirmez.

## Güvenlik

- URL `https` olmalıdır (düz `http` yalnızca geri döngü adresleri için: `localhost`, `::1` ya da `127.*`; örneğin bir test sunucusu).
- Yönlendirmeler asla izlenmez, böylece parola başka bir sunucuya gönderilemez.
- Sunucunun verdiği bir bağlantı, yalnızca yapılandırılmış takvimin içinde kalıyorsa değiştirmek ya da silmek için kullanılır.
- Modele dönen takvim metni güvenilmeyen veri olarak işaretlenir; onu okumuş bir çalışma, web sayfası ya da e-posta
  okumuş gibi "bulaşmış" sayılır.
- Takvim eklemek, denemek ve kaldırmak sahibin işlemidir: eşleşmiş bir masaüstü, bir sunucunun kimlik bilgilerini
  değiştiremez.
