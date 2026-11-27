# Uygulamalarını bağla

**Uygulama**, botunun başka bir servisle çalışmasını sağlar; örneğin Gmail, Notion, Linear ya da dosyaların. Uygulama olmadan bir bot yalnızca sohbet edebilir ve temel araçlarını kullanabilir.

Bazı uygulamalara **MCP sunucusu** denir. Bu yalnızca teknik bir addır: "bir bota yeni araçlar veren eklenti". Bundan fazlasını bilmen gerekmez.

> **Resimler:** "Resim:" ile başlayan satırlar, sonradan eklenecek bir ekran görüntüsünü anlatır.

## Bir uygulama bağla

1. Sol çubukta **Uygulamalar**'a tıkla.
2. Uygulamayı bulmak için **Uygulamalarda ara**'yı ya da kategori etiketlerini kullan. Ekleyebileceğin uygulamalar **Keşfet** altındadır.
3. Uygulamanın sayfasını aç ve **Bağlan**'a bas.
4. Tarayıcında bir sayfa açılır. Orada giriş yap ve erişime izin ver. Sonra Praxi'ye dön.
5. Uygulama **Bağlı** durumuna geçer ve **Uygulamaların** altında görünür.

**Uygulamalar** penceresinin üstünden Composio hesabını bir kez bağla. Tarayıcıda
giriş yapabilir veya proje API anahtarı kullanabilirsin. Ardından uygulamanın
kendi sayfasından izin ver.

## Uygulamayı kimler kullanabilir

Bağlı uygulamanın sayfasını aç. Doğrudan MCP sunucusu için **MCP sunucuları**
bölümünde **Yönet**'i seç. **Bu uygulamayı kimler kullanabilir** altında şunu seç:

- **Tüm botlar**, ya da
- **Yalnızca seçilen botlar**; botları işaretle. Sonra **Kaydet**'e bas.

Hiçbir bot işaretlemezsen **hiçbir bot uygulamayı kullanamaz**. Her uygulamayı yalnızca ona ihtiyacı olan botlara ver.

## Her iznin anlamı

| Gördüğün | Sade sözlerle anlamı |
| --- | --- |
| **Bu uygulamayı kimler kullanabilir** | Botlarından hangilerinin uygulamayı hiç kullanabileceği. |
| **Botların kullanabileceği araçlar** | Uygulamanın yapabildiği şeylerin listesi. Tek tek araçları kapatabilirsin. |
| **veri değiştirir** | Bu araç uygulamada bir şeyleri değiştirebilir, örneğin bir sayfayı düzenleyebilir. |
| **silebilir veya gönderebilir** | Bu araç bir şeyleri silebilir ya da dışarı gönderebilir, örneğin bir e-posta gönderebilir. |
| **Botun okuyup yazabileceği klasör** | Bazı dosya uygulamaları tek bir klasör ister. Yalnızca o klasör paylaşılır. Praxi'nin kendi verileri ile parola ya da oturum klasörlerin hiçbir zaman paylaşılamaz. |
| **Oturumu kapat** | Girişi unut. Uygulama listede kalır ve **Oturum açılması gerekiyor** gösterir. |
| **Bağlantıyı kes** | Uygulamayı kaldır. Praxi önce "Bu sunucunun bağlantısı kesilsin mi?" diye sorar. |

Güvenli varsayılan: gerekene kadar **veri değiştirir** ve **silebilir veya gönderebilir** araçlarını kapalı tut.

## Riskli adımları yine sen onaylarsın

Bir uygulamayı bağlamak onayları atlamaz. Bir bot, uygulamayla bir şeyi değiştirmek ya da göndermek istediğinde bir **Onay gerekiyor** kartı görürsün. Composio uygulamalarında kart, tüm bağımsız değişkenlerin yukarıda gösterildiğini belirtir. Karar vermeden önce her şeyi okumak için **Tüm bağımsız değişkenleri göster**'e bas. Okumalar için kart çıkmaz, değişiklikler botun erişim düzeyine bağlıdır, para hareketi yapan her şey her zaman sorar: bkz. [hangi işlem çalışmadan önce sorar](apps.md#hangi-işlem-çalışmadan-önce-sorar). Bkz. ayrıca [Botunu güvende tut](keep-your-bot-safe.md).

## Veri uygulamanın servisine gider

Botun bir uygulamayı kullandığında gönderdiği veri o servise gider. Composio uygulamalarında Composio'nun sunucularından geçer. Yalnızca güvendiğin servisleri bağla.

## Bir uygulama değiştiğinde

Bir uygulama, araçlarından birini tanımlama biçimini değiştirebilir. Sen onayladıktan sonra bu olursa Praxi o aracı kapatır. **Bir araç, onayladığınızdan sonra değişti** başlığını, **Onayladığınız metin** ve **Şimdiki metin** ile birlikte görürsün.

- İki metni de oku.
- **Yeni sürüme güven**'e yalnızca uygulamaya güveniyorsan ve yeni metin doğru görünüyorsa bas.
- Emin değilsen hiçbir şey yapma. Araç kapalı kalır ve bot onu kullanamaz.

## Durum sözcükleri

| Sözcük | Ne yapmalı |
| --- | --- |
| **Bağlı** | Bir şey yapma. Çalışıyor. |
| **Bağlanıyor** | Birkaç saniye bekle. |
| **Oturum açılması gerekiyor** | **Oturum aç**'a bas. |
| **Bağlı değil** | **Yönet** altında **Yeniden bağlan**'a bas. |
| **Hata** | Satırın altındaki ayrıntıları aç. Yazanı düzelt, sonra **Yeniden bağlan**'a bas. |
| **Karantinada** | Uygulama birkaç kez başarısız oldu ve duraklatıldı. Sorunu düzelt, sonra **Yeniden bağlan**'a bas. |

Daha fazlası: [Uygulamaları bağlama](apps.md) (tüm ayrıntılar) ve [MCP sunucuları](mcp-servers.md) (elle kuranlar için).
