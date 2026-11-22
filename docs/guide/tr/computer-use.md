# Kendi bilgisayarında bilgisayar kullanımı

Sohbet **Bu bilgisayar**'ı kullanıyorsa ve sahip anahtarı *Botlarım bu bilgisayarı kullanabilsin* açıksa,
botlar Windows bilgisayarındaki uygulamalarda çalışabilir. Kapalıysa sohbette tek düğmeli bir
**Bilgisayar kullanımını aç** kartı görünür.

## Bir bot neler yapabilir

| Araç | Amacı |
| --- | --- |
| `desktop_list_apps` | Görünen pencereleri listeler (kimlik, başlık, işlem). |
| `desktop_launch` | Bir program, belge ya da URI başlatır. Paketlenmiş uygulamalar (örneğin WhatsApp) `appx:<AppUserModelID>` kullanır. |
| `desktop_focus` | Açık bir pencereyi öne getirir ve okur. |
| `desktop_get_state` | Bir pencerenin erişilebilirlik (UI Automation) ağacını okur: adlar, roller, ekran sınırları. İsteğe bağlı olarak pencereye özel ekran görüntüsü. |
| `desktop_wait` | Bir pencere başlığı ya da öğe adı/rolü için en çok 10 sn bekler. |
| `desktop_click`, `desktop_type`, `desktop_key`, `desktop_scroll` | Öğe sırasına göre (`gen` ile) ya da ağaçta hedef yoksa gözlenen pencerenin içinde ekran x/y konumuna göre işlem yapar. |

Modele önce erişilebilirlik ağacını okuması, ekran görüntüsünü ise yalnızca ağaç hedefi göstermediğinde alması söylenir.

## Güvenlik

- Her odaklanma, tıklama, yazma, tuş ve kaydırma izin motorundan geçer: varsayılan olarak sorar ve bir onay yalnızca o anki görevle sınırlanabilir.
- Onaylanan girdi, gönderilmeden önce canlı pencereye karşı yeniden denetlenir; başka bir pencere odağı almışsa işlem reddedilir.
- Parola alanlarına asla yazılmaz.
- Sen bilgisayarı kendin kullanırken ya da Durdur / duraklat etkinken hiçbir şey çalışmaz. *Praxi bilgisayarını yönetiyor* göstergesi ve Durdur kısayolu her zaman geçerlidir.

## Doğrulanmayanlar

- Gerçek bir WhatsApp (paketlenmiş uygulama) kurulumuyla davranış, canlı bir makinede denenmedi.
- Gerçek masaüstüne karşı bir Not Defteri/Hesap Makinesi entegrasyon testi eklenmedi; kapsam, sahte bir arka uç ve birim testlerinden oluşuyor.
- Linux/macOS eşleri: odaklanma ve bekleme "desteklenmiyor" yanıtına döner.
