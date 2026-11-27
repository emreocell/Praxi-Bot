# Sağlayıcılar ve modeller

**Sağlayıcı**, senin için bir dil modelini çalıştıran bir servistir (ya da yerel bir
programdır). Praxi yalnızca senin ayarladığın sağlayıcılarla konuşur, başka hiçbir yere bir
şey gönderilmez. **Model** ise bir sağlayıcının sunduğu modellerden biridir. Bot profillerinde
model `provider/model` biçiminde yazılır, örneğin `gemini/gemini-2.5-flash-lite`.

## Yerleşik sağlayıcılar

| Kimlik | Nedir | Anahtar gerekir mi |
| --- | --- | --- |
| `gemini` | Google Gemini, kendi API'si (ücretsiz katmanı var) | `GEMINI_API_KEY` |
| `gemini-openai-compat` | OpenAI uyumlu uç noktası üzerinden Gemini | `GEMINI_API_KEY` |
| `anthropic` | Anthropic Claude, kendi Messages API'si | `ANTHROPIC_API_KEY` |
| `openrouter` | Tek anahtarla birçok model (OpenAI uyumlu) | `OPENROUTER_API_KEY` |
| `deepseek` | DeepSeek (OpenAI uyumlu) | `DEEPSEEK_API_KEY` |
| `groq` | Groq (OpenAI uyumlu, düşük gecikmeli açık modeller) | `GROQ_API_KEY` |
| `mistral` | Mistral AI: Large, Medium, Small, Codestral, Pixtral (OpenAI uyumlu) | `MISTRAL_API_KEY` |
| `together` | Together AI (OpenAI uyumlu, sunucusuz açık modeller) | `TOGETHER_API_KEY` |
| `meta` | Meta Model API: Muse Spark ve Muse Wonder (OpenAI uyumlu) | `META_API_KEY` |
| `ollama` | `http://localhost:11434/v1` adresinde Ollama ile yerel modeller | yok |
| `lmstudio` | `http://localhost:1234/v1` adresinde LM Studio ile yerel modeller | yok |

**Mistral:** La Plateforme'dan bir API anahtarı alman gerekir
(<https://console.mistral.ai/api-keys>). Le Chat sohbet aboneliği bir API anahtarı değildir
ve burada kullanılamaz. Mistral yalnızca tam dokuz harf ya da rakamdan oluşan araç çağrısı
kimliklerini kabul eder. Bu yüzden Praxi-Bot başka kimlikleri (örneğin bir sohbeti başka bir
sağlayıcıdan Mistral'e geçirdiğinde) sabit, dokuz karakterlik bir kimliğe çevirir. Mistral'in
Voxtral konuşmadan yazıya özelliği, dikte için bağlı değildir. Ayrıntılar dikte notlarında.

**Meta (Muse modelleri):** Meta Model API panelinden (<https://dev.meta.ai/>) bir API
anahtarı alman gerekir. Anahtarlar `LLM_` ile başlar. Hazır ayar `https://api.meta.ai/v1`
adresine bağlanır ve `muse-spark-1.3` (günlük kullanım modeli) ile `muse-wonder-1.0` (en
güçlü model, Meta'da hâlâ önizleme aşamasında) modellerini listeler. Tam adı olan
`muse-wonder-1.0` ifadesini kullan: kısa ad olan `muse-wonder` yoktur. İki model de görsel
okur, akış halinde yanıt verir, araç çağırır ve JSON döndürebilir. Meta'nın sınırları takım
başına, model başına dakikada 3.000 istek ve 4 milyon token'dır. Meta yalnızca otomatik araç
seçimini kabul eder ("mutlaka bir araç çağır" isteğini reddeder). Praxi-Bot de böyle bir
istek hiç göndermez. Meta'nın sunduğu `-contributor` sürümleri daha ucuzdur ama Meta'nın
istemlerini ve yanıtlarını eğitimde kullanmasına izin verir. Bu yüzden katalogda yoktur.
Bunu kabul ediyorsan elle seç. Kullanılabilirlik ülkene göre değişir (Meta'nın Coğrafi
Kullanım Politikası'na bak). Muse Image, Muse Voice ve Segment Anything anahtarın model
listesinde görünür ama sohbet modeli değildir. Seçici bunları gizler.

OpenAI uyumlu başka herhangi bir sunucuyu da bir temel URL ile **özel sağlayıcı** olarak
ekleyebilirsin (aşağıya bak). Katalog ayrıca milyon token başına USD fiyatlarıyla bir model
listesi taşır. Uygulama maliyet tahminlerini bu sayede gösterir. Sağlayıcı izin veriyorsa
güncel model listesi doğrudan sağlayıcıdan alınır.

Katalogdaki Anthropic modelleri `claude-opus-5-5`, `claude-sonnet-5-5` ve
`claude-haiku-5-5`. Gemini modelleri arasında `gemini-3.1-flash-lite`,
`gemini-2.5-flash-lite`, `gemini-3-flash-preview` ve `gemini-3.1-pro-preview` var. Fiyatlar
ve model adları değişir. Esas kaynak `providers/catalog/` içindeki katalog dosyalarıdır.

## Sağlayıcı eklemek ve yönetmek

```sh
bot providers list                         # * genel varsayılanı gösterir
bot providers add gemini                   # anahtarı sorar (yazdığın görünmez)
bot providers add gemini --key-env GEMINI_API_KEY --model gemini-2.5-flash-lite
bot providers add mylocal --base-url http://localhost:8000/v1   # özel
bot providers test [id]                    # bağlantıyı dener
bot providers default gemini --model gemini-2.5-flash-lite
```

- `add` önce bağlantıyı dener. `--no-test` bu adımı atlar.
- Eklediğin ilk sağlayıcı genel varsayılan olur.
- API anahtarları gizli bilgi deposunda (işletim sisteminin anahtarlığında ya da şifreli bir
  dosyada) `provider.<id>.api_key` adıyla saklanır. Asla `providers.toml` dosyasına yazılmaz ve
  asla ekrana basılmaz. Anahtarı komut satırına yazmak yerine `--key-env` ya da gizli girişi
  kullan.
- `bot secrets list|set|delete` gizli bilgileri doğrudan yönetir. Değerler hiçbir zaman
  gösterilmez.
- Çalışan bir daemon yeni sağlayıcıyı `POST /v1/providers/reload` sonrasında görür (o sırada
  devam eden çalışmalar eski sağlayıcılarıyla sürer).

## Hangi model neyi çalıştırır

Bir botun profili modelleri **rollerine** göre adlandırır (bkz.
[Botlar ve profiller](bots-and-profiles.md)):

- `executor`: ajan döngüsünü çalıştırır (zorunlu).
- `light`: basit ilk turlar için isteğe bağlı model. Bkz. [model katmanlama](model-tiering.md).
- `classifier`: karar merdiveninin kullandığı, isteğe bağlı küçük ve hızlı bir model.
- `fallback`: yeniden denenebilir hatalardan ya da eksik yeteneklerden sonra devreye giren
  yedek modeller.
- `planner`: ileride kullanılmak üzere kaydedilir. Döngüyü planlayıcı ve uygulayıcı diye ikiye
  ayırmaz.

İyi bir eşleşme, `executor` olarak yetenekli bir model, `classifier` olarak da ucuz bir
modeldir.

Model seçicinin görünen adları, öneri sırasını ve `model_details` bilgisini nereden aldığını
görmek için [Model seçici bilgileri](model-catalog.md) sayfasını oku.

## Gemini ücretsiz katmanı

Google, Gemini için [Google AI Studio](https://aistudio.google.com/) üzerinden ücretsiz bir
API katmanı sunar. Orada bir API anahtarı oluşturman yeter, kredi kartı gerekmez. Praxi'yi
denemenin en ucuz yolu budur.

```sh
export GEMINI_API_KEY=...        # PowerShell: $env:GEMINI_API_KEY = "..."
bot providers add gemini --key-env GEMINI_API_KEY --model gemini-2.5-flash-lite
```

Neler beklemen gerektiğini aşağıda bulabilirsin. Sınırları Google belirler. Bunlar modele ve
hesaba göre farklıdır ve zamanla değişir. Kendi anahtarının sınırlarını AI Studio'daki hız
sınırı sayfasında görebilirsin.

- Her modelin dakika başına istek, dakika başına token ve **günlük istek** sınırları ayrıdır.
  Bazı Flash-Lite modelleri günde yüzlerce isteğe izin verirken bazı modeller yalnızca yirmi
  kadar isteğe izin verebilir. En yetenekli (Pro) modeller ücretsiz katmanda hiç sunulmayabilir.
- Bir ajan her görev için birkaç model çağrısı yapar, bu yüzden küçük bir günlük sınır çabuk
  dolar. `executor` ve `classifier` için bir Flash-Lite modeli seç, görevleri küçük tut ve
  ücretsiz katmanda çok sayıda zamanlama çalıştırmaktan kaçın.
- Sınıra ulaşıldığında sağlayıcı bir hatayla yanıt verir (HTTP 429). Praxi bir model adımını
  artan aralıklarla birkaç kez yeniden dener (3 deneme). Ama günlük sınır birkaç saniye
  beklemekle sıfırlanmaz. Sıfırlanmasını bekle ya da sağlayıcı değiştir.
- Ücretsiz katmanın koşulları ücretli katmandan farklıdır, Google'ın istemlerini nasıl
  kullanabileceği de buna dahil. Hassas bir şey göndermeden önce Google'ın güncel koşullarını oku.
- Bir Gemini anahtarı, karar merdiveninin (K2) kullandığı **embedding** aramasını da açar.
  Böylece istekler iş akışlarınla ve becerilerinle anlam bakımından eşleşebilir.

## Yerel modeller

[Ollama](https://ollama.com/) ya da LM Studio kur, bir model indir, sonra:

```sh
bot providers add ollama --model <model-name>
```

Anahtar gerekmez ve hiçbir şey bilgisayarından çıkmaz. Araç çağırmayı destekleyen bir model
seç, yoksa bot araçlarını iyi kullanamaz.

## OpenAI uyumlu özel sağlayıcılar

```sh
bot providers add myserver --base-url https://my.host/v1 --key-env MY_KEY --model my-model
```

Bunu vLLM, LiteLLM, bir şirket ağ geçidi ve benzerleri için kullan.
