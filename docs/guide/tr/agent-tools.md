# Notlar

Her botun, çalışmalar arasında kullandığı küçük bir yerleşik not defteri vardır: uzun bir görevin planı, bir kontrol listesi, yarın lazım olacak bilgiler. Araçlar: `notes_read` (notları listeler, birini okur ya da arar) ve `notes_write` (bir notu değiştirir, ekleme yapar ya da siler).

* Notlar tek bir bota aittir, diğer botlar onları hiç görmez. Bilgisayarında kalırlar.
* Notlarda parola ya da anahtar saklanmaz: kimlik bilgisine benzeyen metin reddedilir.
* Sınırlar: not başına 32 KB, en fazla 100 not, bot başına 512 KB.
* Güvenlik: bot bir web sayfasını, bir e-postayı ya da başka dış içeriği okuduktan sonra not kaydetmek veya silmek önce senin onayını ister, çünkü bir not botun sonra ne okuyacağını şekillendirir. Böyle bir çalışmada kaydedilen not güvenilmez olarak hatırlanır ve bota talimat olarak değil, veri olarak gösterilir.

Notlar hafızadan farklıdır: hafıza, botun sana ait olduğunu söylediğin bilgileri tutar. Notlar ise botun kendi çalışma alanıdır.

## Hafıza araçları

Hafızası açık olan bir botun dört aracı vardır: `memory_search` (hatırladıklarına bakar), `session_search` (daha önceki bir sohbette söyleneni bulur), `memory_save` ve `memory_forget`. Hafızası **Kapalı** olan bota bu araçlar verilmez. Bkz. [Hafıza](memory.md).

* `memory_save` yalnızca bu turda söylediğin şeyi saklar (mesajından alıntı yapmak zorundadır), gizli bilgileri, soruları ve alt ajan çalışmalarını reddeder, çalışma dış içerik okuduysa onay ister. Botun hafıza ayarını izler: **Kaydetmeden önce sor** seçiliyken not senin onayını bekler, **Öğrendiklerimi diğer botlarımla paylaş** anahtarı ise senin hakkındaki bilgilerin diğer botlarınla paylaşılıp paylaşılmayacağını ya da bu botta kalıp kalmayacağını belirler.
* Arka planda öğrenme bu araçlardan ayrıdır: bir turdan sonra bot kendiliğinden de not tutabilir. Buna araç kullanan, alt çalışma başlatan ya da dış içerik (web sayfaları, dosyalar, e-posta, diğer botlar) okuyan turlar da dahildir. Böyle turlardan çıkan notlar **güvenilmez** diye etiketlenir, **Onayın gerekiyor** bölümünde bekler ve sen onaylayana kadar kullanılmaz. Modele yalnızca alıntılanmış veri olarak gösterilirler, hiçbir zaman talimat olarak değil.

## Saat ve hesap makinesi

Botun iki küçük yardımcı aracı her zaman vardır, ama yalnızca gerektiğinde yüklenirler (bot onları araçları arayarak bulur):

* `time_now`, herhangi bir saat diliminde güncel tarih ve saati verir ve tarih hesabı yapar: gün, ay ya da saat ekler veya çıkarır, bir tarihe ne kadar kaldığını sayar ya da aynı anı başka dilimlerde gösterir. Böylece bot bugünün tarihini tahmin etmek zorunda kalmaz.
* `calculate` kesin aritmetik yapar (`+ - * / % ^`, parantezler, `round` ve `sqrt` gibi birkaç işlev), böylece toplamlar ve yüzdeler “kafadan” hesaplanmaz.

İkisi de yalnızca botun verdiği metin üzerinde hesap yapar ve saati okur. Dosya açamaz, ağı kullanamaz, program çalıştıramaz. Bu yüzden hiçbir zaman onay istemez ve dışarıya bir şey gönderme sayılmaz. İfadelerin uzunluğu ve derinliği sınırlıdır, düz aritmetik olmayan her şey reddedilir.

Bir CSV, JSON ya da PDF dosyasını okumak (`csv_read`, `json_query`, `pdf_extract`) yalnızca izin verdiğin klasörlerde çalışır ve özel veri okuma sayılır: bot az önce bir web sayfası ya da e-posta okuduysa, veriyi dışarı gönderecek her şey önce sana sorar.

## Belgeler ve kod depoları

Botlar yalnızca metin değil, Word, Excel ve PowerPoint dosyalarını da okuyup yazabilir. `docx_read`, `xlsx_read`, `pptx_read`, `pdf_extract`, `csv_read` ve `json_query` okur; `docx_write`, `xlsx_write` ve `csv_transform` yeni dosya oluşturur. Üç araç ise var olan bir dosyanın geri kalanına dokunmadan onu değiştirir:

* `xlsx_append_rows` bir sayfanın son satırından sonra satır ekler.
* `xlsx_update_cells` tek tek hücreleri adresiyle (`B2`) ayarlar. Hücre kendi biçimini korur.
* `docx_append` bir Word belgesinin sonuna başlık, paragraf, liste ve tablo ekler.

Diğer sayfalar, stiller, sütun genişlikleri ve görseller olduğu gibi kalır. Sınırlar: araçlar Excel tablolarını genişletmez, formülleri yeniden hesaplamaz (dosyayı açınca Excel yapar) ve makro dosyalarını (`.xlsm`, `.docm`) ile eski `.xls` / `.doc` dosyalarını reddeder. `fs_write` gibi, her değişiklikten önce bir geri alma noktası kaydedilir.

Bunların hepsi `fs_read` ve `fs_write` ile aynı dosya erişimi ayarına uyar: “Yalnızca klasör” erişimi olan bir bot klasörlerinin dışındaki bir belgeyi açamaz ya da değiştiremez; kimlik bilgilerine ve Praxi’nin kendi verisine de bu yoldan erişilemez. `doc_convert` ve `doc_preview` (Office’ten PDF’ye, sayfa görüntüleri) ayrıca bilgisayarda LibreOffice ve ağdan yalıtılmış bir korumalı ortam ister, yalnızca botun çalışma alanına yazar; bunlar yoksa söyler ve hiçbir şey yapmaz.

Geliştirici araçları da vardır: `git_status`, `git_diff`, `git_log`, `git_show`, `git_branch` ve `git_commit` `git`’i doğrudan çalıştırır (kabuk üzerinden değil, kancalar kapalı), `process_list` çalışan programları gösterir. “Yalnızca klasör” erişiminde depo botun klasörlerinin içinde olmalıdır; “Bu bilgisayar” erişiminde, kimlik bilgisi ya da ayar klasörlerinin içinde olmayan her depo olabilir. Bir depoyu okumak, dosya okumak gibi özel veri okuma sayılır.

OCR model dosyaları bilgisayarda zaten kuruluysa (`PRAXI_OCR_MODEL_DIR` ya da Praxi’nin varsayılan modeller klasörü), bir uygulamanın erişilebilirlik ağacı zayıf olduğunda masaüstü araçları ekrandaki yazıyı da okur ve bulduklarını numaralandırır (`mark:3`); böylece bot ona tıklayabilir. Model dosyaları yoksa hiçbir şey değişmez: araçlar OCR motoru ayarlanmadığını söyler.
