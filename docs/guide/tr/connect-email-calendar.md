# E-postamı ve takvimimi bağla

Tek bir yönlendirmeli akış e-postanı ve takvimini bağlar. Adresini yazarsın; Praxi
sağlayıcın için işe yarayan en kolay yolu seçer, bağlantıyı canlı dener ve bir şey
ters giderse ne yapacağını sade bir dille söyler.

**Ayarlar → Bağlantılar → E-posta ve web → E-postamı ve takvimimi bağla** bölümünden aç.

## Kısaca

1. **Adresin.** Yaz. Praxi sağlayıcıyı adresten anlar (Gmail, Outlook, iCloud, Yahoo,
   Fastmail ya da başkası). Yanlış tahmin ettiyse veya Google Workspace ya da
   Microsoft 365 üzerinde bir iş adresiyse, listeden doğru sağlayıcıyı seç. Botun
   **e-postanı**, **takvimini** ya da ikisini görmesini istediğini işaretle.
2. **O sağlayıcı için en kolay yol** (aşağıdaki tablo). Uygulama parolası gerekiyorsa
   sayfa iki satırlık talimat ve sağlayıcının tam sayfasını açan bir düğme gösterir.
   Parolayı yapıştırır, posta kutusunu hangi botun kullanacağını seçer ve
   **Bağlantıyı dene**'ye basarsın.
3. **Canlı deneme.** Praxi posta sunucularına, istediysen takvime giriş yapar. Hiçbir
   şey kaydedilmez ve hiçbir şey gönderilmez. Çalışınca **Bağla**'ya bas. İkisinden
   yalnızca biri çalışıyorsa **Çalışanı bağla**'yı kullanabilirsin.

## Hangi sağlayıcı neyi kullanır

| Sağlayıcı | E-posta | Takvim | Neden |
| --- | --- | --- | --- |
| iCloud | Uygulamaya özel parola | Aynı parola (CalDAV) | Apple hesap sayfandan aldığın tek parola ikisi için de çalışır. |
| Fastmail | Uygulama parolası | Aynı parola (CalDAV) | Posta ve Takvimler'e izin veren bir parola oluştur. |
| Gmail / Google Workspace | **Google ile giriş** (Google'ın kendi Gmail sunucusu, yalnızca okuma) ya da uygulama parolası (yalnızca e-posta) | Google ile giriş (yalnızca okuma) | Google takvim için uygulama parolası kabul etmez. Giriş, kendi hesabında bir kez oluşturduğun Google istemcisini kullanır; bkz. [Google Workspace](google-workspace.md). |
| Outlook / Microsoft 365 | Uygulamalar penceresinde Microsoft girişi (Outlook) | Aynı | Microsoft e-posta için uygulama parolalarını kapattı; parola artık işe yaramaz. |
| Yahoo | Uygulama parolası | Sunulmuyor | Yahoo'nun Praxi'nin kullanabileceği bir takvim bağlantısı yok. |
| Diğerleri | Uygulama parolası (sunucular `imap.<alan adı>` ve `smtp.<alan adı>` diye tahmin edilir) | Yazdığın sunucu adresinden CalDAV | Sunucu adlarını düzeltmek için **Daha fazla seçenek**'i aç. |

Google ve Microsoft girişleri **Uygulamalar** penceresinde yapılır (sihirbaz onu senin
için açar). Orada işin bitince geri dön ve **Bitirdim, kontrol et**'e bas.

### Takvimler

Yalnızca takvim sunucusunun adresini yazarsın (iCloud ve Fastmail kendiliğinden
dolar). Praxi takvimlerini oradan bulur. Birkaç tane bulursa botun görmesini
istediklerini işaretle; her biri [Takvim (CalDAV)](calendar.md) içinde ayrı bir
takvim olur. Praxi, güvenliğin için bir takvim sunucusunun başka bir adrese
yönlendirmesini izlemez; başka bir sağlayıcıda sağlayıcının verdiği son CalDAV adresini yaz.

## Bot neleri yapabilir: önce okuma, göndermek ikinci bir onay

- Yeni bir posta kutusu **yalnızca okuma ve taslak** olarak başlar: bot postayı
  okuyabilir, arayabilir ve taslak yazabilir; ama hiçbir şey gönderemez.
- **Bot posta da gönderebilsin** ayrı ve işaretsiz bir kutudur. İşaretlersen, botun
  **Tam erişimi** yoksa her mesaj önce sana gösterilir. [Posta koruması](email.md)
  kuralların (çıkış listesi, günlük sınır, gönderim günlüğü) her iki durumda da geçerlidir.
- Bir takvim girişi (uygulama parolası) etkinlikleri okumanın yanında değiştirebilir de.
  Etkinlik eklemek veya değiştirmek, posta göndermek gibi botun erişim düzeyine
  bağlıdır. Başkalarını davet eden her şey aynı onaydan geçer.
- Bir posta kutusu, seçtiğin **tek bir bota** aittir. Yalnızca o bot görebilir.

## Gizli bilgiler nereye gider

Yapıştırdığın uygulama parolası uygulamanın kendi arka plan hizmetine gönderilir ve
gizli bilgi deposunda (işletim sisteminin anahtar zinciri ya da şifreli dosya)
saklanır. Bir daha gösterilmez, hiçbir günlüğe yazılmaz, API'den geri dönmez ve
modelin önüne konmaz. Sihirbazın durum satırları yalnızca "çalışıyor" ya da
"çalışmıyor" der. Google girişindeki Google istemci sırrı da aynı şekilde saklanır.

## Bir şey çalışmazsa

| Gördüğün | Anlamı | Ne yapmalı |
| --- | --- | --- |
| "Sunucu girişi reddetti" | Parola yanlış ya da uygulama parolası gereken yerde normal parolanı yazdın. | Sağlayıcının sayfasında (düğme açar) yeni bir uygulama parolası oluştur ve yeniden yapıştır. iCloud'da Apple Kimliğin iCloud adresin değilse **Daha fazla seçenek** altına yaz. |
| "Uygulama parolaları 2 adımlı doğrulama ister" (Google) | Ayar kapalı ya da iş hesabında yönetici uygulama parolalarını kapatmış. | Geri dön ve **Google ile giriş yap**'ı seç. |
| "Sunucu adlarını tahmin ettik ve yanıt vermiyorlar" | Başka bir sağlayıcı ve sunucusu `imap.<alan adı>` değil. | **Daha fazla seçenek**'i aç ve sağlayıcının verdiği sunucu adlarını yaz. |
| "Güvenli bağlantı kurulamadı" | Sunucu adı ya da bağlantı noktası yanlış ya da ağ engelliyor. | **Daha fazla seçenek** altındaki ad ve bağlantı noktalarını kontrol et. |
| "Sunucu zamanında yanıt vermedi" | Yavaş ya da meşgul sunucu. | Biraz sonra yeniden dene. |
| "Takvim sunucusu bu adreste takvim bulamadı" | CalDAV adresi yanlış. | Sağlayıcının CalDAV için verdiği adresi yaz. |
| "Takvim sunucusu bizi başka bir adrese yönlendirdi" | Sunucu yönlendiriyor; Praxi bunu izlemez. | Sağlayıcının verdiği son takvim adresini yaz. |

## İptal etme ve bağlantıyı kesme

- Her posta kutusunun ve takvimin **Ayarlar → Bağlantılar → E-posta ve web** içinde bir
  çöp kutusu düğmesi var ([E-posta](email.md), [Takvim](calendar.md)). Kayıtlı parolayı da siler.
- **Hepsinin bağlantısını kes** (aynı **E-postamı ve takvimimi bağla** kartında) tüm
  posta kutularını, takvimleri ve Google Gmail ile Takvim girişlerini tek adımda,
  tek bir onaydan sonra kaldırır. Oluşturduğun Google istemcisi kalır (Drive
  kullanıyor olabilir). Botun kendi AgentMail adresine dokunulmaz.
- Erişimi **sağlayıcıda** da geri almak için onun sayfasını aç (sihirbazın son adımı
  bağlantıyı verir): Apple hesap sayfan, Google'ın
  [üçüncü taraf erişimi](https://myaccount.google.com/permissions) sayfası,
  Microsoft'un [uygulama izinleri](https://account.live.com/consent/Manage) sayfası ya
  da Yahoo veya Fastmail'in uygulama parolası listesi; "Praxi" kaydını sil.

## Komut satırı için

`bot email add`, `bot email test` ve `calendar.toml` dosyası eskisi gibi çalışır
(bkz. [E-posta bağlantısı](email.md) ve [Takvim (CalDAV)](calendar.md)); sihirbaz aynı
ayarları yazar.
