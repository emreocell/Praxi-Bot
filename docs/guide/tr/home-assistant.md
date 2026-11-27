# Home Assistant

Botlar Home Assistant varlıklarını okuyabilir ve servisleri çağırabilir (ışığı açmak, termostatı ayarlamak gibi).
Kurulumu uygulamadan yaparsın, elle düzenlenecek dosya yok. Her çağrı izin motorundan geçer.

## Uygulamadan kurulum

1. Home Assistant'ta profilini aç, **Güvenlik**'e git ve bir **uzun ömürlü erişim anahtarı** (Home Assistant'ta “long-lived access token”) oluştur.
2. Praxi'de **Uygulamalar**'ı ve **Home Assistant** kartını aç.
3. Adresi yaz (örneğin `http://homeassistant.local:8123`), anahtarı yapıştır ve **Kaydet ve test et**'e bas.
   Praxi Home Assistant'ın `/api/` adresine sorar ve varlıklarını sayar. Kurulum yalnızca Home Assistant kabul
   ederse kaydedilir.
4. **Home Assistant'ı kimler kullanabilir** altında **Tüm botlar** ya da **Yalnızca bu botlar**'ı seç, sonra
   **Erişimi kaydet**'e bas.

Botlar hemen kullanabilir, yeniden başlatmak gerekmez. Anahtar güvenli depoda saklanır ve bir daha gösterilmez:
değiştirmek için yenisini yaz. **Bağlantıyı test et** kayıtlı kurulumu yeniden dener, **Kaldır** kurulumu ve
saklanan anahtarı siler; araçlar tüm botlardan kalkar.

Bunu yalnızca sahip yapabilir: masaüstü uygulaması ya da sunucunun sahip anahtarı. Yalnızca API anahtarını tutan
bir bot, CLI motoru ya da betik kurulumu okuyamaz, değiştiremez.

## Komut satırından

```
HA_TOKEN=... bot ha setup --url http://homeassistant.local:8123 --token-env HA_TOKEN
bot ha status
bot ha test
bot ha access --bot Atlas          # ya da --all; ikisi de yoksa hiçbir bot
bot ha remove
```

`bot ha setup` anahtarı adını verdiğin ortam değişkeninden (ya da gizli bir istemden) okur ve asla yazdırmaz.
Aşağıda anlatılan servis listesi için `--allow cover.open_cover` kullan (tekrarlanabilir).
Bkz. [Komut satırı başvurusu](cli-reference.md).

## Araçlar ve neler sorar

| Araç | Etkisi | Notlar |
| --- | --- | --- |
| `ha_list_entities` | okuma | Durumlar; istenirse tek bir alan adı için. İsimler güvenilmeyen metindir. |
| `ha_get_state` | okuma | Öznitelikleriyle tek bir varlık. |
| `ha_call_service` | yazma | Gerçek dünyada iş yapar. Her yazma işlemi gibi botun erişim düzeyini izler; yani **Tam erişim**'de kart çıkmadan çalışır. |

Yalnızca sıradan cihaz servisleri botun erişim düzeyini izler: ışıklar, anahtarlar, fanlar, iklim, medya
oynatıcılar, robot süpürgeler, nemlendiriciler, su ısıtıcılar, sayılar ve seçimler, bildirimler ve konuşma.
Geri kalan her şey, sen izin vermedikçe **her** düzeyde, Tam erişim dahil, sorar:

- diğer her alan: kilitler, alarm panelleri, kapaklar (garaj kapıları, panjurlar), vanalar, düğmeler, yardımcılar,
  sirenler, betikler, otomasyonlar ve sahneler, ayrıca MQTT ya da Z-Wave gibi ham protokoller (bir kilide adını
  anmadan ulaşabilirler);
- adında `unlock`, `disarm` ya da `open` geçen her servis;
- adında `garage`, `gate`, `door`, `lock` ya da `alarm` geçen bir varlık üzerindeki her çağrı (garaj kapısı çoğu
  zaman sıradan bir anahtardır);
- bir alanı, bir cihazı, bir katı, bir etiketi, `all`'u ya da özel olarak hiçbir şeyi hedeflemeyen çağrılar; ışıklar,
  fanlar, iklim, medya oynatıcılar, robot süpürgeler, nemlendiriciler ve su ısıtıcılar bunun dışındadır;
- tuhaf varlık adları (küçük harfli `domain.object` dışındaki her şey): Home Assistant'a ulaşmadan reddedilir.

Bir servis için kartı kaldırmak üzere onu **Sormadan izin verilen servisler** alanına satır satır
`domain.service` biçiminde yaz (örneğin `cover.open_cover`). Listedeki bir servis, çağrıldığı her varlık için
sormadan çalışır. Yalnızca tam adlar geçerlidir ve `homeassistant.*` asla listeye eklenemez. Tam erişimde de
sorabilen diğer güvenlik kuralları (örneğin verinin bilgisayarından çıkması) geçerli kalır.

## Güvenlik

- Botlar yalnızca verdiğin adresle konuşur. Yönlendirmeler asla izlenmez, böylece anahtar başka yere gönderilemez;
  anahtar uygulama, API ya da komut satırı tarafından asla geri verilmez.
- `https`; düz `http` yalnızca ev ağı (Tailscale dahil) ya da localhost adresleri için. Adres Praxi'nin kendi
  API'sini, bulut meta veri adresini ya da link-local alanı gösteremez.
- Kod çalıştıran ya da merkezi durduran servisler botlar için engellidir: `shell_command`,
  `python_script`, `pyscript`, `hassio`, `rest_command` ve
  `homeassistant.restart|stop|reload_all`, `recorder.purge*`.
- Varlık adları ve öznitelikleri başkasının düzenleyebileceği metindir: botlar bunları veri sayar, asla talimat
  saymaz.

## Elle (ekransız sunucular)

Uygulama ve `bot ha`, `<veri klasörü>/homeassistant.toml` dosyasını yazar; kendi yazdığın dosya da aynı şekilde
çalışır. Anahtar bir gizli bilgi başvurusudur, anahtarın kendisi değil:

```toml
url = "http://homeassistant.local:8123"
token = "homeassistant/token"             # saklanan bir gizli bilginin adı
allowed_domains = ["light", "switch"]     # isteğe bağlı: yalnızca bu alanlar çağrılabilir
trusted_services = ["cover.open_cover"]   # isteğe bağlı: bunlar için kart çıkmaz
bots = ["<bot kimliği>"]                  # isteğe bağlı: yoksa her bot, [] = hiçbiri
```

Anahtarı `bot secrets set homeassistant/token` ile sakla. Elle düzenlediğin dosyayı okutmak için `bot serve`'ü
yeniden başlat; uygulamadan ya da `bot ha` ile kaydetmek yeniden başlatma gerektirmez (dosya yorumları olmadan
yeniden yazılır).

Elle düzenlenen dosya, uygulamadaki form gibi denetlenir: adlar küçük harfe çevrilir, bir servis
`alan.servis` biçiminde olmalıdır ve `trusted_services` içinde `homeassistant.*` kabul edilmez.
Denetimden geçemeyen dosya kullanılmaz (Home Assistant kapalı kalır, `bot serve` nedenini günlüğe yazar) ve
üzerine hiçbir zaman yazılmaz.
