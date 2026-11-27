# REST API bağlama (OpenAPI)

**OpenAPI 3.0 veya 3.1** belgesi yayımlayan her web servisi, eklenti ya da MCP sunucusu
gerekmeden botların kullanabileceği araçlara dönüşebilir. Praxi'ye belgeyi, API'nin adresini ve
gerekiyorsa bir anahtar verirsin. Praxi bulduğu işlemleri listeler, botun kullanabileceklerini
sen açarsın.

**Uygulamalar**'ı aç ve üst çubuktan **REST API'ler**'i seç.

## API bağla

1. **Ad:** istediğin bir ad. Araçların ön eki olur (`api_<ad>_<işlem>`).
2. **Belge nerede?** Bir **web adresi** ver (örneğin `https://api.example.com/openapi.json`)
   ya da bilgisayarından bir **dosya** (JSON veya YAML) seç. Web adresi sonradan **Adresten
   güncelle** ile yenilenebilir, dosya yenilenemez.
3. **API adresi:** isteğe bağlı. İstekler yalnızca bu adrese gider. Boş bırakırsan belgedeki
   ilk sunucu kullanılır. Belge hiç sunucu vermiyorsa ya da yalnızca göreli bir adres
   veriyorsa adresi senin girmen gerekir.
4. **Giriş:** API'nin ne beklediğini seç:

   | Seçenek | Praxi'nin gönderdiği |
   | --- | --- |
   | Yok | hiçbir şey |
   | Erişim anahtarı (Bearer) | `Authorization: Bearer <anahtar>` |
   | Başlıkta API anahtarı | adını verdiğin başlıkta anahtar (örneğin `X-Api-Key`) |
   | Adreste API anahtarı | adını verdiğin sorgu parametresi olarak anahtar |
   | Kullanıcı adı ve parola | HTTP basic kimlik doğrulaması |

5. **API'yi bağla**'yı seç.

Anahtar veya parola gizli bilgi deposuna (işletim sisteminin anahtar zinciri, sunucuda
şifreli dosya) `openapi/<kimlik>/credential` adıyla kaydedilir. Bir daha gösterilmez ve botun
okuduğu hiçbir şeyin parçası olmaz: araç listesinde, botun gördüğü istekte, sonuçta ve günlüklerde
yer almaz. Belgenin kendisi `<veri klasörü>/openapi-specs/` içinde, ayarlar (gizli bilgi
olmadan) `<veri klasörü>/openapi-apps.toml` içinde durur.

## İşlemleri bot başına aç

Bağlı API'yi aç. **Başta hiçbir şey açık değil.** **Erişimi göster** altından bir bot (ya da
**Tüm botlar**) seç, sonra o botun kullanabileceği işlemleri işaretle. Uzun bir belgede aradığını
bulmak için arama kutusunu kullan.

- **Tüm botlar** için açılan işlem her bota açıktır, sonradan oluşturacağın botlara da.
- Tek bir bot için açılan işlemi yalnızca o bot kullanabilir. Diğer botlar aracı hiç görmez.
- Herkese açık bir işlemde bir botu kapatırsan, işlem diğer tüm botların listesine dönüşür.
  Artık kimsenin kullanamadığı işlem kapatılır; asla herkese açılmaz.
- Aynı anda en fazla 50 işlem açık olabilir, çünkü her biri modelin okuması gereken bir araçtır.
- Sildiğin bot bu listelerden çıkarılır.

Değişiklikler hemen uygulanır, yeniden başlatmak gerekmez.

## Hangi işlemler onay ister

Bir işlemin riski HTTP yönteminden gelir:

| Yöntem | Sınıf |
| --- | --- |
| `GET`, `HEAD`, `OPTIONS` | okuma |
| `POST`, `PUT`, `PATCH` | yazma |
| `DELETE` | yıkıcı |

Bir çağrının sorup sormayacağını, diğer araçlarda olduğu gibi botun izin düzeyi belirler:
okuma okuma gibi, değiştirme yazma gibi, silme yıkıcı işlem gibi ele alınır. **Tam erişimde**
hiçbiri sormaz.

**Para ile ilgili işlemler her düzeyde, Tam erişim dahil, mutlaka sorar.** Yolunda veya
`operationId` değerinde *pay*, *payment*, *payout*, *charge*, *checkout*, *purchase*, *buy*,
*subscription*, *transfer*, *refund*, *withdraw*, *invoice* ya da *billing* gibi bir sözcük
geçen işlem para ile ilgili sayılır. İşlem listesinde **Her zaman sorar** diye işaretlenir.
Bu okumalar için de geçerlidir (fatura listelemek de sorar), çünkü eşleşme yalnızca ada bakar ve
bir kez fazla sormak, bir kez fazla ödemekten iyidir. Bu araçlar **Para** izin kategorisindedir.

Araçlar dış bir servise bağlandığı ve başkasının yazdığı metni döndürdüğü için güvenilmeyen
içerik korumaları da geçerlidir: bir bot aynı çalışmada web sayfası ya da e-posta okuduysa,
API'ye yapacağı çağrı önce sorar.

## Praxi neyi korur

- **Tek sunucu.** İstekler yalnızca verdiğin API adresine gider. Botun verdiği yol değerleri
  kodlanır, sunucuyu değiştiremez ve ana yolun dışına çıkamaz (`..` reddedilir).
- **Yönlendirme yok.** Yönlendirme ile yanıt veren API izlenmez; böylece bir yönlendirme
  anahtarını başka bir sunucuya götüremez.
- **Özel adres yok.** Kendi makinendeki veya ağındaki adresler (`localhost`, `192.168.x.x`,
  `10.x.x.x`, bağlantı-yerel ve bulut meta veri adresleri) hem API için hem belgeyi indirmek
  için reddedilir; bir ad bunlardan birine çözülürse de.
- **Proxy yok.** Ortamın proxy ayarları bu istekler için kullanılmaz.
- **Boyut sınırları.** Belge en fazla 8 MiB, yanıt 256 KiB'ta (ya da daha azında) kesilir ve
  istek 30 saniyede zaman aşımına uğrar. Kesilen yanıt "kesildi" diye işaretlenir.
- **Gizli bilgi dışarıda kalır.** Anahtar yalnızca çağrı sırasında okunur; her sonuçta, hatada
  ve günlükte maskelenir. Anahtarı taşıyan başlık veya sorgu parametresi araçtan gizlenir ve
  bot tarafından ayarlanamaz.

## Sınırlar ve bilinmesi gerekenler

- Belgedeki yalnızca yerel `$ref`'ler çözülür. Dosya yükleyen (`multipart/form-data`)
  işlemler veya parametreleri çözülemeyen işlemler dışarıda bırakılır ve API altında sayılır.
- Kullanımdan kaldırılmış işlemler listelenmez.
- Her araç `api_<kimlik>_<operationId>` adını alır (belge `operationId` vermiyorsa
  `api_<kimlik>_<yöntem>_<yol>`).
- Bir API'nin tüm istekleri tek bir girişi kullanır. OAuth 2 akışları için erişim anahtarını
  Erişim anahtarı (Bearer) olarak yapıştır; Praxi onu yenilemez.
- API ve belge herkese açık bir adreste olmalı. Kendi ağındaki bir servis için şimdilik başka
  bir yol gerekir.
- Belge değişirse **Adresten güncelle**'yi seç. Hâlâ var olan işlemler açık kalır, kaybolanlar
  kapatılır.

## HTTP API

Aynı işlevler masaüstü uygulamasına ve sahip anahtarını taşıyan betiklere `/v1/openapi-apps`
altında açıktır ([`docs/api/openapi.json`](../../api/openapi.json)):
`GET`/`POST /v1/openapi-apps`, `GET`/`PUT`/`DELETE /v1/openapi-apps/{id}` ve
`POST /v1/openapi-apps/{id}/refresh`. Bu yollar yalnızca sahibe açıktır: düz API jetonu yetmez,
eşleştirilmiş telefon ve diğer cihazlar reddedilir. Anahtar yalnızca yazılabilir.

Ayrıca bak: [Uygulamaları bağlama](apps.md), [MCP sunucuları](mcp-servers.md),
[Botunu güvende tut](keep-your-bot-safe.md).
