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
kaldırabilirsin; Ayarlar'ın kaydettiği parola da silinir (`calendar.toml` içinde elle verdiğin bir gizli bilgi
başvurusuna dokunulmaz, çünkü başka bir şey onu kullanıyor olabilir).

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
| `calendar_delete_event` | yıkıcı | `uid` ile bir etkinliği siler. Geri alınamaz; bu yüzden alt izin düzeyleri önce sorar. Etkinliği sen düzenliyorsan ve sunucu katılımcılar için iptal gönderiyorsa, onlar bilgilendirilir. |

Tarihler `YYYY-MM-DD` (tam gün) ya da saat farkıyla RFC 3339 biçimindedir.

### Davetler

Katılımcı eklemek, katılımcısı olan bir etkinliği değiştirmek (herhangi bir değişiklik, bir not bile:
sunucu herkese haber verir) ya da etkinliği silmek başka insanlara posta gönderir; bu yüzden e-posta
göndermekle aynı onay adımından geçer. Alt izin düzeyleri önce sana sorar; **Tam erişim** güvenilen bir
çalışmanın geçmesine izin verir (para harcamadan önce yine sorar). Her biri denetim kaydına yazılır.

Takvim sunucusu CalDAV zamanlamasını destekliyorsa davetleri kendisi gönderir. Desteklemiyorsa bot,
e-posta hesabın üzerinden `.ics` ekli bir e-posta gönderir (Ayarlar > Posta ve web > E-posta bölümünden
bir hesap kur). Katılımcısı olmayan bir etkinlik kimseyi bilgilendirmez. Giden posta kuralların her iki
durumda da geçerlidir ve hiçbir şey yazılmadan önce uygulanır: engelleme listendeki bir adres ya da günlük
sınırın aşılması değişikliği durdurur; gönderim günlüğü her daveti, gönderen botla birlikte gösterir.

## Güvenlik

- URL `https` olmalıdır (düz `http` yalnızca bu bilgisayar için: `localhost`, bir `*.localhost` adı ya da
  `127.0.0.1` veya `::1` gibi bir geri döngü adresi; örneğin bir test sunucusu).
- Bulut meta veri ve bağlantı-yerel (link-local) aralığındaki adreslere (örneğin `169.254.169.254`) ve
  daemon'un kendi portuna, hangi ad oraya çıkarsa çıksın, asla bağlanılmaz. Kendi ağın serbesttir; bu
  yüzden evdeki bir sunucudaki takvim çalışır.
- Bir sunucu, daemon'u yalnızca senin yazdığın sunucu üzerindeki başka bir takvim adresine yönlendirebilir
  (takvimlerini kendi sunucuları arasında dağıtan iCloud bunların arasında geçebilir). Bunun dışındaki
  her şey, adresin içinde bir giriş bilgisi, başka bir port ya da şema reddedilir.
- Bir sunucu yanıtı en çok 4 MB okunur ve bir liste 10.000 etkinlikte durur; kötü niyetli bir sunucu
  belleği ya da zamanı tüketemez.
- Kaydedilen parola yalnızca kaydedildiği adrese ve kullanıcı adına gönderilir. Bunlardan birini
  değiştirmek parolayı yeniden yazmak demektir.
- Yönlendirmeler asla izlenmez, böylece parola başka bir sunucuya gönderilemez.
- Sunucunun verdiği bir bağlantı, yalnızca yapılandırılmış takvimin içinde kalıyorsa değiştirmek ya da silmek için kullanılır.
- Modele dönen takvim metni güvenilmeyen veri olarak işaretlenir; onu okumuş bir çalışma, web sayfası ya da e-posta
  okumuş gibi "bulaşmış" sayılır.
- Takvim eklemek, denemek ve kaldırmak sahibin işlemidir: eşleşmiş bir masaüstü, bir sunucunun kimlik bilgilerini
  değiştiremez.
