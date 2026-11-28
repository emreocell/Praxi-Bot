# Görevler: tek büyük iş, ara onay olmadan

Uzun süren görevler oturumlar halinde ilerler; bekleme kayıtları yeniden başlatmadan sonra da durur.
Tam rehber için İngilizce metne bak: [Missions (EN)](../missions.md).

## Bir postayı, zamanlayıcıyı veya webhook'u beklemek

Bir madde hemen devam edemezse bot `mission_wait` ile park eder. Görevi uyandırabilecek kaynaklar:

* **Zamanlayıcı** (`after_seconds`, 30 saniye ile 7 gün arası).
* **Yinelenen program** (`every`: örneğin `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR` biçiminde RFC 5545 `RRULE`; isteğe bağlı IANA saat dilimi ile yerel saat ve yaz/kış saati uygulanır). Her oluşum görevi uyandırır; seri bitene kadar bekleme kayıtlı kalır.
* **Biten yardımcı çalışması** (`child_run`: aynı görev oturumunda başlatılan `spawn_subagent` çalışması). Yardımcının son metni webhook gövdesi gibi güvenilmeyen veri olarak sonraki özette gelir.
* **Botun kendi gelen kutusundaki posta** (alan, konu veya anahtar kelime filtresi).
* **Webhook** (sahibin bu bot için oluşturduğu gelen webhook).

Beklemeler veritabanında saklanır; daemon kapansa bile kayıt kalır.

## Bu sürümün sınırları

Bugün çalışanlar arasında zamanlayıcı, posta, webhook, **yinelenen uyandırma** ve **yardımcı çalışması bitince uyandırma** da var. **Henüz yok:** posta yanıtı sınıflandırması (bounce, STOP), görev başına ayrı izin kapsamı, dışa ulaşım hızı ve opt-out kuralları ile yardımcı botlar için alt görevler.