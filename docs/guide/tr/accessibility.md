# Erişilebilirlik

Praxi yalnızca klavyeyle, ekran okuyucular gibi yardımcı teknolojilerle, açık, koyu ve yüksek kontrastlı temalarda, ayrıca azaltılmış hareket ya da azaltılmış saydamlık açıkken kullanılabilecek biçimde tasarlandı.

Kısayolların kısa başvuru tablosu için [Klavye kısayolları](keyboard-shortcuts.md) sayfasına bak. Uygulamadaki kısayol sayfasını istediğin zaman da açabilirsin: metin alanlarının dışında **`?`** tuşuna bas, komut paletinden (`Ctrl/Cmd+K`) **Klavye kısayolları**'nı çalıştır ya da sohbetin yazı alanına `/help` yaz.

---

## Klavyeyle gezinme

### Atlama bağlantısı ve ana bölgeler

- **Ana içeriğe geç**: Pencerenin en üstünden `Tab`'a basınca kenar çubuğunun sol üstünde bir **Ana içeriğe geç** düğmesi belirir. Ona `Enter` ile basarsan odak sol kenar çubuğunu atlayıp ana bölmedeki asıl denetime (örneğin sohbetin yazı alanına ya da görünümün başlığına) geçer.
- **Anlamlı ana bölgeler**:
  - Soldaki bot şeridi, `<nav>` içeren ve sıra değişikliklerini sesli duyuran bir `<aside>` bölgesidir.
  - Ortadaki bölme bir `<main>` bölgesidir (`#main-content`).
  - Sağdaki bot paneli (`BotPanel`) etiketli, tamamlayıcı bir `<aside>` bölgesidir (`id="bot-panel"`) ve `Esc` ile kapanır.
  - Açılır pencereler (`Settings`, `Help`, `New Bot`, `Edit Bot`, `Bot Profile`, `Skills`, `Add Skill`, `Connected Apps`, `Command Palette`) `role="dialog"` ve `aria-modal="true"` ile çizilir, arkadaki kabuğu `inert` yapar, `Tab` / `Shift+Tab` tuşlarını etkin katmanın içinde tutar ve kapanınca odağı kendisini açan denetime geri verir.

### İlk kurulum sihirbazı

İlk kurulum sihirbazının üç adımı da yalnızca klavyeyle kullanılabilir (**Bir yapay zekâ bağla**, **Ne kadar serbest olsun?**, **Botunla tanış**):

1. **Bir yapay zekâ bağla (`brain`)**:
   - Dil düğmeleri (`TR`, `EN`, `DE`) ve **Bunu sonra eklerim** çerçevenin üstünde, doğal `Tab` sırasında durur.
   - Bulunan CLI motorlarına, bulut ve yerel sağlayıcı kutucuklarına, uyumluluk etiketlerine ve API anahtarı alanına (bağlanmak için `Enter`) `Tab` ve `Shift+Tab` ile ulaşılır.
   - Adımlar arasında geçerken (**Devam et** / **Geri**) odak adımın `<h1>` başlığına gider. Böylece ekran okuyucu yeni adımın başlığını ve adım ilerlemesini (`Step X of 3`) okur.
2. **Ne kadar serbest olsun? (`freedom`)**:
   - Özerklik seviyesi kartları (**Her zaman sor**, **Dengeli**, **Tam özerklik**) gezinilebilir `tabIndex` ile bir `role="radiogroup"` oluşturur.
   - Özerklik seviyeleri arasında geçmek için `Left` / `Up` ve `Right` / `Down` ok tuşlarını, **Geri** ve **Devam et** düğmelerine ulaşmak için `Tab`'ı kullan.
3. **Botunla tanış (`bot`)**:
   - İlk odak bot adı alanına gelir.
   - 16 karakter silüeti ve hazır renk örnekleri gezinilebilir `tabIndex` kullanır: şekil ızgarasına ya da renk satırına girmek için `Tab`'a bas, bir şekil ya da renk seçmek için `Left` / `Right` / `Up` / `Down` ok tuşlarını kullan. Bir `Tab` daha özel renk seçiciye (`<input type="color">`) götürür, ardından **Geri** ve **Başla** gelir.

### Sohbet, adım listesi ve onay kartları

- **Yazı alanı**:
  - Varsayılan olarak `Enter` mesajı gönderir, `Shift+Enter` yeni satır ekler (**Ayarlar → Genel → Mesaj gönder** içinden `Ctrl/Cmd+Enter`'a çevrilebilir).
  - Yazı alanı boşken ya da ilk satırdayken `Up` / `Down` önceki kullanıcı mesajlarını geri getirir (`Esc` taslağı geri yükler).
  - `/` ya da `@` yazmak, eğik çizgi komutu ya da bot bahsetme tamamlama listesini açar (gezmek için `Up` / `Down`, seçmek için `Enter` ya da `Tab`, kapatmak için `Esc`).
- **Mesaj listesi ve adım listesi**:
  - Mesaj kaydırıcısı (`role="log"`, `aria-live="polite"`, `aria-relevant="additions"`) `PageUp` / `PageDown` ve ok tuşlarıyla kaydırmak için klavyeyle odaklanabilir (`tabIndex={0}`).
  - Herhangi bir mesaj satırına odaklanmak eylem çubuğunu açar (**Kopyala**, **Sesli oku**, **Düzenle ve yeniden gönder**, **Mesaj bilgisi**). Düğmelerin tıklama alanı en az 24×24 pikseldir.
  - Düşünme zincirleri ve araç adımı grupları, odaklanabilir açma düğmeleri (`aria-expanded`) kullanır. Böylece her ara adım ve kod çıktısı `Enter` ya da `Space` ile incelenebilir.
- **Onay, soru ve gizli bilgi isteği kartları**:
  - Bir çalışma onay, açıklayıcı bir soru ya da kimlik bilgisi için beklemeye geçtiğinde, yazı alanının üstündeki çalışma durum satırında bekleyen kartı görüşe kaydıran bir **Göster** düğmesi çıkar (`prefers-reduced-motion: reduce` ayarına uyar).
  - Onay kartları eylem düğmelerini (**Onayla**, **Bu oturum için izin ver**, **Her zaman izin ver**, **Reddet**) mantıklı bir `Tab` sırasında toplar. Gizli bilgi isteği formları `Esc` ile iptal edilir.

### Sağ panel (`BotPanel`) ve boyutu değişen şeritler

- **Boyutu değişen kenar çubukları**: Soldaki bot şeridinin de sağdaki bot panelinin de klavyeyle odaklanabilen boyutlandırma tutamaçları vardır (`role="separator"`, `aria-orientation="vertical"`). Bir tutamağa odaklanıp `Left` / `Right` ok tuşlarına basarak sütunu adım adım yeniden boyutlandırabilirsin.
- **Bot listesini yeniden sıralama**: Açık sol şeritteki her bot satırında klavyeyle kullanılan bir sıralama tutamağı düğmesi vardır. Tutamağa odaklanıp `Up` / `Down` (ya da `Home` / `End`) tuşlarına basarak botu taşırsın. Yeni konumu sesli bir canlı alan duyurur.
- **Bot paneli sekmeleri**: Sağ paneldeki **Şimdi** ve **Kitaplık** sekmeleri, gezinilebilir `tabIndex` ve `Left` / `Right` / `Home` / `End` tuşlarıyla `role="tablist"` olarak çalışır. Sağ panelin içinde herhangi bir yerde `Esc`'e basmak paneli kapatır ve odağı başlık çubuğundaki bot simgesine geri verir.

### Ayarlar (`SettingsModal`)

- **Açma**: `Ctrl/Cmd+,` tuşlarına bas, profil menüsünden **Ayarlar**'ı seç ya da başlık çubuğundaki dişli düğmesine tıkla. Odak **Ayarlarda ara** alanında başlar (`data-autofocus`).
- **Bölümler arasında gezinme**: `Tab` ile soldaki bölüm listesine geç, sonra **Genel**, **Görünüm**, **Yapay zekâ ve modeller**, **Bilgisayar**, **Posta ve takvim**, **Kullanım**, **Tanılama** ve **Güncellemeler** arasında geçmek için `Up` / `Down` / `Home` / `End` ok tuşlarını (ya da `Tab` ve `Enter` / `Space`) kullan.
- **Denetimler**:
  - Bölmeli hap denetimleri (`Segmented`) ve bilgisayar hedef kartları (`ComputerTargetBlock`), gezinilebilir `tabIndex` ve `Left` / `Right` / `Up` / `Down` ok tuşlarıyla `role="radiogroup"` kullanır.
  - Aç/kapat anahtarları `aria-checked` ile `role="switch"` kullanır ve `Space` ya da `Enter` ile değişir.

---

## Ekran okuyucu duyuruları ve canlı alanlar

Ekran okuyucunun çıktısı gürültüsüz ama bilgilendirici kalsın diye:

- **Akan çalışma durumu**: `RunStatusLine` içindeki durum cümlesi yalnızca kendi üzerinde `role="status"`, `aria-live="polite"` ve `aria-atomic="true"` kullanır. 1 saniyelik geçen süre sayacı (`aria-hidden="true"`) ve eylem düğmeleri canlı metin öğesinin dışında durur, yani akan saniyeler tekrar tekrar duyuruya yol açmaz.
- **Onay istekleri ve durdurma uyarıları**: Bir çalışma onay beklemeye geçtiğinde ya da bir durdurma isteği doğrulanamadığında veya başarısız olduğunda, durum cümlesi `role="alert"` (`aria-live="assertive"`) olur ve kullanıcıya hemen haber verilir.
- **Onay ve gizli bilgi geri sayım süreleri**: Onay kartlarındaki ve gizli bilgi isteği kartlarındaki saniyelik süre geri sayımları `role="timer"` ile `aria-live="off"` kullanır, böylece ekran okuyucuyu her saniye bölmez ve rahatsız etmez.
- **Bildirim mesajları**: Kısa süreli bildirimler, sessiz bir durum bölgesinin içinde çizilir (`role="status"`, `aria-live="polite"`).

---

## Odak göstergesi, temalar ve kontrast

- **Görünür odak halkaları**: Etkileşimli denetimler `:focus-visible` çerçeveleri ve `--shadow-focus` (`0 0 0 2px var(--color-background), 0 0 0 4px var(--color-focus)`) kullanır. Böylece klavye odağı, her yüzeyin ve arka planın üstünde görünür kalan iki tonlu bir hale taşır.
- **Tema ve kontrast denetimi**: Yüksek kontrastlı **Yüksek kontrast koyu** (`high-contrast`) ve **Yüksek kontrast açık** (`high-contrast-light`) temaları dahil, yerleşik 13 temanın hepsi CI'da denetlenir. `pnpm --dir apps/desktop/ui check:themes` WCAG metin, soluk metin, kenarlık ve 3:1 odak halkası kontrastını zorunlu kılar. `pnpm --dir apps/desktop/ui check:a11y` ise ekranlar ve temalar boyunca Playwright ile `axe-core` WCAG 2.1 A/AA denetimlerini çalıştırır.

---

## Azaltılmış hareket ve azaltılmış saydamlık

- **Azaltılmış hareket (`prefers-reduced-motion: reduce`)**:
  - İşletim sistemi azaltılmış hareket istediğinde CSS anahtar kare animasyonları (dönen göstergeler, durum atışları, mesaj giriş geçişleri, açılır kutu yükselme animasyonları, ilk kurulum geçişleri) ve üzerine gelince oluşan dönüşümler kapatılır.
  - Kodla yapılan kaydırma (`scrollToEnd`, bekleyen onaya atlayan **Göster** düğmesi, gelişmiş sağlayıcı ayrıntılarını açma) `prefers-reduced-motion: reduce` etkinken `"smooth"` yerine `"instant"` kullanır.
  - Üretilmiş bot avatarları azaltılmış hareket açıkken nefes alma, göz kırpma ve imleci gözle izleme hareketlerini kapatır.
- **Yüzey stili ve saydamlık**:
  - Buzlu cam ya da arka planlar yazıyı okumayı zorlaştırıyorsa **Ayarlar → Görünüm** sayfasını aç. **Panel stili**'ni **Düz** ya da **Çizgi** (`line`) yap, sohbet alanını düz yap (**Saydam sohbet alanı**'nı kapat), **Arka plan**'ı **Yok** yap ya da **Yüksek kontrast koyu** / **Yüksek kontrast açık** temasını seç.
