# Beceriler

**Beceri**, bir bota zor bir işin nasıl yapılacağını öğreten kısa bir Markdown rehberidir. Örneğin
“özel bir açılır liste nasıl kullanılır” ya da “Windows Ayarları'nda nasıl gezinilir”. Beceriler kod
değildir. Botun, işe yaradıkları zaman okuduğu yönergelerdir.

Bot, mevcut becerilerin kısa bir **dizinini** (ad ve tek satırlık açıklama) her zaman görür. Bir
becerinin tam metnini yalnızca gerektiğinde, `skill_list`, `skill_read` ve `skill_hint` araçlarıyla
yükler. Bot, öğrendiği bir şeyi hatırlamak için kendisi de bir beceri **yazabilir** (`skill_write`).
Bunlar o botun kendi klasörüne kaydedilir: `<data dir>/bots/<bot-id>/skills/`.

## Beceriler nerede durur

Sonraki katmanlar, aynı ada sahip önceki katmanın yerine geçer:

1. Praxi ile gelen **yerleşik** beceriler (deponun `skills/` klasörü).
2. **Senin becerilerin**: `<data dir>/skills/`.
3. **Bota özel** beceriler: `<data dir>/bots/<bot-id>/skills/`. `skill_write`, botun öğrendiklerini
   burada, çağıran botun kendi klasöründe saklar. Ortak `<data dir>/skills/` klasörü ise
   yayımlanmış, paylaşılan katmandır.

Bugün yerleşik olan beceriler: `cookie-consent-banners`, `custom-dropdowns`,
`date-pickers`, `github-basics`, `linux-file-upload-dialog`,
`macos-file-dialogs`, `research` (alt soruları planlar, `web_research` çalıştırır, kaynaklı bir
rapor kaydeder; bkz. [Araştırma raporları](web.md#araştırma-raporları)),
`shadow-dom-and-iframes`, `sign-in-forms`, `web-tables-and-spreadsheets`, `windows-file-upload-dialog`,
`windows-save-as-dialog`, `windows-settings-navigation`.

**Başlangıç paketi** gündelik işleri kapsar: `document-to-action-items`,
`meeting-action-items`, `weekly-review-planning`, `email-inbox-triage`,
`product-price-monitor`, `competitor-news-monitor`, `grounded-citations`,
`blocked-page-recovery` ve `maps`. Bunlar Hermes Agent'tan uyarlandı (MIT, bkz.
`THIRD_PARTY_NOTICES.md`). Senin onayın olmadan hiçbir şeyi göndermez, silmez ya da
zamanlamaz. Web sayfalarını, postaları ve belgeleri yönerge olarak değil, veri olarak ele alır.

Beş yerleşik **rehber**, Praxi'nin mevcut araçlarını ve onay kapılarını kullanan çok adımlı iş
akışlarını anlatır: `signup-to-site` (botun kendi posta kimliği, daemonun ürettiği parola,
doğrulama postasını `email_wait_for` ile beklemek, uzun kayıt görevleri için `site_ledger`
site listesi, CAPTCHA ya da 2FA için işi sahibine devretmek), `lead-research` (doğrulanmış adayları bir çalışma alanındaki `leads/<campaign>.csv`
dosyasına kaydetmek), `outreach-drafts` (her aday için yalnızca taslak e-postalar, sahibi toplu
olarak inceler, asla göndermez), `office-doc-prep` (desteklenmeyen dönüşümlerin yapılabildiğini
söylemeden `docx`, `xlsx` ve `csv` dosyaları oluşturmak ve incelemek) ve `install-capability`
(ana makineye paket kurulumunu, ana makinenin sınırlarını aşmak yerine sahibine bırakmak).

## Kendin yaz

`<data dir>/skills/<name>/SKILL.md` dosyasını oluştur:

```markdown
---
name: custom-dropdowns
description: Operate non-native dropdowns, comboboxes and autocomplete lists built from divs.
version: 0.1.0
when:
  keywords: [dropdown, select, combobox, autocomplete]
---
# Custom dropdowns

1. Click the control with `browser_click` to open it.
2. Run `browser_snapshot` again; options often appear at the end of the page.
3. Click the option by its ref and verify the control shows the chosen text.
```

Kurallar:

- `name`: `a-z`, `0-9`, `-`, `_`, `.` karakterlerinden 1 ile 64 arası karakter. Harf ya da
  rakamla başlamalı, `..` içermemeli. Aynı zamanda klasör adıdır ve üzerine yazma anahtarıdır.
- `description`: tek satır, en çok 200 karakter. Bot, beceriyi okuyup okumayacağına buna bakarak
  karar verir, bu yüzden açık ve özel yaz.
- `version`: harf, rakam, `.`, `-`, `_` ve `+` karakterlerinden 1 ile 64 arası karakter. Harf ya
  da rakamla başlamalı, `..` içermemeli ve `.` ile bitmemeli.
- `author` ve `license` (isteğe bağlı): tek satırlık teşekkür satırları, örneğin bir beceri açık
  kaynaklı bir çalışmadan uyarlandıysa. Modele gösterilmezler.
- `when` (isteğe bağlı), becerinin kendiliğinden öne çıkmasını sağlar. Üç anahtarın üçü de
  listedir: `domains` (`github.com` gibi, alt alan adları da eşleşir), `apps` (`SystemSettings`
  gibi uygulama adları, büyük küçük harf fark etmez, `.exe` yok sayılır) ve `keywords`
  (görev metniyle eşleştirilir).
- Dosyanın tamamı en çok 64 KB olabilir. Ek dosyalar, `SKILL.md` dosyasının yanındaki klasöre
  konabilir.
- Modele gösterilen dizinin bir boyut bütçesi vardır. Bu yüzden birkaç odaklı beceri, yüzlerce
  belirsiz beceriden iyidir.

Geçersiz beceri dosyaları, bir not düşülerek atlanır. Bu bir kesinti yaratmaz.

Beceriler yönlendiriciyi de besler: karar merdiveninin (K2) anahtar sözcük ve embedding araması,
bir istek için bir beceri önerebilir (bkz. [İş akışları](workflows.md)).

## Beceri yükle

Başkalarının yazdığı beceriler güvenilmeyen içeriktir. `bot skills install` bir becerideki hiçbir
şeyi asla çalıştırmaz. İçindekileri sen gördükten sonra dosyaları `<data dir>/skills/` içine
kopyalar.

```text
bot skills install https://github.com/someone/praxi-skills.git --subdir github-triage
```

1. Beceri geçici bir klasöre çekilir, doğrulanır ve taranır. Ad, sürüm, kaynak, **sabitlenmiş
   commit**, dosya sayısı ve statik taramanın tüm bulgularını görürsün (betikler, ağ kullanımı,
   kimlik bilgisi kalıpları, istem enjeksiyonu cümleleri, gizlenmiş içerik). Klasörde betik ya da
   ikili dosya varsa bir uyarı da çıkar.
2. Terminalde onaylarsın ya da `--accept <fingerprint>` verirsin (parmak izi raporda yazılıdır).
   Parmak izi her dosya yolunu ve her baytı kapsar. Sen baktıktan sonra değişen içerik reddedilir.
3. Dosyalar veri klasörüne kopyalanır ve `<data dir>/skills.lock.json` dosyasına kaydedilir
   (kaynak, commit, içerik özeti, zaman).

Her zaman geçerli kurallar: git ve dizin kaynakları için parmak izi her zaman gerekir, CLI yerel
klasörlerde de bunu ister. Sembolik bağ, özel dosya, klasör dışına çıkan yol ya da çok sayıda
veya çok büyük dosya içeren arşivler reddedilir. Dizin kayıtları tam bir commit özetini
sabitlemek zorundadır. Tarama yalnızca bir öneridir: **hiçbir bulgu çıkmaması asla güvenli
olduğu anlamına gelmez**, dosyaları oku.

```text
bot skills install https://example.com/index.json --name github-triage   # from an index
bot skills install ./my-skill                                            # local folder
bot skills list                  # source, commit, risk, usage, MODIFIED/MISSING flags
bot skills update github-triage  # shows new findings and risk change, asks first
bot skills update                # check every hub-installed skill
bot skills uninstall github-triage
bot skills sync                  # restore skills whose folder is missing (verifies hashes)
```

Bir güncelleme, kayıtlı kaynağı yeniden çeker ve tarama sonucunun yüklü kopyayla farkını yazdırır
(yeni ve kalkan bulgular, riskin önceki ve sonraki hâli, eski ve yeni commit). Değiştirmek için
yeni parmak izi gerekir. Yani eski sürümü onaylamak yeni sürümü onaylamış olmaz. Kaldırma işlemi
klasörü, kilit kaydını ve kullanım sayaçlarını siler. `sync`, içeriği kilitlenmiş özetle artık
uyuşmayan bir beceriyi geri yüklemeyi reddeder.

Çalışan bir daemon, yüklenen, güncellenen ve kaldırılan becerileri bir sonraki yenilemede
görür. Bir değişiklik görünmüyorsa daemonu yeniden başlat. Yüklenen beceriler
ortak katmana girer, bu yüzden bir bot onları diğer beceriler gibi okuyabilir.

## Uygulamadaki beceri kitaplığı

Bir botun **Beceriler** penceresinde **Beceri kitaplığı** düğmesi vardır. İki sekmesi var.

- **Yüklü**, bir beceri kaynağından eklenen becerileri listeler: ad, açıklama, sürüm, nereden
  geldiği, güvenlik denetiminin ona verdiği risk ve neler istediği, sade sözlerle. Her birinde
  **Güncellemeyi denetle** ve **Kaldır** düğmeleri bulunur. Botların kullandığı bir beceriyi
  kaldırırken Praxi o botları gösterir ve önce yeniden sorar.
- **Göz at**, Praxi ile gelen beceri listesini (`skills/index.json`) okur. **İncele ve yükle**,
  beceriyi çeker, komut satırıyla aynı güvenlik denetimini yapar ve bir şey yüklenmeden önce
  neler istediğini gösterir. Yükleme her zaman, okuduktan sonra senin bastığın bir düğmedir. İncelediğin
  parmak izine bağlıdır, arada değişen içerik reddedilir.

Denetimin Düşük riskin üstünde bulduğu bir beceri, ancak bulguları okuduğunu işaretlersen yüklenir.
Yüklü sürümden daha fazlasını isteyen bir güncelleme (yeni bir istek türü ya da daha yüksek risk)
neyin yeni olduğunu gösterir ve ikinci bir onay kutusu ister. Adı, yerleşik ya da senin kendi
becerinle çakışan bir beceri yüklemeye sunulmaz. **Gelişmiş** altında başka bir beceri listesi
kullanabilirsin. Bu liste, `https://` ile başlayan ve bir web sitesi adı taşıyan bir adres olmalıdır
(IP adresi, `localhost` ya da kullanıcı girişi içeren adres olmaz). Uygulama listeyi yalnızca okur. İçindeki
her beceri için aynı inceleme geçerlidir.

## Ortak becerileri API ile yönetmek

Yerel sahip (eşleştirilmiş bir cihaz değil), daemonun API'si üzerinden ortak becerileri
oluşturabilir, içe aktarabilir, güncelleyebilir ve silebilir. Bkz.
[Ortak becerileri yerel uygulamadan yönetmek](skill-management-api.md).

## Bir bota hangi becerilerin verileceğini seçmek

Bir botun profilinde **Beceriler** altındaki **Becerileri yönet**, bota hangi ortak ve yerleşik
becerilerin sunulacağını belirler. Varsayılan hepsidir (**Tüm ortak beceriler**), **Yalnızca
seçtiklerim**'i seçince seni bir liste bekler. Botun kendi öğrendiği beceriler her zaman etkin
kalır. Botun yayımladığı bir beceri, diğer botlara kendiliğinden atanmaz.

Atanmamış bir beceri, botun isteminde, beceri ipucunda, yönlendiricinin aday listesinde ve
`skill_list` içinde görünmez, `skill_read` de onu reddeder. Atama okunamazsa (bozuk bir satır,
kullanılamayan bir bot klasörü), bot hepsini değil, hiçbir ortak beceriyi almaz.

Bu bir istem ve araç sınırıdır, **gizlilik değildir**. Ortak beceriler `<data folder>/skills/`
içinde düz dosyalardır. O klasöre dosya erişimi ya da kabuk erişimi verilmiş bir bot, atanmamış
bir beceriyi yine diskten okuyabilir. Bir beceri dosyasında gizli bilgi tutma.
