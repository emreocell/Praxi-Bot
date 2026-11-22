# Takvim (CalDAV)

Botlar bir CalDAV takviminde (Nextcloud, Radicale, Fastmail, iCloud, Synology ...) etkinlik okuyabilir ve
ekleyebilir. Her çağrı, diğer araçlar gibi izin motorundan geçer.

## Kurulum

1. Takvim parolasını gizli bilgi deposunda sakla (asla bir yapılandırma dosyasında değil):

   ```
   bot secrets set calendar/home/password
   ```

2. `<veri klasörü>/calendar.toml` dosyasını oluştur:

   ```toml
   [[calendar]]
   name = "home"
   url = "https://dav.example.com/calendars/me/personal/"   # takvim koleksiyonu
   username = "me"
   password = "calendar/home/password"                      # başvuru, parolanın kendisi değil
   ```

3. `bot serve`'ü yeniden başlat. `calendar_*` araçları, kullanmasına izin verilen botlarda görünür.

## Araçlar

| Araç | Etkisi | Notlar |
| --- | --- | --- |
| `calendar_list_events` | okuma | Bir aralıkla çakışan etkinlikler; yinelenen etkinlikleri sunucu açar. Başlıklar ve notlar güvenilmeyen metindir. |
| `calendar_create_event` | yazma | Bir etkinlik ekler. Katılımcı yazılmaz, yani kimse davet edilmez ya da bilgilendirilmez. |
| `calendar_delete_event` | yıkıcı | `uid` ile bir etkinliği siler; her zaman sorar. |

Tarihler `YYYY-MM-DD` (tam gün) ya da saat farkıyla RFC 3339 biçimindedir.

## Güvenlik

- URL `https` olmalıdır (düz `http` yalnızca geri döngü adresleri için: `localhost`, `::1` ya da `127.*`; örneğin bir test sunucusu).
- Yönlendirmeler asla izlenmez, böylece parola başka bir sunucuya gönderilemez.
- Sunucunun verdiği bir bağlantı, yalnızca yapılandırılmış takvimin içinde kalıyorsa silmek için kullanılır.
- Modele dönen takvim metni güvenilmeyen veri olarak işaretlenir; onu okumuş bir çalışma, web sayfası ya da e-posta
  okumuş gibi "bulaşmış" sayılır.
