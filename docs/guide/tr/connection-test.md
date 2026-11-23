# Bağlantıyı dene

Praxi-Bot'taki her anahtarın ve uygulama bağlantısının bir **Bağlantıyı dene** düğmesi var: yapay zekâ
sağlayıcı anahtarları (Ayarlar → Yapay zekâ), web araması ve dikte anahtarları (Ayarlar), e-posta
hesapları, MCP sunucuları (Uygulamalar) ve ilk kurulumdaki anahtar adımı. Deneme bilgisayarındaki Praxi
hizmetinde çalışır, anahtarını asla göstermez, yaklaşık 12 saniye sonra vazgeçer ve aşağıdaki
sonuçlardan biriyle yanıt verir. HTTP durumu ve hizmetin kendi hata kodu **Ayrıntılar** kutusundadır.

MCP sunucusu denemesi sunucuyu başlatır ve araçlarını listeler. Hiçbir aracı çalıştırmaz.

<a id="bad-key"></a>
## Hatalı anahtar

Hizmet "hayır" dedi (HTTP 401/403 ya da "API key not valid" gibi bir ileti). Anahtarı sağlayıcının
sitesinden yeniden kopyala, başında ya da sonunda eksik olmadığını ve doğru sağlayıcıya ait olduğunu
kontrol et, sonra yeniden kaydet. Bir sohbete ya da nota yapıştırdığın anahtar sağlayıcı tarafından
iptal edilmiş olabilir; yenisini oluştur.

<a id="no-network"></a>
## Ağ yok

Hizmete ulaşılamadı ya da zamanında yanıt vermedi. İnternet bağlantını, VPN'ini ve proxy'ni kontrol et.
Bu bilgisayardaki bir program için (Ollama, LM Studio, bir MCP sunucusu) çalıştığından emin ol.

<a id="quota-exceeded"></a>
## Kota doldu

Anahtar sorunsuz ama plan, kredi ya da hız sınırı dolmuş (HTTP 402/429). Biraz bekleyip yeniden dene ya da
sağlayıcının sitesinde planını ve kredini kontrol et.

<a id="wrong-endpoint"></a>
## Yanlış adres

Sunucu yanıt verdi ama bu hizmet gibi değil (HTTP 404 ya da API olmayan bir sayfa) ya da bir MCP
sunucusunun komutu başlatılamadı. Girdiğin web adresini (`https://` ve yol dahil; birçok hizmet `/v1` ile
biten bir yol ister) ya da MCP sunucusunun komutunu kontrol et.

<a id="provider-outage"></a>
## Hizmet kesintisi

Hizmetin kendisinde sorun var (HTTP 5xx). Birkaç dakika sonra yeniden dene ya da sağlayıcının durum sayfasına bak.

<a id="unknown"></a>
## Bilinmeyen sonuç

Yukarıdakilerden hiçbiri değil. **Ayrıntılar**'ı aç, durumu ve kodu sağlayıcının belgeleriyle karşılaştır
ve yardım isterken ayrıntıları kopyala. Anahtarını asla içermezler.
