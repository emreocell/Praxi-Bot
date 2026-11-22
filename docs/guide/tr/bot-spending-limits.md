# Bot harcama sınırları

Her bota, model çağrılarının kullanabileceği miktar için günlük, haftalık ve aylık sınır koyabilirsin.
Botu aç, **Düzenle**'yi, sonra **Sınırlar**'ı seç.

## Neler ayarlayabilirsin

Her dönem için (günlük, haftalık, aylık; yerel takvime göre, hafta pazartesi başlar) isteğe bağlı dört alan vardır:

| Alan | Ne yapar |
| --- | --- |
| Uyarı eşiği (USD) / (token) | Bu miktarın %80'inde ve %100'ünde seni uyarır. Bot çalışmaya devam eder. |
| Durdurma eşiği (USD) / (token) | Bu miktarı aşacak bir model çağrısından **önce** çalışma durur. |

Boş bırakırsan sınır yoktur. Uyarı eşiği, durdurma eşiğinden büyük olamaz.

## Yeni botlar için varsayılan

Hiç yapılandırılmamış bir bot **günde 5 USD ya da 10 milyon token** ile sınırlanır. Sınırlar bölümü bunu
söyler; istersen yükseltebilir ya da kaldırabilirsin. Bu özellikten önce var olan botlar, sen sınır koyana kadar
sınırsız çalışmaya devam eder.

## Sınıra ulaşılınca ne olur

Çalışma "Günlük sınıra ulaşıldı" (ya da haftalık, aylık) kartıyla biter ve üç seçenek sunar:
**Sınırı yükselt** (Sınırlar bölümünü açar), **Bekle** (sayaç gösterilen saatte sıfırlanır) ya da **Durdur**
(durmuş halde bırak). Hiçbir şey döngüyle yeniden denenmez ve model çağrılmaz. Zamanlanmış çalışmalar,
kanal mesajları (örneğin Telegram), alt ajanlar ve arka plandaki hafıza işleri de aynı sınırlara uyar.

## Kullanım nasıl sayılır

- Para, her çağrının kaydedilen maliyetinden gelir ([LLM çağrı defteri](../llm-call-ledger.md)).
  Fiyatı bilinmeyen modeller yalnızca token ekler. Bu yüzden para (USD) sınırı onları sayamaz: fiyatı
  bilinmeyen bir modeli durdurmaz. Her yeni bota bir token sınırı da konmasının nedeni budur; token sınırı her modeli sayar.
  Fiyatı bilinmeyen modeller kullanıyorsan ayrıca bir token sınırı koy.
- Sağlayıcının kullanım bildirmediği bir çağrı, temkinli bir tahminle sayılır; yani eksik veri hiçbir zaman
  bedava demek değildir. Sayfa, kaç çağrının bu şekilde sayıldığını gösterir.
- Sayaçlar defterde tutulur, bu yüzden yeniden başlatmalardan etkilenmez. Aynı botun paralel çalışmaları
  kalan miktarı paylaşır.

## Sınırları kim değiştirebilir

Yalnızca sahibi (bu bilgisayardaki masaüstü uygulaması). Eşleştirilmiş bir telefon ya da başka bir masaüstü
sınırı değiştiremez veya silemez; yalnızca okuyabilir. Bir bot, bir CLI motoru, MCP kipi ya da yalnızca
API belirtecine sahip herhangi bir şey sınırı değiştiremez ya da kaldıramaz; her değişiklik eski ve yeni
değerlerle birlikte denetim günlüğüne yazılır (`bot.spend_limits_changed`). Uzak bir ana makinenin sınırları o makinede değiştirilir.

Kullanım sayfasındaki, tüm kurulumu kapsayan aylık sınır bunların üstüne ayrıca uygulanır.

API: `GET` ve `PUT /v1/bots/{id}/spend-limits`.
