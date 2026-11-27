# Bu bilgisayarda mı, sunucuda mı çalışsın

**Kısa yanıt: bu bilgisayarda çalıştır. Sunucuya ihtiyacın yok.**

## Bu bilgisayarda (varsayılan)

Botların, Praxi'yi kurduğun bilgisayarda çalışır. Praxi açıkken ve bilgisayarın uyanıkken çalışırlar.

- Kolay: ayrıca kurulacak bir şey yok.
- Özel: sohbetlerin ve dosyaların burada kalır.
- Sınır: bilgisayarın kapalıyken ya da uykudayken botların çalışmaz. Zamanlanmış işler seni bekler (bkz. [İşleri otomatikleştir](automate.md)).

Uygulamada bu bilgisayarın adı **Bu bilgisayar**. Üstteki bilgisayar menüsünde görürsün.

## Sunucuda (isteğe bağlı)

**Sunucu**, gece gündüz açık duran başka bir bilgisayardır. Çoğu kişi internetten kiralar. Bazıları evdeki eski bir bilgisayarı kullanır.

Sunucuyu yalnızca şu durumlarda seç:

- kendi bilgisayarın kapalıyken işlerin gece çalışmasını istiyorsan, ya da
- botların telefonundan her saatte yanıt versin istiyorsan.

Önce bilmen gerekenler:

- Sunucu para tutabilir. Bazı sağlayıcıların ücretsiz planı var.
- Birkaç basit bilgi gerekir: sunucunun adresi, bir kullanıcı adı ve bir anahtar dosyası.
- Sunucudaki bot, senin dosyalarında değil, sunucunun dosyalarında çalışır.
- O botun anahtarları ve sohbetleri sunucuda durur. Onu güvende tutman gereken ikinci bir bilgisayar gibi düşün.

## Sunucu nasıl eklenir

1. Üstteki bilgisayar menüsünü aç.
2. **Sunucu ekle…**'yi seç.
3. Üç kısa adımı izle. Praxi sunucuyu kontrol eder ve neyi düzeltmen gerektiğini söyler.

Ücretsiz bir seçenek dahil tam anlatım [Sunucu ekleme](servers.md) sayfasında. Linux sunucu komutları [Her zaman açık Linux sunucusu](linux-host.md) sayfasında.

![Bu bilgisayar ve Sunucu ekle seçenekleriyle bilgisayar menüsü](../images/tr/computer-menu.webp)

## Geri dön ya da sunucuyu kaldır

- Yeniden yerelde çalışmak için bilgisayar menüsünde **Bu bilgisayar**'ı seç.
- Bilgisayar menüsünde **Sunucuların**'ı aç ve bir sunucuyu unutmak için **Kaldır**'a bas. Sunucunun kendisi ve botları çalışmaya devam eder. Yalnızca bu bilgisayar onu unutur.

## Sunucu yanıt vermiyorsa

"{name} yanıt vermedi" iletisi, sunucunun kapalı olduğu ya da ağı olmadığı anlamına gelir. Aç ve yeniden dene. Daha fazla adım: [Sık görülen sorunları çöz](fix-common-problems.md).
