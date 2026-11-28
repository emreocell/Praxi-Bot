# Sesli arama modu (eller serbest sohbet)

Masaüstü uygulamasındaki sesli arama modu, bir botla yazmadan konuşmanı sağlar. Sohbet yazı
alanındaki telefon düğmesine basarsın, konuşursun, botun yanıtını sesli duyarsın ve yeniden
konuşursun.

Arka planda sesli arama, yerel dikte yazıya çevirme uç noktasının (`POST /v1/voice/transcribe`)
aynısını kullanır. Yazıya çevrilen metnini normal bir sohbet mesajı olarak gönderir ve yanıtı,
işletim sisteminin konuşma motoruyla, o botun ayarlı sesi ve hızıyla (ya da Ayarlar'daki genel
varsayılanla) sesli okur. Daemonda ayrı bir ses yolu yoktur.

## Başlamadan önce

- **Ayarlar → Genel** altında bir dikte yazıya çevirme servisi ayarla (Whisper uyumlu ya da
  Gemini Live). Hiçbiri ayarlı değilse aramayı başlatmak hemen durur ve bir tane eklemeni
  isteyen bir mesaj çıkar.
- Praxi masaüstü uygulamasına mikrofon erişimi ver.
- Mümkünse kulaklık kullan. Uygulama WebView'dan yankı engelleme ister, ama hoparlörden
  mikrofona geri sızan ses botun sözünü erken kesmesine yine de yol açabilir.
- Sesli arama modu yalnızca **masaüstü uygulamasında** vardır. `bot` CLI'sinde ya da
  [Telegram](telegram.md)'da yoktur.

## Konuşmayı yazıya çevirme sağlayıcısını seçmek

**Ayarlar → Genel → Sesli dikte** altında, hem dikte hem sesli aramalar için geçerli bir
**Sağlayıcı** menüsü vardır:

- **Whisper uyumlu (OpenAI / yerel)**: OpenAI tarzı bir `/audio/transcriptions` servisi, bulutta
  ya da yerel bir sunucuda (varsayılan).
- **Gemini Live**: Google'ın Live API'si, yalnızca konuşmanı metne çevirmek için kullanılır. Bir
  Gemini API anahtarı yapıştır (Google AI Studio'dan) ve istersen başka bir Live model adı gir.
  Varsayılan `gemini-3.5-transcribe-live`. Anahtar doğrudan gizli bilgi deposuna gider ve bir
  daha gösterilmez. Google ile konuşan uygulama penceresi değil, daemondur ve her cümle için kısa
  bir bağlantı kurar. Yalnızca yazıya çevrilen metin kullanılır. Yanıt yine seçtiğin bot
  modelinden gelir, Live modelinin kendi yanıtı yok sayılır.
- **Bu bilgisayarda (çevrimdışı)**: Praxi-Bot'un yerel ses işleminde Silero konuşmayı algılar,
  Whisper yazıya çevirir. Bir model seç, indir ve sağlayıcıyı kaydet. Türkçe ve Almanca için en
  isabetli seçenek small modelidir. Silero dahil tiny yaklaşık 105 MB, base 162 MB ve small
  377 MB yer kaplar. Her iki model de MIT lisanslıdır. Bu sağlayıcı açıkken eşleştirilmiş
  kullanıcıların **Telegram ses notları** da bu bilgisayarda yazıya çevrilir (en çok 2 dakika),
  metin “🎤 ...” olarak geri yansıtılır ve sonra yazılmış metin gibi işlenir. Eğik çizgili
  komutlar ve onaylar için yine yazılmış metin ya da düğmeler gerekir. Başka herhangi bir
  sağlayıcıyla bot ses notunu okuyamadığını söyler. Ses notu bulut servisine gönderilmez.

Eller serbest aramada kaydı seçili bilgisayar yazıya çevirir. Orada bir bulut sağlayıcısı
ayarlıysa kayıt o sağlayıcıya gider. Ücretsiz anahtarlar Google'ın sınırlarına ve koşullarına tabidir.

## Çevrimdışı dikte ve sesli okuma

Arka plan hizmetini `cargo build -p cli --features local-voice` komutuyla derle. Standart yapıda
bu özellik kapalıdır. Modeller, sahip Ayarlar'da **İndir**'e bastığında indirilir. İndirmeden önce
boyut, kaynak ve lisans gösterilir. Praxi-Bot yeterli disk alanı olduğunu kontrol eder, 512 MiB
boş alan ayırır ve dosyaları kullanmadan önce sabit SHA-256 değerleriyle doğrular. Dosyalar
`<veri dizini>/models/voice/` altında saklanır. Bağlantı kesilirse devam etmek için yeniden
**İndir**'e bas. Model dosyaları kurulum paketinde yer almaz.

1. **Ayarlar → Genel → Sesli dikte** altında **Bu bilgisayarda (çevrimdışı)** seçeneğini kaydet.
   Sohbet yazı alanındaki mikrofon düğmesini veya **Mikrofon ve bas-konuş** ayarından açtığın
   kısayolu kullan. Algılanan metin yazı alanına gelir. Bu bilgisayarda çevrimdışı dikte açıksa
   eşleştirilmiş sunucudaki sohbetlerin yazı alanı da bu bilgisayarı kullanır.
2. **Ayarlar → Genel → Ses → Çevrimdışı sesli okuma** altında Supertonic 3'ü indir (yaklaşık
   146 MB, OpenRAIL-M) ve anahtarı aç. Yanıtın yanındaki **Sesli oku** düğmesini kullanabilir veya
   tamamlanan her yanıtın otomatik okunmasını açabilirsin. Eller serbest aramadaki ses, işletim
   sisteminin ses ayarını kullanır.

İki model de işlemcide çalışır. İlk ses birkaç saniye sürebilir. Büyük Whisper modeli daha fazla
işlemci süresi ister. Dikte ve sesli okuma Türkçe, İngilizce ve Almanca dahil uygulamanın dilini
izler. Supertonic uygulama dilini desteklemediğinde İngilizce okur. Kurulumdan sonra ses adımları
çevrimdışı çalışır. Sohbet yanıtlarını bot için ayarladığın dil modeli üretir.

Çevrimdışı sesin kullanılabilirliğini ve hangi model dosyalarının bulunduğunu görmek için
`bot doctor` çalıştır veya **Ayarlar → Tanılama** bölümünü aç. Ayarlar ve ses işlemi, model
dosyalarının SHA-256 değerlerini sırasıyla indirme durumunu gösterirken ve kullanmadan önce doğrular.

## Arama başlatmak ve kullanmak

1. Konuşmak istediğin botla bir sohbet aç.
2. Sohbet yazı alanının araç çubuğundaki telefon düğmesine (**Sesli sohbeti başlat**) tıkla.
3. Botun avatarı, bir mikrofon göstergesi (**Mikrofon açık** ya da **Mikrofon susturuldu**) ve
   canlı bir durum satırıyla, ekranı kilitlemeyen bir **Sesli arama** paneli açılır.

Telefon düğmesi hep kullanılabilir kalır. Kurulum eksikse panel hangi anahtarı ya da sağlayıcıyı
ayarlaman gerektiğini, sahip yetkisini ya da daemon bağlantısını nasıl geri getireceğini veya
mikrofon erişimine Windows Ayarları → Gizlilik ve güvenlik → Mikrofon bölümünden (masaüstü
uygulamalarının erişimi dahil) nasıl izin vereceğini anlatır. Sorunu giderdikten sonra panelde
yeniden dene ya da telefona yeniden tıkla. Bekleyen bir mikrofon izni istemi, aramanın sonsuza
kadar “bağlanıyor” durumunda kalmasına yol açmaz, zaman aşımına uğrar.

Bu sohbette zaten çalışan bir bot varsa o çalışmayı bitir ya da durdur, sonra aramayı yeniden
dene. Bu açıklama görünürken mikrofon kapalı kalır. Arama sırasında telefona tıklamak var olan
paneli öne getirir. **Aramayı bitir** mikrofonu bırakır.

Arama şu adımları kendiliğinden tekrarlar:

1. **Dinliyor**: Uygulama, enerjiye dayalı ses algılama (VAD) ile mikrofonun ses düzeyini izler.
   Konuşma, izlenen oda gürültüsü eşiğinin üstünde 150 ms sürdüğünde bir tur başlatır ve 1,2 sn
   sessizlikten sonra bitirir. 350 ms'den kısa kayıtlar gürültü sayılıp atılır. 10 sn konuşma
   olmazsa *“Konuşma algılanmadı. Dinlemeye devam ediyorum.”* görünür. Her cümle en çok 120 sn
   ile sınırlıdır.
2. **Düşünüyor**: Kaydedilen parça, bellekte ayarladığın yazıya çevirme servisine gönderilir ve
   sonra atılır. Konuşma tanındıysa sohbette sıradan bir kullanıcı mesajı olarak gönderilir.
3. **Konuşuyor**: Botun yanıtı akarken tamamlanan cümleler işletim sisteminin konuşma motoruna
   sıraya konur. Böylece ses ilk cümleyle başlar (kod blokları ve tablolar okunmaz).

### Sözünü kesmek ve arama denetimleri

Bot konuşurken en az 400 ms yüksek sesle konuşman ya da **Sözünü kes**'e tıklaman, sesli yanıtın
kalanını durdurur ve yeniden dinlemeye geçer.

Panel denetimleri:

- **Mikrofonu sustur / Mikrofonu aç**: Mikrofonun ses kanalını kapatır ya da yeniden açar.
- **Basılı tutarak konuş**: Otomatik ses algılamayı kapatır ve bir **Konuşmak için basılı tut**
  düğmesi gösterir (düğmeye odaklıyken `Space` ya da `Enter` tuşunu basılı tutarak da çalışır).
  VAD'ın erken ya da geç devreye girdiği gürültülü odalarda işe yarar.
- **Altyazılar**: Senin söylediklerinin ve botun söylediklerinin canlı altyazılarını açar ya da
  kapatır.
- **Sözünü kes**: Sürmekte olan sesli yanıtı durdurur (bot konuşurken görünür).
- **Aramayı bitir**: Mikrofonu bırakır ve paneli kapatır. Panelde `Esc`'ye basmak ya da sohbeti
  kapatmak da aramayı bitirir.

## Neler yapabilir, neler yapamaz

- **Yapabildikleri**: Çok turlu, eller serbest sohbetler yürütür, anında bas-konuş moduna geçer,
  yanıtları botun seçili sesiyle cümle cümle okur ve canlı altyazı gösterir.
- **Yapamadıkları**:
  - Ayarlı bir yazıya çevirme servisi ya da çalışan bir mikrofon olmadan çalışmaz.
  - Araç çağrılarını sesle onaylayamaz ya da reddedemez (aşağıya bak).
  - Oda gürültüsünü cihaz başına kendiliğinden ayarlamayı henüz bilmez. VAD konuşmayı kaçırırsa
    ya da arka plan gürültüsüne takılırsa **Basılı tutarak konuş**'u kullan.
  - Otomatik sözü kesmenin hemen ardından gelen ilk heceleri yakalayamaz, çünkü turun kaydedicisi
    arama yeniden dinlemeye geçtiğinde başlar.

## Gizlilik ve güvenlik

- **Mikrofon denetimi açıkça sende**: Mikrofon yalnızca telefon düğmesine tıkladığında açılır,
  yalnızca arama paneli görünürken açık kalır ve **Aramayı bitir**'e tıkladığında, `Esc`'ye
  bastığında, sohbetten çıktığında ya da ölümcül bir hata olduğunda bırakılır. Susturmak ses
  kanalının kendisini kapatır.
- **Diske ses kaydedilmez**: Kaydedilen ses, ayarladığın yazıya çevirme servisine gönderilip
  atılana kadar yalnızca bellekte durur. Asla diske yazılmaz ve günlüğe geçmez. Altyazılar
  yalnızca bileşenin belleğinde kalır ve arama bitince kaybolur.
- **Onaylar yalnızca tıklamayla verilir**: Yazıya çevrilen konuşma her zaman yalnızca düz bir
  sohbet mesajı olarak gönderilir. Sesli arama modu hiçbir onay API'sini çağıramaz. Bir bot bir
  araç çağrısı için izin istediğinde arama **Onayınız bekleniyor** durumuna girer, mikrofon
  girişini yok sayar ve sohbetteki kartı kullanmanı hatırlatır. Panel ekranı kilitlemediği için
  onay kartı tıklanabilir kalır. *“Evet, onayla”* demenin hiçbir etkisi yoktur.
- **Yanıtlar veridir**: Botun yanıtları yalnızca sesli okunur. Bir yanıttaki metin, arama
  eylemlerini tetikleyemez.
