# Web tarama eklentisi (isteğe bağlı)

Yerleşik [web araçları](web.md) çoğu sayfayı zaten okur. **Web tarama eklentisi**,
bir sunucuya ekleyebileceğiniz küçük bir ek programdır: daha temiz makale metni,
bir sayfadaki bağlantı listesi ya da kısa bir tarama (aynı sitenin en fazla 10
sayfası) için. Tek bir araç ekler: `web_scrape`. Botlara önce `web_extract`
denemeleri, `web_scrape` aracını yalnızca o yetmediğinde kullanmaları söylenir.

Giriş yapmanız gereken sayfalar için **değildir** ve JavaScript çalıştırmaz.
Asla giriş yapmaz, CAPTCHA çözmez, kim olduğunu gizlemez ve ücret duvarını aşmaz.
Bir site hayır derse bot durur ve bunu söyler.

## Ekleme ve kaldırma

Bunu yalnızca siz, kendi bilgisayarınızdan, SSH girişinizle yapabilirsiniz.
Sunucudaki Praxi kullanıcısının yönetici hakkı yoktur; bir bot bunu asla kuramaz.

1. Üstteki **bilgisayar seçicisini** açın → **Sunucularınız**, sunucuyu seçin ve **Araç paketleri**ye basın.
2. **Web tarama** kutusunu işaretleyin. Python gerekir (**Geliştirici araçları** ile gelir).
3. **İncele**, hiçbir şey olmadan önce tam olarak neyin değişeceğini gösterir.
   Onaylarsanız sunucu yaklaşık 40 MB indirir.
4. Kaldırmak için aynı satırdaki **Kaldır** düğmesini kullanın. Eklentiyi, servisini
   ve anahtarını siler. Python ve diğer araçlarınız kalır.

Yeniden kurmak güvenlidir ve bozuk bir kurulumu onarır. Araç, o sunucudaki Praxi
bir sonraki başladığında (yeniden başlatma ya da güncelleme sonrası) botlara görünür.

**Bot başına ayrı bir anahtar yoktur.** Eklenti bir sunucuya kurulunca o sunucudaki
tüm botlar, diğer web araçları gibi `web_scrape` aracını alır. Olağan onaylar
geçerli kalır: bir sayfayı okuyan bot, size sormadan özel veri gönderemez. Tüm
botlar için kapatmak üzere **Kaldır** düğmesini kullanın.

## Eklenti ne yapar, ne yapmaz

- Yalnızca `127.0.0.1` üzerinde dinler (port 47811); bilgisayardaki diğer
  programların `/etc/praxi/scrape.token` anahtarına ihtiyacı vardır.
- Yalnızca herkese açık sayfaları okur. Özel, yerel ve bulut meta veri adreslerini
  reddeder, her yönlendirmeyi yeniden denetler; en çok 5 yönlendirme, 5 MB, sayfa başına 20 saniye.
  Süre sınırı katıdır: verisini bayt bayt damlatan bir site sınırda kesilir ve eklentiyi
  sonraki istek için asla tıkayamaz. Yalnızca HTML, XML ve metin; çerez yok.
- `robots.txt` dosyasına (`Crawl-delay` dahil) uyar ve kendini dürüstçe tanıtır:
  `PraxiBot/1 (+https://github.com/emreocell/Praxi-Bot)`.
- Naziktir: bir siteye saniyede en fazla 1, dakikada en fazla 30 istek (`robots.txt`
  isteği de sayılır).
- Sunucunun DNS adresleri, **kurulum sırasında** `/etc/resolv.conf` dosyasından küçük
  bir ek dosyaya yazılır. DNS ayarları sonradan değişirse (ör. yeni DHCP kirası)
  **Araçlar**'dan eklentiyi yeniden kurun.
- Getirdiği metin, `web_extract` gibi **güvenilmeyen sayfa içeriğidir**: botun
  okuyacağı veridir, asla talimat değildir.

## Lisanslar

Praxi'nin kendi `praxi_scrape.py` dosyası: Apache-2.0. [Trafilatura](https://github.com/adbar/trafilatura)
2.4.0: Apache-2.0. Bağımlılıklar: Apache-2.0, BSD, MIT (`certifi`: MPL-2.0,
değiştirilmemiş). Araç paketleri penceresi her paket için lisansı ve kaynak
bağlantısını gösterir; ayrıntılar `THIRD_PARTY_NOTICES.md` içindedir. Ayrı copyleft
programlar (ör. AGPL-3.0) asla pakete eklenmez; ileride biri sunulursa yazarından
indirilir ve kurulumdan önce lisans bildirimini onaylamanız gerekir.

## Kişisel veriler

Sayfalar kişisel veri içerebilir. KVKK ve GDPR kapsamında neyi, neden ve ne kadar
süre topladığınızdan siz sorumlusunuz. Yalnızca ihtiyacınız olanı okuyun ve bir
sitenin kullanım koşullarına bakın. Bu genel bilgidir, hukuki tavsiye değildir.
