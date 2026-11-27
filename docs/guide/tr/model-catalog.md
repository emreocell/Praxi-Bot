# Model seçici bilgileri

Daemon, ayarlanmış her sağlayıcının döndürdüğü model kimliklerini açıklamalarla zenginleştirir.
Derlenmiş tablo yalnızca gösterim verisidir: özel kimlikleri sınırlamaz, botun kayıtlı modelini
değiştirmez, yetenek geçersiz kılmalarını değiştirmez ve fiyatlandırmaya dokunmaz.

## API

`GET /v1/providers`, `configured[].models` alanını bir metin dizisi olarak korur. Bu kimlikler artık
sohbet adaylarına süzülür ve önce öneri sırasına, sonra görünen ada göre dizilir. Eklenen
`configured[].model_details` dizisi aynı sırayı kullanır:

```json
{
  "id": "claude-sonnet-5-5",
  "display_name": "Claude Sonnet 5.5",
  "kind": "chat",
  "recommended": true,
  "recommended_rank": 1,
  "deprecated": false,
  "note": null
}
```

Kısa bir seçici için önce `recommended: true` olan kayıtları göster, kalanları bir **Tüm modeller**
açılır bölümünün arkasına koy. Arka uç listenin kalanını sınırlamaz. Mevcut masaüstü seçicisi
görünen adları ve sıralamayı kullanır. Yerleşim ve açılır bölüm ayrı arayüz yenilemesinin işidir.

Sohbet dışı ve kullanımdan kalkmış modelleri de almak için `GET /v1/providers?chat_only=false`
kullan. Varsayılan `chat_only=true`'dur. Uç nokta ve eş izinleri değişmez. Yeni bir Tauri komutu
eklenmez.

`kind`, birincil kullanımdır: `chat`, `reasoning`, `vision_capable`, `embedding`, `image`, `audio`,
`video` ya da `other`. İlk üçü sohbet adayıdır. Önerilen bir modelin türü, görüntü kabul etse ya da
akıl yürütse bile `chat`'tir. Çalıştırma yetenek bayrakları kendi başlarına doğruluk kaynağı olarak
kalır. Bilinmeyen kimlikler insan okuyabilir bir görünen ad alır ve asla öneri almaz. Temkinli
çıkarım, görüntü/video/ses/müzik/gömme/gerçek zamanlı üretim, özelleşmiş derin araştırma,
bilgisayar kullanımı, denetim ve yeniden sıralama uç noktalarını belirten adları gizler.

API yalnızca sağlayıcının gerçekten döndürdüğü kimlikleri listeler. Canlı listede olmayan,
derlenmiş bir modeli araya eklemez. Bu yüzden yerel modellerin önce kurulmuş ve yerel sunucuda
görünür olması gerekir. Başarısız bir listeleme hiç model adayı döndürmez ve maskelenmiş bir hata
verir. Bozuk ya da gizli bilgi taşıyan kimlikler, kullanılamaz kimliklere çevrilmek yerine çıkarılır.
Botun mevcut ya da elle girilmiş özel modeli, bu listede olmasa bile kullanılabilir kalır.

## Derlenmiş veri ve bakım

`crates/provider-catalog/src/model_metadata.json` dosyası depodaki tabloyu, `last_reviewed` tarihini
ve resmî kaynak adreslerini içerir. `providers/catalog/*.toml` sağlayıcı uç noktalarını, bağlam
sınırlarını ve başvuru fiyatlandırmasını tutar. Doğrulayıcı şunları denetler:

- gerekli alanlar ve geçerli ISO `YYYY-MM-DD` gözden geçirme/fiyat tarihleri,
- benzersiz sağlayıcı kimlikleri, takma adlar ve model kimlikleri. Her takma ad kendi asıl sağlayıcısına
  çözülür ve gönderilen her TOML sağlayıcısı/modeli tabloda yer alır,
- sağlayıcı başına benzersiz görünen adlar (büyük/küçük harf fark etmez),
- makul bağlam pencereleri, çıktı sınırları (`max_output_tokens <= context_window`) ve negatif
  olmayan, sonlu fiyatlandırma (`cached_per_mtok <= input_per_mtok`),
- 1 ile 4 arasında benzersiz öneri sıraları, sağlayıcı başına en fazla dört öneri, `:free` önerisi
  olmaması, kullanımdan kalkmış ya da sohbet dışı öneri olmaması,
- kullanımdan kalkmış modellerin hem `"deprecated": true` hem de insan okuyabilir bir notla (örneğin
  `"legacy"` ya da `"retired"`) açıkça işaretlenmesi,
- `model_metadata.json` ile aşağıdaki özet tablo arasında hiç sapma olmaması.

`model_metadata.json` dosyasını değiştirmek çıkarımı ya da çalıştırmayı değiştirmez.

<!-- BEGIN:model-catalog-table -->
Son gözden geçirme: 2026-11-15.

| Sağlayıcı | Takma adlar | Son gözden geçirme | Önerilen adaylar | Kaynaklar |
| --- | --- | --- | --- | --- |
| `gemini` | `gemini-openai-compat` | 2026-11-15 | Gemini 4 Carbon (`gemini-4-carbon`), Gemini 4 Argon (`gemini-4-argon`), Gemini 4 Hydrogen (`gemini-4-hydrogen`), Gemini 3.5 Flash-Lite (`gemini-3.5-flash-lite`) | <https://ai.google.dev/gemini-api/docs/models>, <https://ai.google.dev/gemini-api/docs/deprecations> |
| `openai` | Yok | 2026-11-15 | GPT-6.1 Sol (`gpt-6.1-sol`), GPT-6 Luna (`gpt-6-luna`), GPT-6 Astra (`gpt-6-astra`), GPT-6.1 Terra (`gpt-6.1-terra`) | <https://developers.openai.com/api/docs/models> |
| `anthropic` | Yok | 2026-11-15 | Claude Sonnet 5.5 (`claude-sonnet-5-5`), Claude Opus 5.5 (`claude-opus-5-5`), Claude Haiku 5.5 (`claude-haiku-5-5`), Claude Fable 5.2 (`claude-fable-5-2`) | <https://platform.claude.com/docs/en/models/overview> |
| `openrouter` | Yok | 2026-11-15 | Claude Sonnet 5.5 (`anthropic/claude-sonnet-5.5`), GPT-6 Luna (`openai/gpt-6-luna`), Gemini 3.8 Flash (`google/gemini-3.8-flash`) | <https://openrouter.ai/anthropic/claude-sonnet-5.5>, <https://openrouter.ai/openai/gpt-6-luna>, <https://openrouter.ai/google/gemini-3.8-flash> |
| `xai` | Yok | 2026-11-15 | Grok 4.7 (`grok-4.7`) | <https://docs.x.ai/developers/models> |
| `deepseek` | Yok | 2026-11-15 | DeepSeek V4.1 Flash (`deepseek-flash`), DeepSeek V4 Pro (`deepseek-v4-pro`) | <https://api-docs.deepseek.com/quick_start/pricing/> |
| `ollama` | Yok | 2026-11-15 | Qwen 3 · 8B (`qwen3:8b`), Qwen 3 · 4B (`qwen3:4b`), Qwen 3 · 30B (`qwen3:30b`), Llama 3.3 · 70B (`llama3.3:70b`) | <https://ollama.com/library/qwen3>, <https://ollama.com/library/llama3.3> |
| `lmstudio` | Yok | 2026-11-15 | Yok (kurulu yerel modellere bağlı) | <https://lmstudio.ai/docs/developer/openai-compat/models> |
| `cli:claude` | Yok | 2026-11-15 | Claude Code default (`default`), Claude Sonnet (`sonnet`), Claude Opus (`opus`), Claude Haiku (`haiku`) | <https://code.claude.com/docs/en/model-config> |
| `cli:codex` | Yok | 2026-11-15 | Codex default (`default`) | <https://developers.openai.com/codex/models> |
| `groq` | Yok | 2026-11-15 | GPT-OSS 120B (`openai/gpt-oss-120b`), GPT-OSS 20B (`openai/gpt-oss-20b`) | <https://console.groq.com/docs/models>, <https://console.groq.com/docs/rate-limits> |
| `mistral` | Yok | 2026-11-15 | Mistral Large (`mistral-large-latest`), Mistral Medium (`mistral-medium-latest`), Mistral Small (`mistral-small-latest`), Codestral (`codestral-latest`) | <https://docs.mistral.ai/models> |
| `together` | Yok | 2026-11-15 | DeepSeek V4.1 Flash (`deepseek-ai/DeepSeek-V4.1-Flash`), Llama 3.3 70B Instruct Turbo (`meta-llama/Llama-3.3-70B-Instruct-Turbo`), GPT-OSS 120B (`openai/gpt-oss-120b`) | <https://docs.together.ai/docs/serverless/models>, <https://docs.together.ai/docs/serverless/rate-limits> |
| `meta` | Yok | 2026-11-27 | Muse Spark 1.3 (`muse-spark-1.3`), Muse Wonder 1.0 (`muse-wonder-1.0`) | <https://dev.meta.ai/docs/models>, <https://dev.meta.ai/docs/pricing-rate-limits> |
<!-- END:model-catalog-table -->

### Tazelik araçları ve eskime hatırlatmaları

Praxi tasarım gereği çevrimdışı çalışır, bu yüzden katalog tazeliği çalışma sırasında indirilerek
değil, kayıtlı `last_reviewed` tarihine bakılarak denetlenir:

- **Çevrimdışı doğrulama ve tazelik denetimi:**
  ```sh
  bot catalog check --docs docs/guide/model-catalog.md
  ```
  `model_metadata.json` dosyasını, `providers/catalog/*.toml` dosyalarını ve yukarıdaki belge
  tablosunu doğrular, sonra her sağlayıcının gözden geçirme tarihini ve yaşını yazdırır.
  Doğrulama hataları sıfır olmayan kodla çıkar. Eskime (90 günden eski) bir uyarı yazdırır ve
  `--strict` verilmedikçe 0 koduyla çıkar. Makinelerin okuyacağı çıktı için `--json` ekle.
- **Bir sağlayıcı anlık görüntüsüne (JSON ya da TOML) karşı çevrimdışı fark:**
  ```sh
  bot catalog diff anthropic ./anthropic-models.json
  bot catalog diff gemini ./gemini-models.toml
  ```
  Bir sağlayıcının (ya da `gemini-openai-compat` gibi bir takma adın) kayıtlı kataloğunu, kaydedilmiş
  bir liste anlık görüntüsüyle karşılaştırır ve eklenen, kaldırılan ve değişen modelleri, ayrıca
  olası yeniden adlandırmaları yazdırır. Salt okunurdur: dosyaları asla düzenlemez ve ağ çağrısı
  yapmaz. Anlık görüntü yolu yerine `--live` verirsen, ayarlanmış sağlayıcını saklanan anahtarıyla
  sorgulamayı isteğe bağlı olarak seçersin (o da salt okunurdur).
- **Yukarıdaki Markdown tablosunu yeniden üret:**
  ```sh
  bot catalog render-docs
  ```
- **Eskime hatırlatmaları:** `last_reviewed` 90 günden eskiyse `bot doctor` ve Ayarlar, Tanılama
  engellemeyen bir `catalog_freshness` uyarısı bildirir. Haftalık bir GitHub Actions iş akışı
  (`.github/workflows/catalog-freshness.yml`) çevrimdışı denetimi çalıştırır ve bir gözden geçirme
  zamanı geldiğinde iş özetine engellemeyen bir uyarı yazar.

Bakımcıların gözden geçirme rutini için [Model kataloğunu taze tutma](../../contributing/MAINTAINING.md)
sayfasına bak.

OpenAI ve xAI şu anda ana dalda özel OpenAI uyumlu sağlayıcı yapılandırmasını kullanır. Bu değişiklik
yeni sağlayıcı bağdaştırıcıları değil, model bilgisi sağlar. Yalnızca Claude ve Codex'in çalışma motorları
vardır. Cursor/OpenCode/Gemini CLI ana dalda yalnızca algılanır, bu yüzden yeni bir çalıştırılabilir
sağlayıcı ya da tahmin edilmiş model kimliği eklenmez.

CLI motorlarının Praxi'de canlı model listesi API'si yoktur. Ayarlanmış modeli ya da mevcut `default`
seçeneğini korurlar. Tablodaki diğer CLI takma adları, hesap hakkı iddiası olarak eklenmez. xAI ve
DeepSeek bilerek üçten az öneriye sahiptir, çünkü doğrulanmış güncel sohbet kadroları daha kısadır.
OpenRouter'ın üç adayı, birinci taraf kimlikleri değil, doğrulanmış tam kimliklerini kullanır.
Öneri ya da varsayılan olarak hiçbir `:free` model sabitlenmez. Bir `free` notu yalnızca canlı listede
bir OpenRouter `:free` kimliği göründüğünde eklenir.

## Otomatik katalog güncellemeleri (uygulama güncellemesi gerekmez)

Yeni modeller sürekli çıkar. Seçici bir uygulama sürümünü beklemesin diye Praxi-Bot daha yeni bir model
listesini kendi başına indirebilir.

- **Ne yapar:** daemon, en çok günde bir kez projenin GitHub sürümünden iki küçük dosya
  (`provider-catalog.json` ve imzası `provider-catalog.json.minisig`) indirir. İkisi de sabit
  `https://` adreslerine yapılan düz `GET` isteğidir. **Senin, botlarının ya da sohbetlerin hakkında
  hiçbir şey gönderilmez.**
- **Yalnızca imzalı dosyalar kullanılır.** İmza, uygulamaya gömülü açık anahtarla eşleşmelidir.
  Değiştirilmiş, imzasız, çok büyük ya da *daha eski* bir dosya atılır ve elindeki model listesi
  çalışmaya devam eder. Daha yeni bir dosyadaki bilinmeyen ek alanlar yok sayılır.
- **Nasıl birleştirilir:** her sağlayıcı için indirilen kayıt, gözden geçirme tarihi aynı ya da daha
  yeniyse yerleşik kaydın yerini alır. Dosyanın anmadığı sağlayıcılar olduğu gibi kalır.
- **Görmek için:** `bot doctor` “Catalog updated 2026-12-01” yazar ve liste 30 günden eski olunca
  güncelleme ipucu verir.
- **Kapatmak için:** `bot catalog auto-update off` (durumu `bot catalog auto-update status` ile
  bak, `on` ile yeniden aç). `BOT_CATALOG_UPDATES=off` ortam değişkenini ayarlamak da durdurur.
- **Elle güncellemek için:** `bot catalog update` daha yeni bir kataloğu hemen kurar.
  `bot catalog update --check` yalnızca bir tane olup olmadığını söyler.

İndirilen dosyalar `<data dir>/provider-catalog/` içinde durur ve her açılışta yeniden doğrulanır,
bu yüzden elle düzenlenmiş bir kopya yok sayılır.

### Bakımcılar: katalog yayımlama

İmza anahtarı **asla** commit edilmez. Bir kez üret ve dosyayı özel tut (bir parola yöneticisi ya da
çevrimdışı bir disk):

```sh
python scripts/release/sign_provider_catalog.py --generate-key ~/catalog-key.b64
```

Açık anahtarı yazdırır. O metin, `crates/provider-catalog/src/catalog_update.rs` içindeki
`PINNED_PUBLIC_KEY` değerine eşit olmalıdır (değiştirmek bir uygulama sürümü gerektirir). Her yenileme
için `crates/provider-catalog/src/model_metadata.json` dosyasını güncelle (`last_reviewed` tarihini
artır), sonra:

```sh
PRAXI_CATALOG_SIGNING_KEY="$(cat ~/catalog-key.b64)"   python scripts/release/sign_provider_catalog.py --out dist/
```

ve `dist/provider-catalog.json` ile `dist/provider-catalog.json.minisig` dosyalarını sürüm varlıkları
olarak yükle (uygulama `releases/latest/download/...` adresini okur). Betik, Python `cryptography`
paketine ihtiyaç duyar.

## Yerel modeller: donanım ve öneriler

`GET /v1/local-models` (yalnızca sahip), bu bilgisayarın Ollama ya da LM Studio ile neyi çalıştırabildiğini
bildirir. En iyi çaba esaslıdır ve salt okunurdur:

- **Donanım yoklaması.** RAM, mantıksal çekirdekler, veri klasöründeki boş disk, ayrıca kuruluysa
  `nvidia-smi` üzerinden GPU adı ve ekran belleği (sabit bağımsız değişkenler, kabuk yok, 1,5 sn sınırı,
  16 KiB çıktı sınırı). Apple silicon birleşik belleğini sayar. Okunamayan her şey tahmin edilmez,
  bilinmiyor diye bildirilir.
- **Öneri motoru.** Adaylar, model tablosunun derlenmiş `ollama` satırlarıdır. Bir modelin boyut sınıfı,
  kendi kimlik etiketindeki parametre sayısıdır (`qwen3:8b` 8 milyardır). Her biri, temkinli 4 bitlik
  tahminlerle bir karar alır (iyi çalışır, yavaş çalışır, çok büyük). `recommended`, sığan ve diskte
  yer kalan en büyük modeldir. Bellek bilinmiyorsa ya da katalog boşsa null olur (“öneri yok”).
  `ollama pull <id>` komutu yalnızca düz kimliklerden kurulur ve kopyalaman için gösterilir. Praxi onu
  asla çalıştırmaz.
- **Nerede görünür.** İlk kurulum sihirbazı (Ollama seçildiğinde), Ayarlar > Modeller > Yerel modeller
  ve `bot doctor` (süreç içinde hesaplanan bilgilendirici bir satır). `GET /v1/doctor` bunu bilerek
  dışarıda bırakır: donanım parmak izi yalnızca sahibe açıktır.
- **Gizlilik.** Donanım özeti bu bilgisayarda kalır: bir modele, bir sağlayıcıya ya da telemetriye asla
  gönderilmez.

## Bir model listesinin sınırları

Canlı bir model listesi ve derlenmiş bir öneri, bir hesabın kalan kotası, yeterli kredisi ya da
döndürülen her kimliği çağırma izni olduğunu kanıtlayamaz. Bu özellik ücretli bir tamamlama ya da
gerçek hesap yoklaması yapmaz. Sağlayıcı hataları ve hesap başına kota çalışma anının konularıdır.
Bilinmeyen model adlarının çıkarımı temkinlidir, ileride sağlayıcıların vereceği girdi türü ve hak bilgisinin
yerini tutmaz.

Gemini 2.5 Flash-Lite, Google'ın güncel listesine göre **kullanımdan kalkmış değildir**. Üzerinde
`note: "existing users only"` notu vardır. Yeni projeler güncel önerilen bir model seçmeli olsa da bu
ayrım önemlidir. Mevcut botlar ayarlı kimliklerini korur.

## Sahibin doğrulaması

1. Uygulamada ayarlanmış bir sağlayıcıyı aç ve insan okuyabilir adların gösterildiğini doğrula. Bir ad
   seçmek yine özgün sağlayıcı/model kimliğini kaydeder.
2. `GET /v1/providers` ile `?chat_only=false` çıktısını karşılaştır: varsayılan, medya, gömme,
   özelleşmiş uç noktalar ve bilinen kullanımdan kalkmış modelleri dışarıda bırakır.
3. Yerel bir model sunucusunu durdur ve yenile: API listeleme hatasını bildirir ve kurulmamış öneriler
   görünmez.
4. Bir botu, derlenmiş tabloda olmayan özel bir modelde tut ve normal bir tur çalıştır. Model bilgisi,
   yürütücüsünü değiştirmez.

Otomatik testlerin hiçbirinde gerçek bir sağlayıcı tamamlaması, kurulu bir masaüstü açılışı ya da hesap
kotası denetimi yoktur. Tüm model ve yol deneme verileri yerel sahtelerdir.
