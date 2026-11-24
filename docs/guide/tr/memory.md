# Bellek: botların neyi hatırladığı

Başka dilde: [English](../memory.md) · [Deutsch](../de/memory.md).

Praxi botları sohbetler arasında işe yarar şeyleri hatırlayabilir: adın, yanıtları nasıl sevdiğin ya da sürekli döndüğün bir proje gibi. Bu sayfa neyin hatırlandığını, nerede durduğunu ve nasıl görüp düzeltebileceğini, unutturabileceğini ve kapatabileceğini sade bir dille anlatır.

## Neler hatırlanır

Normal bir sohbet turundan sonra bot **senin** söylediklerine bakar ve "Berlin'de yaşıyor" ya da "Kısa yanıtları seviyor" gibi kısa notlar tutabilir. Bunlar yalnızca modelden istenmez, kodda da zorunlu kılınır:

* Yalnızca senin söylediklerin, kullandığın tam sözlerle birlikte alıntı olarak tutulur. Sorular, "ya şöyle olsa" fikirleri ve şakalar tutulmaz.
* Parolalar, API anahtarları ve gizli bilgiye benzeyen hiçbir şey tutulmaz.
* Bir yanıttan önce, mesajına uyan birkaç not arka plan bilgisi olarak mesajın önüne konur. Asla talimat olarak kullanılmazlar.

Her not aynı şekilde ele alınmaz:

* **Senin söylediklerin** hemen kaydedilir ve kullanılır ("Kaydetmeden önce sor"u seçmediysen).
* **Dışarıdan alınanlar** (web sayfası, dosya, e-posta, araç sonucu ya da başka bir bot) *güvenilmez* diye etiketlenir. **Onayın gerekiyor** listesinde bekler ve **sen onaylayana kadar kullanılmaz**.

Bot, sen istediğinde de bir notu kaydedebilir veya unutabilir ("doğum günümün 3 Mart olduğunu hatırla"). Bunun için kendi mesajından alıntı yapması gerekir.

## Nerede durur

Her şey, bilgisayarında Praxi veri klasöründe (`memory.db` dosyası), sohbetlerinden ayrı saklanır. Aynı dosya, botun "önceki sohbetlerde ara" aracı için geçmiş mesajlarının aranabilir bir kopyasını da tutar; belleği **Kapalı** olan bot buna yeni bir şey eklemez. Hatırlanması için hiçbir şey kimseye gönderilmez. Her bot ortak notlarını ve kendi notlarını görür; başkalarının notları asla görünmez.

Çevrimiçi bir gömme (embedding) sağlayıcısıyla **Akıllı** bulmayı açarsan (aşağıda), notlarının metni arama verisine çevrilmek üzere o sağlayıcıya gönderilir. Bu yalnızca açık bir onay kutusunu işaretledikten sonra olur ve varsayılan olarak kapalıdır.

## Görme, düzeltme ve unutturma

**Ayarlar → Kişisel bellek**'i aç, bir bot seç; şunları yapabilirsin:

* her notu kategorisi, ait olduğu bot, ne zaman kaydedildiği ve geldiği alıntıyla birlikte okumak;
* bir notu **Düzenle** (düzenlemen senin söylediğin sayılır) veya **Unut**;
* bir notta yapılan son değişikliği **Geri al**;
* kendin not eklemek;
* bir sonraki mesajının önüne tam olarak neyin konacağının **önizlemesini** görmek.

Yalnızca bir süre doğru olan bir not **sona erer** ve bir tarih gösterir. Bu süre dolunca bot notu kullanmayı bırakır. "Cumaya kadar", "bu hafta" ya da "3 günlüğüne" gibi şeyler söylediğinde bot bu tarihi kendisi koyar; İngilizce, Almanca ve Türkçe bu tür ifadeleri anlar ve okuyamadığı tarihleri tahmin etmez.

**Unut**, bir notu gizler ve kullanılmasını durdurur. Bu bilgisayardaki geri alma geçmişinde kalır. Her şeyi diskten silmek için Praxi'yi kapat ve veri klasöründeki `memory.db` dosyasını sil; botlar boş bir bellekle başlar.

## Onayın gerekiyor

Web sayfalarından, dosyalardan, araçlardan veya başka botlardan alınan notlar bellek ayarlarının en üstündeki **Onayın gerekiyor** listesinde görünür. **Onayla**, botun notu kullanmasına izin verir (modele yine komut olarak değil, alıntılanmış veri olarak gösterilir). **At** notu unutturur. Sen karar verene kadar bot onu kullanmaz ve önizlemede asla görünmez.

## Kapat ya da önce sor

Her botun kendi ayarı **Botu düzenle → Gelişmiş → Bellek** altında (ve **Ayarlar → Kişisel bellek**'in en üstünde) bulunur:

| Ayar | Ne olur |
| --- | --- |
| **Açık** (varsayılan) | Söylediklerinden öğrenir. Dışarıdan gelenler senin onayını bekler. |
| **Kaydetmeden önce sor** | Her yeni not önce **Onayın gerekiyor** listesinde bekler. |
| **Kapalı** | Bot hiçbir şey öğrenmez, hatırlamaz ve bellek araçları yoktur. Mevcut notlar saklanır ama kullanılmaz. |

**Gelişmiş** altında:

* **Bir yanıta okunan bellek**, bir mesajın önüne ne kadar arka plan konabileceğini (yaklaşık 150 ile 1500 kelime, varsayılan 600) belirler. Daha fazlası daha çok bağlam verir ama her yanıtı biraz yavaşlatır ve pahalılaştırır.
* **Öğrendiklerimi diğer botlarımla paylaş** (varsayılan açık), seninle ve geçmiş olaylarla ilgili bilgileri tüm botlarınla paylaşır. Kapatırsan bu bot öğrendiği her şeyi kendine saklar. Yöntemler ve dersler her zaman onları öğrenen botta kalır.

Bu ayarları yalnızca sen, botun çalıştığı bilgisayarda değiştirebilirsin; eşleştirilmiş bir telefon veya başka bir bilgisayar değiştiremez. **Güvenli mod** belleği tamamen kapalı tutar.

## Notlar nasıl bulunur: Temel veya Akıllı

* **Temel (kelime eşleşmesi)** varsayılandır. Bir not, mesajınla aynı kelimeleri içeriyorsa bulunur ("çay", "yeşil çay seviyor"u bulur). Model gerektirmez ve hiçbir yere bir şey göndermez. "Araba" ile "otomobil"in aynı şey olduğunu anlamaz.
* **Akıllı (anlam)**, bir botun bilgi kaynakları için zaten kurduğun bir gömme modelini kullanır ([Bilgi gömmeleri](../knowledge-embeddings.md) sayfasına bak); böylece "araba", "otomobil kullanıyor"u bulabilir. Açmak, notlarını arka planda yeniden dizinler ve ilerlemeyi gösterir; bitene kadar Temel kullanılır. Model başarısız olursa Praxi Temel'e döner ve hiçbir şey kaybolmaz.

Geçiş için **Ayarlar → Kişisel bellek → Anılar nasıl bulunur** bölümünü kullan.
