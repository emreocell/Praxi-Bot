# Sık görülen sorunları çöz

Gördüğün iletiyi bul. Adımları sırayla yap. Aşağıdaki ileti başlıkları, uygulamanın gösterdiği sözlerdir.

Hiçbiri işe yaramazsa: profil menüsünden **Yardım**'ı aç, **Kopyalanacakları göster**'e bas, oku, sonra **Kopyala**'ya bas. Anahtarlar, parolalar ve e-posta adresleri gizlenir. Hiçbir yere bir şey gönderilmez. Metni bir hata raporuna yapıştır. Teknik [Sorun giderme](troubleshooting.md) sayfasına da bak.

## 1. "Praxi'nin yerel servisi durdu" ya da "Praxi yerel servisine ulaşamıyor"

Praxi'nin arka plan parçası çalışmıyor.

1. 10 saniye bekle. Hâlâ açılıyor olabilir ("Praxi'nin yerel servisi açılıyor"). Praxi kendiliğinden yeniden dener.
2. **Yerel servisi yeniden başlat**'a (ya da **Servisi yeniden başlat**'a) bas.
3. Hâlâ olmadı mı? Praxi'yi tamamen kapat ve yeniden aç.

Botların ve sohbetlerin güvende.

## 2. "Windows Praxi'nin servisini engelledi" ya da "bir Windows bileşeni eksik"

Antivirüsün ya da Windows servisi başlatmayı reddetti, ya da bir Windows parçası eksik.

1. Engellendiyse: antivirüsünü ya da SmartScreen'i aç ve Praxi klasöründeki `bot.exe`'ye izin ver. Sonra **Yeniden dene**'ye bas.
2. Bileşen eksikse: Microsoft'tan "Microsoft Visual C++ Redistributable (x64)" kur. Sonra **Yeniden dene**'ye bas.
3. "Başka bir program Praxi'nin ihtiyaç duyduğu bağlantıyı kullanıyor": o programı kapat ya da bilgisayarı yeniden başlat.
4. Tekrarlarsa: **Ayrıntıları kopyala**'ya bas ve ayrıntıları gönder. **Günlük klasörünü aç** günlüğü gösterir.

## 3. Bot yanıt vermiyor ya da "Model yanıt veremedi"

Yapay zekâ servisi yanıt vermedi.

1. İletinin altındaki ayrıntıları oku. Servisin ne bildirdiğini söylerler.
2. **Yeniden dene**'ye bas.
3. **Ayarlar → Yapay zekâ ve hafıza**yı aç ve anahtarının orada olduğunu kontrol et. Yoksa yeniden ekle.
4. Başka bir model seç ve yeniden dene.

## 4. "bu anahtarı kabul etmedi"

Anahtar tam kopyalanmamış ya da başka bir servise ait.

1. Anahtar sayfasına dön ve anahtarı yeniden kopyala. Satırın tamamını kopyala.
2. Yeniden yapıştır. Kutu hemen kontrol eder.
3. Doğru servisi seçtiğinden emin ol (örneğin Gemini anahtarı için Google Gemini).
4. Beyin olarak bir kodlama aracı kullanıyorsan: "O kodlama aracında oturumun kapalı" iletisi, **Ayarlar**'dan ona giriş yapman gerektiği anlamına gelir. "O aboneliğin kullanım sınırı doldu" iletisi, sıfırlanmasını beklemen ya da başka bir model seçmen gerektiği anlamına gelir.

## 5. "… servisine ulaşılamadı" ya da "Bu çok uzun sürdü"

Büyük olasılıkla sorun internet bağlantında.

1. İnternetinin bir tarayıcıda çalıştığını kontrol et.
2. **Yeniden dene**'ye bas.
3. VPN ya da şirket ağı kullanıyorsan bir an için kapat ve yeniden dene.

Bir mesaj gitmediyse ("Mesajın gönderilmedi") bağlantını kontrol et ve yeniden gönder.

## 6. "Bu adıma izin vermedin"

**Reddet**'e bastın ya da kartın süresi doldu ve yanıt hayır oldu.

1. Bota yeniden iste.
2. **Onay gerekiyor** kartı çıkınca oku ve **Bir kez izin ver**'e bas.
3. Güvendiğin bir şey için her seferinde sorulmasını istemiyor musun? Kartta **Her zaman**'ı kullan. Bkz. [Botunu güvende tut](keep-your-bot-safe.md).

## 7. "Bot aradığı dosyayı ya da programı bulamadı" ya da "… kurulu değil"

1. Yeniden iste ve dosyanın tam adını ya da klasörünü söyle.
2. İletide bir program adı geçiyorsa o programı kur. Sonra yeniden iste.
3. Bota dokunmaması gereken bir klasörü kullanması söylendiyse onu güvenilen klasör olarak ekle: **Ayarlar → Güvenlik**.

## 8. "Güvenli mod açık"

Güvenli mod araçları, iş akışlarını ve zamanlamaları kapatır. Sorunları gidermek içindir.

1. Praxi'yi kapat.
2. Normal şekilde aç (güvenli modda değil).

Her şey geri gelir. Hiçbir şey silinmedi.

## 9. "… yanıt vermedi" ya da "Bot hedef bilgisayara ulaşamadı"

Bir sunucu ya da başka bir bilgisayar kapalı veya bağlı değil.

1. Aç ve ağını kontrol et.
2. Bir dakika bekle ve yeniden dene.
3. O bilgisayara ihtiyacın yok mu? Bilgisayar menüsünde **Bu bilgisayar**'ı seç.

Daha fazlası: [Bu bilgisayarda mı, sunucuda mı çalışsın](local-or-server.md).

## 10. "Bot limitine ulaşıldı" ya da "Bu bot limitine yaklaştı"

Bir harcama limiti koydun ve bot onu kullandı. Yapay zekâ servisi çağrılmadı.

1. Botun ayarlarını aç ve limiti yükselt ya da sıfırlanmasını bekle.
2. Ya da koşuyu durmuş bırak.

Bkz. [Bot harcama limitleri](bot-spending-limits.md).

## 11. Bir uygulama "Hata", "Oturum açılması gerekiyor" ya da "Karantinada" gösteriyor

1. **Uygulamalar**'ı aç, uygulamayı bul ve **Yönet**'e bas.
2. **Oturum açılması gerekiyor**: **Oturum aç**'a bas.
3. **Hata**: satırın altındaki ayrıntıları aç ve yazanı yap.
4. Sonra **Yeniden bağlan**'a bas.
5. "Composio henüz kurulmadı": bir uygulamada **Bağlan**'a bas ve bir kez giriş yap.
6. "Bir araç, onayladığınızdan sonra değişti": iki metni de oku. **Yeni sürüme güven**'e yalnızca uygulamaya güveniyorsan bas.

Bkz. [Uygulamalarını bağla](connect-your-apps.md).

## 12. Zamanlanmış bir iş çalışmadı ya da "Bu zamanlama geçerli değil"

1. İşler yalnızca Praxi açıkken ve bilgisayar uyanıkken çalışır. Kapalıysa iş kaçırılmış görünür. **Yeniden dene**'ye bas.
2. **Zamanlanmış görevler**'i aç ve işin duraklatılmadığını kontrol et. **Sonraki 5 çalışma** listesi ne zaman çalışacağını gösterir.
3. "Bu zamanlama geçerli değil": saati ya da tekrar kuralını kontrol et. Sözlerle yazmak yerine **Kolay** sekmesini kullan.
4. Güvenli mod açık mı? Bkz. sorun 8.
5. İş senin onayını bekliyor olabilir. **Senden bekleyenler**'e bak.

Bkz. [İşleri otomatikleştir](automate.md).

## 13. Praxi ana ekran yerine bir başlangıç kontrolü gösteriyor

Praxi başlayamadığında, bağlı olduğu şeyleri kontrol eder ve yanlış olan ilkini tek bir ana düğmeyle söyler. Kontrol sırasında hiçbir şey değiştirilmez ve hiçbir yere bir şey gönderilmez.

- **Windows'ta Praxi'nin penceresi için gereken bir parça eksik** (Microsoft WebView2): **WebView2'yi Microsoft'tan indir**'e bas. Microsoft'un resmî sayfası açılır. Kur, sonra Praxi'yi yeniden başlat. WebView2 yoksa Praxi kapanmadan önce yalnızca küçük bir mesaj kutusu gösterebilir: orada **Evet**'i seçersen aynı sayfa açılır.
- **Praxi'nin ihtiyaç duyduğu bir dosya yok**: antivirüs yazılımları `bot.exe` dosyasını sık sık karantinaya alır. Antivirüs programında geri yükle ya da Praxi'yi yeniden kur, sonra **Yeniden dene**'ye bas.
- **Praxi veri klasörüne kaydedemiyor**: **Veri klasörünü aç**'a bas. Klasörün var olduğunu, hesabının değiştirebildiğini ve diskin dolu ya da salt okunur olmadığını kontrol et.
- **Başka bir program Praxi'nin kapısını kullanıyor**: onu kapat ya da bilgisayarı yeniden başlat, sonra **Yeniden dene**'ye bas.
- **Saat yanlış görünüyor**: Windows'ta doğru tarih ve saati ayarla, sonra **Yeniden dene**'ye bas.

Hata bildirimi için bir rapor kopyalamak istersen **Ayrıntıları kopyala**'ya bas. **Gelişmiş** teknik bilgileri gösterir; kullanıcı adın ve ana klasörün yerine `~` ve `<user>` yazılır.
