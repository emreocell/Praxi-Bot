# Bir şey olduğunda

Bot panelinde **Otomasyon → Bir şey olduğunda** bölümünü açın. Yeni e-posta,
imzalı çağrı, bir akışta yeni öğe, bir web sayfasında değişiklik, başlamak üzere
olan bir takvim etkinliği, durumu değişen bir Home Assistant cihazı veya zamanlama
için kural ekleyin. “İncelemem için yanıt taslağı
hazırla” gibi bir talimat yazın. Kaydedip gözden geçirdikten sonra açın. Yeni
kurallar her zaman kapalı başlar.

Mevcut posta ve talimat zamanlamaları ile imzalı adresler aynı listede görünür. Sessiz saat ve sınır eklemek için birini açın; kaydetmek, kuralı inceleyip yeniden açana kadar durdurur. Gelişmiş iş akışı, ön kontrol ve dosya izleme zamanlamaları Zamanlamalar bölümünde kalır.

E-posta için önce bir IMAP posta kutusu bağlayın ve botun zamanlanmış posta
okumalarına izin verin. Klasörü, göndereni (tam adres veya alt alan adlarını da kapsayan alan adı; metin parçası
olmaz),
isteğe bağlı konu ve ek filtresini seçin. Varsayılan kontrol aralığı iki
dakikadır; bir dakikadan kısa olamaz. İlk kontrol başlangıç noktası oluşturur;
eski postalar çalıştırma başlatmaz. Birlikte gelen postalar tek çalıştırmada
başlık listesi olur. Arama en yeni 100 eşleşmeyle sınırlıdır; teslimat garantisi
vermez. AgentMail gerekli döngü önleme başlıklarını sunmadığı için düzenleyici
yalnızca IMAP kutularını gösterir. Ek filtresi sunucunun MIME yapısı bilgisine
bağlıdır; bilinmeyen bilgi eşleşmiş sayılmaz.

Kutunun kendi adresinden gelen postalar, otomatik yanıtlar, toplu/liste postaları
ve abonelikten çıkma başlığı taşıyan postalar atlanır. Gönderen filtresinde
açıkça belirtilmeyen no-reply adresleri de atlanır. Gönderen filtresi kimlik
doğrulamaz. E-posta sohbet kanalı bağımsızdır: aynı kutu için ikisini birden
açarsanız hem sohbet hem kural çalıştırması başlayabilir.

Çağrılar için **İmzalı adresler** altında adres oluşturup kurala bağlayın.
İmza, tekrar saldırısı koruması ve çağrı sınırları geçerliliğini korur. Bağlı
adres önceki talimatı yerine bu kurala olay ekler. Kural kapalıyken çağrılar
reddedilir; önceki talimata geri dönülmez. Kuralı kaldırmak bağı kaldırır.

## Akışlar, web sayfaları, takvim etkinlikleri ve Home Assistant

Bu dört kural e-posta gibi çalışır: ilk kontrol yalnızca var olanları not eder,
bu yüzden eski öğeler çalıştırma başlatmaz; peş peşe olaylar tek çalıştırmada
birleşir. Aşağıdaki sessiz saatler, sınırlar, kuyruk, **Örnekle dene** ve onay
davranışı bunlar için de geçerlidir. Düzenleyici bunları yalnızca sizin
oluşturmanıza izin verir: bir bot oluşturamaz; zamanlama API'si ve araçları
bunları reddeder.

**Bir akışta yeni öğe.** Bir RSS veya Atom adresi verin. Yeni öğe botu başlığı,
bağlantısı, tarihi ve kısa bir özetiyle (yazının tamamıyla değil) başlatır.
Varsayılan kontrol 30 dakikada bir; izin verilen en sık aralık 15 dakikadır.

**Bir web sayfası değişiyor.** Adresi ve izlenecek bölüm için bir CSS seçici
(örneğin `#price` veya `h1.title`) verin. Seçilen metin son kontrolden
farklıysa bot bu metni alır (en çok 4.000 karakter). Seçilen metni sürekli
değişen sayfalar, örneğin bir saat, her kontrolde çalıştırma başlatır; daha
durağan bir bölüm seçin. Seçici hiçbir şey bulamazsa veya sayfa okunamazsa
kontrol “metin boşaldı” demek yerine başarısız sayılır; art arda on başarısız
kontrol kuralı duraklatır. Varsayılan saatte bir, en sık 15 dakikada bir.

İkisi de yalnızca kaydettiğiniz adresi getirir; verinin içinde bulunan bir
bağlantıyı asla izlemez. İstekler botun gezinme araçlarındaki web korumasıyla
çıkar: yalnızca `http` ve `https`; bu bilgisayara, yerel ağınıza, link-local
veya bulut meta veri adreslerine asla (her yönlendirmede ve çözülen her adreste
denetlenir), en çok üç yönlendirme, 15 saniye sınırı, akış için 1 MB ve sayfa
için 2 MB. Adres kullanıcı adı veya parola taşıyamaz. Botun web'i kullanabiliyor
olması gerekir: güvenli mod, kilitli veya reddedilmiş web kategorisi ya da web
içermeyen bir profil kuralı durdurur. Kuralı adresiyle kaydetmek, tam olarak o
adres için onayınızdır; her kontrolde web için yeniden sorulmaz.

**Bir takvim etkinliği başlamak üzere.** Bir takvim ve kaç dakika önce
(1–1.440) seçin. Kural, botun okuyabildiği bağlı bir takvim ister; posta
okumalarında olduğu gibi zamanlanmış çalıştırmalar için izin verilmiş olmalıdır.
Yaklaşık iki dakikada bir kontrol eder; bu yüzden çalıştırma seçtiğiniz süreden
birkaç dakikaya kadar geç başlayabilir ve her etkinlik için bir kez çalışır.
Başka bir zamana taşıdığınız etkinlik yeni sayılır. Kuralı açtığınızda zaten bu
aralığa girmiş etkinlikler, tüm gün süren etkinlikler ve “boş” işaretli
etkinlikler başlatmaz. Bot başlığı, başlangıcı, başlamasına kalan dakikayı ve
yeri alır; açıklamayı ve katılımcıları asla almaz.

**Bir Home Assistant cihazı değişiyor.** Varlığı (örneğin
`binary_sensor.front_door`) ve isterseniz geçmesi gereken durumu (`on`,
`unlocked` …) girin. Home Assistant bağlı olmalı ve bota izin verilmiş olmalıdır;
aksi halde kural kaydedilemez. Yaklaşık dakikada bir kontrol eder ve yalnızca
durumu karşılaştırır; parlaklık veya ölçüm gibi öznitelikleri karşılaştırmaz.
Bir dakika içinde gidip gelen değişiklikler kaçabilir. Bot varlığı, adını, yeni
durumu ve öncekini alır.

Dördünün olay metni de başkası tarafından yazılmıştır. Kısaltılır, denetim ve
görünmez karakterlerden arındırılır ve her olay gibi güvenilmeyen veri olarak
teslim edilir. Art arda on kontrol başarısız olursa kural duraklar ve “Dikkat
gerekiyor” görünür.

Sessiz saatler seçtiğiniz saat dilimini ve yaz saati değişikliklerini izler.
Olayları atlayabilir veya pencere bitince tek özet için saklayabilirsiniz.
Varsayılan sınır kural başına son bir saatte 10, son 24 saatte 50 çalıştırmadır;
botun tüm kuralları için 30 ve 150'dir. Bot için tanımlanan en düşük tavan tüm
kurallarına uygulanır. Fazla olaylar sonraki özette birleşmek üzere bekler.
Kuyruk ve sayaçlar yeniden başlatmada korunur. “Bugün” sayısı UTC'ye göredir.

Her kural en çok 100 olay ve 48 KB saklar. Posta kuyruğu dolarsa veya çalıştırma
başarısız olursa kural duraklar. Art arda on posta kontrolü hatası da duraklatır.
Yeniden açmadan önce etkinliği, bağlantıyı ve filtreleri kontrol edin. Kuralı
düzenlemek bekleyen eski olayları siler ve posta başlangıç noktasını yeniler;
hız sayaçları korunur. Bir grup ayrıldıktan hemen sonra süreç çökerse o grup
kaybolabilir; tam bir kez teslimat garantisi yoktur. Önemli postaları kutuda tutun.

**Örnekle dene**, botun alacağı tam talimatı ve aynı boyut sınırına sahip
korumalı olay metnini gösterir. Model çağırmaz, araç çalıştırmaz. Posta kuralları
yalnızca başlıkları okur; gövdeyi ve ekleri indirmez. Çağrı metni 4.096 karakterle,
son olay bloğu 12 KB ile sınırlıdır. Başlıklardaki görünmez karakterler temizlenir.
Olay metni, talimatınızı yok saymayı istese bile, güvenilmeyen veridir.

Kuralları yalnızca yerel sahip oluşturabilir, düzenleyebilir veya açabilir.
Bot kendi izinleriyle çalışır; dış olay çalıştırmalarında zamanlayıcının tek
seferlik onay tabanı da korunur. Onay gereken işlem bekler ve bildirim gösterir.
Hiçbir işlem otomatik onaylanmaz. Parolalar kurala yazılmaz; posta kutusunun
veya imzalı adresin mevcut yapılandırmasında kalır.
