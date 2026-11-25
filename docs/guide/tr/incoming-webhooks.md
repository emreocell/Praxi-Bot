# Gelen webhook'lar

Bir webhook, dışarıdaki bir şey bir adresi çağırınca botlarından birini başlatır. Bunu GitHub, bir form ya da evdeki bir anahtar yapabilir. Webhook'u bot panelinde, **Otomasyon** sekmesinde oluşturursun.

Praxi yeni bir kapı açmaz. Adres, zaten `127.0.0.1` üzerinde dinleyen daemon'dadır. Dışarının oraya ulaşması için kendi seçtiğin bir tüneli bu adrese yönelt. Cloudflare Tunnel ve `ssh -L` iki yoldur. Yönlendiricide yeni bir kapı açma. Eşleşmiş bir telefon ya da başka bir bilgisayar bu webhook'ları oluşturamaz, deneyemez ve çağıramaz.

## Ne ayarlarsın

1. Botu aç ve **Otomasyon**'u seç.
2. Webhook'a kısa bir ad ver. İstersen botun veriyle ne yapacağını yaz. Boş bırakırsan bottan veriyi özetlemesi ve içindeki emirlere uymaması istenir.
3. **Webhook oluştur**'a bas.
4. Gizli anahtarı şimdi kopyala. Sonra liste adresi ve adı gösterir, gizli anahtarı asla.
5. Çağrıların durmasını istediğinde webhook'u **Kapalı** yap. **Kaldır** onu siler. **Test gönder** uygulamadan bir çalışma başlatır.

## Çağıran ne gönderir

Ham metni `/v1/incoming-webhooks/{id}` adresine `POST` et.

İki başlık zorunludur:

- `X-Webhook-Timestamp`: şu anki zaman, Unix milisaniye.
- `X-Signature-256`: `sha256=` ve ardından `{zaman}.{ham metin}` metninin gizli anahtarla HMAC-SHA256 değerinin hex hali. `X-Hub-Signature-256` aynı değerle kabul edilir.

Yalnızca gövdenin imzası yetmez. Zaman 5 dakika içinde olmalıdır ve o imza bir kez geçerlidir. İkinci kopya reddedilir. Gövde en fazla 64 KB olabilir. Bir dakika içinde 30'dan fazla kabul edilmiş çağrı reddedilir. Kapalı bir webhook bu sınıra yazılmaz.

## Bot metinle ne yapar

Gövde veridir, emir değildir. Praxi onu güvenilmeyen içerik olarak saklar ve `call:birşey` gibi bir satırı araca çevirmez. Çalışma, botun kendi izin düzeyini kullanır. Bir araç onayını gerektirirse soru bir bildirim olarak sıraya girer; zamanlanmış bir işin sorduğu gibi.

Test düğmesi aynı yoldan küçük bir yük gönderir: `{"praxi_test":true}`. Dışarıdaki hız sınırını harcamaz.

## Ne yazılır

Oluşturma, düzenleme, silme, test, kabul ve ret denetim günlüğüne yazılır. Günlük nedeni saklar (kötü imza, eski zaman, tekrar, hız sınırı, kapalı ya da güvenli mod). Gizli anahtarı ve gövdeyi saklamaz.
