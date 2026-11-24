# Yardımcılar (isteğe bağlı): embedding ve karar yardımcısı

Praxi'yi kullanmak için bunların hiçbirine ihtiyacınız yok. Anahtar yoksa Praxi
kelimeleri eşleştirmek için basit bir çevrimdışı yöntem kullanır ve **hiçbir şey
bilgisayarınızdan çıkmaz**.

Daha fazlasını eklemek için **Ayarlar > Modeller > Yardımcılar (isteğe bağlı)**
bölümünü açın.

## Embedding nedir?

Embedding, bir metni sayı listesine çevirir; böylece Praxi notları ve anıları
anlamına göre bulabilir ("Berlin'deki evim" ifadesi "nerede oturuyorum" sorusuyla
eşleşir). Praxi birden fazla sağlayıcı kullanabilir ve tek birine bağlı değildir:

| Sağlayıcı | Ne için iyi |
| --- | --- |
| Google Gemini (`gemini-embedding-001`, `gemini-embedding-2`) | Kişisel kullanım için ücretsiz bir anahtar yeter |
| OpenAI (`text-embedding-3-small`, `text-embedding-3-large`) | Ücretli, yaygın |
| OpenAI uyumlu herhangi bir sunucu | Ollama, LM Studio, OpenRouter, Mistral, Together: adresi, modeli, vektör boyutunu ve gerekirse anahtarı girin |
| Yerel model | Yerel embedding ile derlenmiş sürümlerde; metin bilgisayarınızda kalır |
| Çevrimdışı yöntem | Son çare olarak her zaman var |

## Metniniz yalnızca izninizle gönderilir

Yerel model ve çevrimdışı yöntem dışındaki her sağlayıcı notlarınızın, anılarınızın
veya belge parçalarınızın **metnini alır**. Bu yüzden:

- Sağlayıcı eklerken, metnin o sağlayıcıya gönderileceğini açık sözlerle onaylamanız
  istenir. Onay kutusu işaretlenmezse sağlayıcı hiç çağrılmaz.
- Parolalar ve anahtarlar metinden önce çıkarılır.
- Gizli işaretlenen hiçbir şey gönderilmez; o grup çevrimdışı yöntemi kullanır.
- Seçtiğiniz botları uzak sağlayıcılardan uzak tutabilirsiniz.
- API anahtarları gizli depoda durur, ayar dosyasına yazılmaz ve bir daha gösterilmez.
- Bunu yalnızca sahibi (masaüstü uygulaması veya sunucunun sahip anahtarı) değiştirebilir.

## Ücretsiz anahtarlar ve sınırlar

Ücretsiz anahtarların dakikalık (istek ve token) ve günlük sınırları vardır. Praxi
kendi sayacını tutar, her sınırın yaklaşık %90'ında kalır, kısa süre bekler; olmazsa
yasaklanma riskine girmek yerine sıradaki sağlayıcıyı kullanır. Ayarlardaki
çubuklar dakikadaki isteği, dakikadaki tokenı ve günlük isteği gösterir. Sayaçlar
bellektedir ve Praxi yeniden başlayınca sıfırlanır.

Sınırlar Praxi'nin içindeki tarihli bir listeden gelir ("son kontrol" tarihine
bakın). Sağlayıcılar sınırları değiştirir; sağlayıcı başına üzerine yazabilirsiniz.
Google AI Studio ücretsiz katmanı için 2026-11-24 tarihinde kontrol edildi:

| Model | İstek/dk | Token/dk | İstek/gün |
| --- | --- | --- | --- |
| Gemini Embedding 1 ve 2 | 100 | 30.000 | 1.000 |
| Gemini Decision 1 | 200 | 50.000 | 10.000 |

## Yedek zinciri

Her amaç için (doğru kısayolu veya aracı seçme, hafızayı hatırlama) Praxi
sağlayıcıları sizin sıranızla dener. Kotası biten, anahtarı reddedilen veya
çevrimdışı olan sağlayıcı bir süre dinlenir ve sıradaki kullanılır. Çevrimdışı
yöntem her zaman sondadır. Farklı sağlayıcıların vektörleri asla karıştırılmaz.

## Karar yardımcısı (isteğe bağlı)

Praxi, küçük evet/hayır ve seçim soruları için Google'ın Decisions modelini
(`gemini-decision-1`) isteğe bağlı yardımcı olarak kullanabilir. **Varsayılan olarak
kapalıdır**, bir Gemini anahtarı gerektirir ve aynı yerden açılır. Yalnızca tavsiye
verir: bir karar hiçbir zaman izin veremez veya bir aracı onaylayamaz. Çağrılar,
diğer model çağrıları gibi LLM çağrı defterinde ve harcama sınırlarında sayılır.
`decider.toml` dosyasını kendiniz yazdıysanız (isteğe bağlı karar havuzu)
Ayarlar ona dokunmaz.

## Sorunlar

| Gördüğünüz | Ne yapmalı |
| --- | --- |
| Anahtar reddedildi | Anahtarı sağlayıcıda kontrol edip yeniden yapıştırın |
| Ücretsiz sınıra ulaşıldı, bekliyor | Bekleyin veya ikinci bir sağlayıcı ekleyin; Praxi kendiliğinden devam eder |
| Yanıtların boyutu yanlış | Vektör boyutu modelle uyuşmuyor; önerilen sayıyı kullanın |
| Ulaşılamıyor | Sunucu adresini ve sunucunun çalıştığını kontrol edin |
