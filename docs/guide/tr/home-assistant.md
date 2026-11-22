# Home Assistant

Botlar Home Assistant varlıklarını okuyabilir ve servisleri çağırabilir (ışığı açmak, termostatı ayarlamak gibi).
Her çağrı izin motorundan geçer.

## Kurulum

1. Home Assistant'ta profilini aç ve bir **uzun ömürlü erişim belirteci** oluştur.
2. Onu gizli bilgi deposunda sakla (asla bir yapılandırma dosyasında değil):

   ```
   bot secrets set homeassistant/token
   ```

3. `<veri klasörü>/homeassistant.toml` dosyasını oluştur:

   ```toml
   url = "http://homeassistant.local:8123"
   token = "homeassistant/token"             # başvuru, belirtecin kendisi değil
   allowed_domains = ["light", "switch"]     # isteğe bağlı: çağrılabilecekleri sınırla
   ```

4. `bot serve`'ü yeniden başlat.

## Araçlar

| Araç | Etkisi | Notlar |
| --- | --- | --- |
| `ha_list_entities` | okuma | Durumlar; istenirse tek bir alan adı için. İsimler güvenilmeyen metindir. |
| `ha_get_state` | okuma | Öznitelikleriyle tek bir varlık. |
| `ha_call_service` | yıkıcı | Gerçek dünyada iş yapar, bu yüzden her zaman sorar. |

## Güvenlik

- `https`; düz `http` yalnızca ev ağı ya da localhost adresleri için.
- Yönlendirmeler asla izlenmez, böylece belirteç başka yere gönderilemez.
- Kod çalıştıran ya da merkezi durduran servisler botlar için engellidir: `shell_command`,
  `python_script`, `pyscript`, `hassio`, `rest_command` ve
  `homeassistant.restart|stop|reload_all`, `recorder.purge*`.
- `allowed_domains`, çağrılabilir alan adlarını daha da daraltır (boş = engellenmeyen her şey).
