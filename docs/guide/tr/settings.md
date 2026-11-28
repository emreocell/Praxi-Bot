# Ayarlar

**Ayarlar**'ı profil menüsünden (sol rayın altındaki adın), `Ctrl/Cmd+,` ile ya da
yazı alanında `/settings` yazarak aç. Değişiklikler kendiliğinden kaydedilir: her
değişiklikten sonra sağ üstteki etikette **Kaydedildi** yazar.

![Ayarlar, Genel sayfası: adın, dilin, bildirimler ve solda sayfa listesi](../images/tr/settings-general.webp)

**Güvenlik** sayfasında soldaki sayfa listesini, alttaki **Gelişmiş ayarlar** anahtarını
(**Daha fazla seçenek**), **Önerilene sıfırla** düğmesini, üç güvenlik düzeyini ve birçok karttaki **Bu ne demek?** (`?`) açılır kutularını görürsün.

## Sayfalar

Soldaki listede sekiz sayfa, üç grupta toplanır:

| Grup | Sayfa | Ne işe yarar |
| --- | --- | --- |
| **Sen** | **Genel** | Adın, dilin, bildirimler, ses, dikte, bilgisayarla başlatma, diğer asistanlardan içe aktarma. |
| **Sen** | **Görünüm** | Tema, vurgu, arka plan, duvar kâğıdı, okunabilirlik. |
| **Botların** | **Yapay zekâ ve hafıza** | Modeller ve API anahtarları, botların hatırladıkları, belgeler ve bilgi klasörleri. |
| **Botların** | **Bağlantılar** | Botların çalıştığı bilgisayar, posta, takvim, web araması (Composio uygulamaları için **Uygulamalar**). |
| **Botların** | **Güvenlik** | Güvenlik düzeyi, güvenilen klasörler, verilen izinler, etkinlik günlüğü. |
| **Botların** | **Kasa** | Botların kullanabileceği girişler ve sırlar (ayrı sayfa; menüden ya da `/vault` ile). |
| **Uygulama** | **Sistem** | Kullanım, harcama limitleri, depolama, tanılama (`bot doctor`), günlükler. |
| **Uygulama** | **Güncellemeler** | Kurulu sürümler, güncelleme kanalı, sürüm notları. |

Profil menüsünün üstündeki hızlı kutucuklar **Görünüm**, **Yapay zekâ** ve **Kullanım**a (**Sistem** sayfası) gider.

## Arama

Sol üstteki arama kutusuna yaz. Her ayar; başlığına, açıklamasına ve yaygın
alternatif sözcüklerine göre aranır (aksanlar ve büyük harfler önemli değildir,
yani "gonder" yazınca "Gönder" bulunur). Eşleşmeler vurgulanır ve her sonuç
hangi sayfada olduğunu gösterir.

## Daha fazla seçenek

Varsayılan olarak her sayfa çoğu kişinin ihtiyaç duyduklarını gösterir. Nadir
kartlar alttaki **Daha fazla seçenek** (açıkken **Gelişmiş ayarlar**) arkasındadır.
Ayarları her açışında bu bölüm kapalı gelir; yalnızca arama ya da doğrudan bir bağ
gelişmiş bir karta işaret ederse kendiliğinden açılır. Arama, gelişmiş ayarları da
her zaman bulur.

Birçok kartta **`?` simgesi** vardır: üzerine gel veya tıkla; kısa açıklama ve
ilgili rehber sayfasına bağlantı görürsün.

## Güvenlik düzeyleri

Bir bota özel kurallar vermediysen botlar üç düzeyden birine uyar:

| Düzey | Kendi başına yapar | Önce sana sorar |
| --- | --- | --- |
| **Dikkatli** | Dosya okur, bir şeyleri araştırır, metin taslağı hazırlar. | Dosya düzenler ya da kaydeder, mesaj ya da e-posta gönderir, web sayfası açar, komut çalıştırır. |
| **Dengeli** (önerilen) | Okur, gezinir, çalışma alanındaki dosyaları düzenler. | Bir şeyleri siler, mesaj ya da e-posta gönderir, para harcar, senin bilgisayarını kullanır. |
| **Serbest** | Silme ve gönderme dahil hemen her şeyi. Kendi başlattığın bir çalışmada bir sayfa okuduktan sonra bile sormadan web'de gezer, tıklar ve yazar. | Para harcama, tehlikeli komutlar, kayıtlı bir sırrı yazmak, e-posta göndermek ve güvenilmeyen bir sayfa okunduktan sonra program çalıştırmak. Çalışma ayrıca özel verini (dosya, posta, hafıza) tutuyorsa, bunlardan bir parçayı içeren yeni bir site ya da form. |

Bunlar daemon'un `read-and-draft`, `approve-destructive` ve `full` hazır ayarlarıdır.
Eski "Salt okunur" hazır ayarı ile kategoriye göre kurallar, aynı sayfada **Daha
fazla seçenek** altında durmaya devam eder.

## Önerilene sıfırla

**Genel**, **Görünüm** ve **Güvenlik** sayfalarında **Önerilene sıfırla** bulunur.
Önce sorar ve yalnızca o sayfayı değiştirir: botlarına, sohbetlerine ve anahtarlarına
asla dokunmaz. **Güvenlik** sayfasında düzeyi Dengeli'ye geri alır ve "Her zaman izin
ver" kurallarını korur.

## Küçük ekranlar

640 px genişliğe kadar sayfa listesi, ayarların üstünde yana kaydırabileceğin bir
satıra dönüşür.

*Onay*, *ana makine* ve *hazır ayar* gibi sözcükler için [sözlüğe](glossary.md) bak.