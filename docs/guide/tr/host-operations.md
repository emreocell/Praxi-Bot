# Praxi sunucusunu işletme

Her an açık duran Linux sunucunun (systemd, yetkisiz `praxi` kullanıcısı) günlük işletimi.
Sunucuyu kurmak [Praxi'yi uzak sunucuda çalıştırma](remote-host.md) sayfasının konusudur.
Bu sayfa, kurulumdan sonra ne yapacağını anlatır.

Sunucuda `sudo praxi ...`, servis ortamıyla birlikte `praxi` kullanıcısı olarak çalışan `bot`
CLI'ıdır. Sunucunun sahibi olan bilgisayardan `bot host add`, `repair`, `upgrade` ve `remove`
komutları o terminalde SSH üzerinden çalışır. Bunlar daemon rotası değildir. SSH bir anahtar
dosyası ya da `ssh-agent` kullanır. Parolalar desteklenmez. `<TARGET>`, `user@server` ya da
kayıtlı addır.

Aşağıda anlatılanlar `apps/cli` ve `crates/host-provision` kodunun yaptığı şeylerdir. Bu sayfadaki
hiçbir şey canlı bir sunucuda çalıştırılmadı (**doğrulanmadı**).

## Günlükler

Sunucu daemon'unun kendi günlük dosyası yoktur. `praxi.service`, `StandardOutput` ayarlamaz;
bu yüzden stdout ve stderr journal kaydına gider. Açılış her adım için bir stderr satırı yazar:
`startup: <step> (+N ms)`. Journal `startup: listening` satırını gösterince API yanıt verir.
Liderlik denetimi bundan sonra çalışır ve bitince `startup: leader components started in background`
satırını yazar. Zamanlanmış işler ve kanallar ancak o zaman başlar. Bak: [Sunucu liderliği](host-leadership.md).

```sh
journalctl -u praxi -n 200 --no-pager
journalctl -u praxi -u praxi-xvfb -u praxi-desktop -u praxi-node -f
sudo systemctl status praxi.service praxi-xvfb.service praxi-desktop.service
```

`bot host add`, `repair` ve `upgrade` bir adım tablosu ile `Log (redacted):` satırını ve bir yol
yazdırır. Bu dosya sunucuda değil, komutu çalıştırdığın bilgisayarda
`<data directory>/hosts/<saved-name>/provision.log` konumundadır. Günlük; eşleştirme biletlerini,
özel anahtar bloklarını ve parola, token ya da gizli bilgi taşıyan satırları ayıklar.

`desktop-daemon.log` dosyasını masaüstü uygulaması, o bilgisayarda başlattığı daemon için yazar.
Sunucunun günlüğü değildir.

Loopback üzerinden sağlık denetimi (varsayılan port 47821 ya da `/etc/praxi/praxi.env` içindeki `BOT_PORT`):

```sh
curl -fsS --noproxy '*' --max-time 2 http://127.0.0.1:47821/v1/health
```

Yeni kurulmuş bir sunucu, bir sağlayıcı ayarlanana kadar `"status":"setup_required"`, sonra
`"status":"ok"` yanıtı verir. `praxi.env` dosyasını ekrana yazdırma. Gizli bilgi deposunun parolasını
taşır.

## Başlatma, durdurma ve yeniden başlatma

Birimler:

| Birim | Görevi | Kurulum programı etkinleştirir mi |
| --- | --- | --- |
| `praxi.service` | `bot serve` ya da `bot serve --host`, yalnızca loopback. `Restart=on-failure`, `RestartSec=5` | evet |
| `praxi-xvfb.service` | sanal ekran | masaüstüyle birlikte |
| `praxi-desktop.service` | o ekranda Xfce oturumu | masaüstü oturumuyla birlikte |
| `praxi-node.service` | `bot-node run`. `RestartSec=10` | kurulur, etkinleştirilmez |

Sunucu modu, birleştirilmiş birimin son `ExecStart=` satırıdır. `bot host repair` ve
`bot host upgrade` kurulumu `--host-mode` ile yapar, yani o satır `bot serve --host` olur.
`systemctl cat praxi.service` ile denetle.

systemd 249 ve daha yenisinde `praxi-xvfb.service`, `praxi.service` ile
`praxi-desktop.service` birimlerini ayakta tutar. Xvfb hâlâ etkinken `praxi` birimini durdurursan
`praxi` yeniden başlar. Takımı durdurmak için Xvfb'yi durdur (diğerlerini o bağlar, hepsi onunla
birlikte durur):

```sh
sudo systemctl stop praxi-xvfb.service
sudo systemctl start praxi-xvfb.service
sudo systemctl start praxi-desktop.service
sudo systemctl start praxi.service
systemctl is-active praxi.service praxi-xvfb.service praxi-desktop.service
```

Yalnızca daemon'u yeniden başlatmak için `sudo systemctl restart praxi.service` kullan.
Sanal ekran yoksa `praxi.service` birimini doğrudan başlat ve durdur.

Kurulum programı, kendi paket kurulumları için `NEEDRESTART_MODE=l` ayarlar; böylece Ubuntu'nun
needrestart kancası bu kurulumlar sırasında Praxi'yi yeniden başlatmaz. Kendi `apt upgrade`
komutunda da bu kancaların birimleri yalnızca listelemesini, dokunmamasını istiyorsan
`NEEDRESTART_MODE=l` ayarla. systemd 249'dan eski sistemlerde kurulum programı `Upholds=` satırını
koymaz ve bir açılış yardımcısı kullanır. Ayrıntılar
[uzak sunucu sorun giderme](remote-host.md) bölümünde.

## Yükseltme ve onarım

Her iki komut da sunucuya SSH ile bağlanır, kurulum betiklerini yükler ve başarılı olunca `Done`
yazar. Başarısız bir adım çalışmayı durdurur. Nedenini düzelt ve aynı komutu yeniden çalıştır.
Zaten doğru olan adımlar `skip` bildirir.

Sürüm karşılaştırması yalnızca major.minor.patch üçlüsüne bakar. Ön sürüm eki sayılmaz.
Sunucunun üçlüsü bu uygulamanınkinden yeniyse her kip `not downgrading` uyarısı verir ve
sunucuyu olduğu gibi bırakır.

| Sunucu durumu | `bot host add` | `bot host upgrade` | `bot host repair` |
| --- | --- | --- | --- |
| Hiçbir şey kurulu değil | kurar ve eşleştirir | başarısız olur: `add` kullan | kurar ve eşleştirir |
| Düz `bot serve` (`--host` değil) | başarısız olur: `repair` kullan | yerinde yeniden kurar | yerinde yeniden kurar |
| Sunucu modu, aynı sürüm, servis etkin | kurulumu atlar, yine eşleştirir ve sağlığı denetler | kurulumu atlar, yine eşleştirir ve sağlığı denetler | yerinde yeniden kurar |
| Sunucu modu, eski sürüm ya da sunucu modu ama etkin değil | bu uygulamanın sürümünü kurar | bu uygulamanın sürümünü kurar | bu uygulamanın sürümünü kurar |

“Bu uygulamanın sürümü”, ikili dosyaya gömülü sürüm etiketidir (`PRAXI_RELEASE_TAG`), o yoksa
`v` artı ikili dosyanın sürümüdür. Bak: [sunucunun hangi etiketi kurduğu](release-linux.md).

Bir kurulum gerçekten çalışırken ve `/var/lib/praxi` zaten varsa, yardımcı önce yalnızca root kullanıcısının
okuyabildiği bir arşiv yazar (aşağıdaki Yedekleme ve geri yükleme bölümüne bak).
Sonra `install-linux.sh --version <tag> --host-mode` komutunu çalıştırır. `--no-desktop`
vermediysen `--with-desktop-session` de verir. `--no-desktop` bu bayrağı koymaz. `--without-desktop`
vermez, bu yüzden zaten var olan bir ekran kalır. Kurulum programı `/var/lib/praxi`,
`/etc/praxi/praxi.env` ve `/etc/praxi/praxi-node.env` dosyalarını korur.

Dosyaları değiştirmeden önce kurulum programı etkin birimleri durdurur (önce Xvfb'yi, ki bir
ayakta tutma işlemi kurulumun ortasında daemon'u başlatmasın) ve önceki `/opt/praxi`, sarmalayıcı
komutlar, env dosyaları ve birimleri bir kenara kopyalar. Kurulum sonra başarısız olursa o kopya
geri konur ve önceden etkin olan birimler yeniden başlatılır. Betik
`rolling back to previous working installation` der. Bu geri alma `/var/lib/praxi` dizinini
geri yüklemez. Veritabanını açmak onu ileri doğru taşır ve bu açma sağlık denetiminden önce
olur, yani başarısız bir başlatma verileri çoktan taşımış olabilir. Önceki baytlara ihtiyacın
varsa veriyi anlık yedekten geri koy. Canlı bir başarısız yükseltme **doğrulanmadı**.

Kurulumdan sonra komut, bu bilgisayar henüz eşleşmediyse onu eşleştirir, o cihaza ekran görüntüleme
ve girdi izni verir ve eş bağlantısı üzerinden sağlığı denetler (beş saniye arayla altı deneme).
Çalışan sunucu sürümünün major.minor.patch değeri bu uygulamadan farklıysa sağlık denetimi başarısız
olur ve mesaj `bot host upgrade` komutunu yeniden çalıştırmanı söyler. SSH sunucu anahtarı
değişmişse komut herhangi bir değişiklikten önce durur (`host key mismatch`). Yeni bir anahtar,
`yes` yazmanı ister ya da `--yes` verirsin.

`bot host remove <name>` sunucuyu yerelde unutturur ve bu bilgisayarın oradaki yetkisini iptal eder.
`--uninstall` ayrıca kaldırır ve veriyi korur. `--purge` ise `/var/lib/praxi`, `/etc/praxi` ve
`praxi` kullanıcısını siler. `--yes`, `--purge` için yazdırılan onayı atlar.

Yönetilen güncelleyiciyle kurulmuş bir Linux sunucusunda **Sunucuyu güncelle**
düğmesi, masaüstü uygulamasının yayınlanan sürümünü eşleştirilmiş bağlantı üzerinden
ister. Bu işlem için masaüstünün sunucu yönetici yetkisi gerekir ve isteği root
güncelleyiciye yalnızca arka plan servisinin kendi süreci iletebilir. Güncelleyici,
sunucuda kurulu olandan eski bir sürümü asla kurmaz ve sunucunun kanalına uyar (aşağıya bak):
`stable` seçili bir sunucu ön sürüme geçmez. SHA‑256 özetini ve GitHub derleme kaynağını
(yalnızca sürüm etiketi için, hiçbir dal için değil) doğrular, servisi durdurur, SQLite
veritabanlarını `/var/lib/praxi-updates/db-snapshot` klasörüne kopyalar (yalnızca root
okuyabilir; sonraki güncelleme bunu değiştirir), program dosyalarını değiştirir,
`praxi.service` birimini başlatır ve yeni sürecin hazır olduğunu bildirmesini en fazla altı
dakika bekler. Hazır olmazsa güncelleyici onu durdurur, veritabanlarını ve önceki program
dosyalarını geri koyar ve önceki servisin açıldığını kontrol eder. Yeni sürümün bu sürede
yazdığı veriler geri almayla birlikte kaybolur. Güncelleme kopya için boş disk alanı ister;
yeterli alan yoksa hiçbir şeyi değiştirmeden durur. Daha eski kurulumlarda doğrulanmış
kurulum programıyla bir kez yükseltme yap. Uygulama güncellemeyi başlatamazsa SSH ile
yükseltme yolunu veya uygulamanın gösterdiği komutları kullan.

Zamanlanmış güncellemeler varsayılan olarak kapalıdır. Ayarı değiştirmek için
`/etc/praxi/updates.toml` dosyasını root olarak oluştur ve modu ve kanalı seç.

```toml
mode = "notify"         # off, notify, auto
channel = "stable"      # stable, pre-release
```

systemd zamanlayıcısı her gün kontrol eder. `notify`, masaüstünde seçili sunucu
için yeni sürümü gösterir. `auto` güncellemeyi de kurar ve botlar çalışırken servisi
yeniden başlatabilir. `stable` kararlı sürümü bekler. `pre-release` ön sürümleri de
kontrol eder. Yazdığın `channel` değeri masaüstündeki güncelleme düğmesi için de geçerlidir.
Yazmazsan ön sürüm çalıştıran sunucu ön sürümleri, kararlı sürüm çalıştıran sunucu kararlı
sürümleri izler. Dosya sıradan bir dosya olmalı, sahibi root olmalı, bağlantı olmamalı ve
16 KiB'den küçük olmalı. Ayar değişikliğini hemen uygulamak için
`sudo systemctl start praxi-update-check.service` çalıştır. Hata kayıtları için
`sudo journalctl -u 'praxi-update*.service'` komutunu kullan. Masaüstünden gelen
güncelleme isteklerini kapatmak için sunucuda `praxi-update.socket` birimini durdur
ve devre dışı bırak.

Yönetilen geri alma, program dosyalarını ve güncellemeden hemen önce kopyalanan veritabanlarını
geri yükler. Yarıda kesilen bir güncelleme (süre sınırı, çökme) başarısız sayılır ve servis yeniden
başlatılır; böylece güncelleme düğmesi kapalı kalmaz. `praxi self-update --rollback` ve aşağıdaki
kurulum programı geri alması yalnızca program dosyalarını değiştirir. Önemli veriler bulunan bir
sunucuyu yükseltmeden önce aşağıdaki yedekleme adımlarını uygula. Böylece başarısız
bir veri geçişinden de dönebilirsin.

## Geri alma

Yükseltmenin ne kadar ilerlediğine uyan yolu kullan.

- **Servisler durdurulduktan sonra kurulum başarısız oldu.** Kurulum programı önceki ikili
  dosyaları, birimleri, sarmalayıcıları ve env dosyalarını kendisi geri koyar. Şununla doğrula:
  `systemctl is-active praxi.service` ve `journalctl -u praxi -n 50`.
- **Yükseltme başarılı oldu ama önceki sürümü istiyorsun.** O etiketi imzalı kurulum programıyla
  (`--version <old tag>`) yeniden kur, [Praxi'yi uzak sunucuda çalıştırma](remote-host.md)
  sayfasındaki güncellemeyle aynı şekilde. Sunucunun major.minor.patch değeri, komutu çalıştırdığın
  uygulamanınkinden yeniyse `bot host upgrade` bunu yapmaz.
- **Veri, yükseltme öncesi kopyaya dönmeli.** Anlık yedeği `/var/backups/praxi` içinden (aşağıda)
  geri yükle. Bunu çalıştırmak istediğin ikili dosyalarla yap, çünkü daha yeni bir daemon veri
  dizinini açarken SQLite'ı ileri doğru taşır.

`praxi self-update` ve `praxi self-update --rollback` farklı bir komuttur. Kurulum önekinin içindeki
bir sürüm dizinini değiştirirler ve systemd'yi yeniden başlatmazlar. `bot host upgrade` değildirler.

## Yedekleme ve geri yükleme

İki ayrı kopya vardır. Onları aynı dosya sanma.

**Veri dizini anlık yedeği.** `repair` ya da `upgrade` var olan bir `/var/lib/praxi` dizinini
değiştirmeden önce yardımcı, root olarak şunu çalıştırır:

```sh
install -d -m 700 /var/backups/praxi
tar -C /var/lib -czf /var/backups/praxi/praxi-data-YYYYMMDDTHHMMSSZ.tgz praxi
```

Zaman damgası UTC'dir. Arşiv, veri dizininin tamamıdır (veritabanı, şifreli gizli bilgi dosyası, API
token'ı, tarayıcı profilleri, sunucu ayarı). `/etc/praxi/praxi.env` dosyasını içermez. O parola
olmadan geri yüklenen gizli bilgi dosyası açılamaz. Env dosyasını arşivle birlikte sakla ve ikisini
de sunucunun herkese açık diskinden uzak tut. Kendi kopyan için elle çalıştıracağın `tar` da aynısıdır.
SQLite sessiz olsun diye önce servisleri durdur. Yardımcı kendi anlık yedeğinden önce onları
durdurmaz (o kopyanın çökmeye karşı tutarlı olup olmadığı **doğrulanmadı**).

Geri yükleme bu düzeni izler. Bir arşiv açılarak **doğrulanmadı**:

```sh
sudo systemctl stop praxi-xvfb.service || sudo systemctl stop praxi.service
sudo mv /var/lib/praxi /var/lib/praxi.aside
sudo tar -C /var/lib -xzf /var/backups/praxi/praxi-data-YYYYMMDDTHHMMSSZ.tgz
sudo systemctl start praxi-xvfb.service
sudo systemctl start praxi-desktop.service
sudo systemctl start praxi.service
```

Bu birimler kurulu değilse Xvfb ve masaüstü satırlarını atla. `aside` dizini geri alma yolundur.
Sahiplik, arşivin kaydettiği şeydir (yardımcının `tar` komutu root olarak çalışır).

**Taşınabilir durum dosyası.** `sudo praxi backup create <file> --passphrase-file <pass>` ve
`sudo praxi backup restore <file> --passphrase-file <pass>`; botları, sohbetleri, hafızayı,
becerileri, iş akışlarını, zamanlamaları ve sağlayıcı ayarlarını tek bir şifreli dosyaya kopyalar.
API anahtarları yalnızca `--include-secrets` ile dahil edilir. API token'ı, eşleştirilmiş cihaz
durumu ve tarayıcı profilleri hiçbir zaman dahil edilmez. `restore`, bir daemon veri dizinini
tutarken ya da servis portunda yanıt verirken çalışmayı reddeder, bu yüzden önce birimleri durdur.
`praxi` kullanıcısı dosyayı okuyabilmeli. Ayrıntılar: [Şifreli yedekleme ve geri yükleme](backup.md).
`--include-secrets` olmadan yapılan bir geri yüklemeden sonra anahtarları yeniden gir
(`sudo praxi setup` ya da `sudo praxi secrets set`).

## Masaüstü uygulamasını eşleştirme

**Sunucu sağlayıcısının konsolundan:** masaüstünde **Sunucu ekle** ekranını aç
ve **Sunucum var** seçeneğini seç. Kurulum komutunu sunucu konsoluna yapıştır.
Komut kurulum programını doğrular, servisi kurar ve QR ile bir `bot-pair:` satırı
gösterir. Satırın tamamını 30 dakika içinde uygulamaya yapıştır. Süre dolduysa
sunucuda `sudo praxi host invite --role desktop` ile yeni bir kod al.

**Praxi desktop** eşleştirmeyi yaptığın bilgisayarı temsil eder. Çevrimdışı olması
o bilgisayarı hedefleyen işleri etkiler. Sunucuyu hedefleyen işler, masaüstü
kapalıyken sunucuda çalışmaya devam edebilir.

Masaüstü uygulaması sunucuyla dışa doğru açılan bir eş kanalı üzerinden konuşur. Sunucu bunun için
fazladan bir dinleme portu açmaz. İki eşleştirme yolu var:

**Sahibi olan bilgisayardan** (olağan yol): masaüstü uygulamasını ya da `bot serve` komutunu açık
bırak, sonra `bot host add user@server` çalıştır. Komut `--host-mode` ile kurar, sunucuda tek
kullanımlık bir masaüstü daveti oluşturur, bu bilgisayarı yerel daemon üzerinden eşleştirir ve ekran
görüntüleme ile girdi izni verir. Davet bileti `provision.log` dosyasına yazılmaz.

**Zaten kurduğun bir sunucudan:** sunucuda `sudo praxi host invite --role desktop` çalıştır. Bu davet
iki dakika sürer, bir kez işe yarar ve (sarmalayıcı sunucunun sahip anahtarını gönderdiği için)
sunucu yönetimini de içerir. Uygulamada bilgisayar seçiciyi aç, **Sunucu ekle** seçeneğini seç ve
bileti yapıştır. Bilet o alanda kalır. Uygulama onu saklamaz.

Yönetici bir masaüstü, ek bir komut olmadan ekranı izleyebilir ve kullanabilir. Eşleştirilmiş diğer
hiçbir cihaz, sen sunucuda izin verene kadar bunu yapamaz. İzin yeniden başlatmalardan sonra da
kalır:

```sh
sudo praxi host devices
sudo praxi host screen --all-desktops
sudo praxi host screen <key> --view-only
sudo praxi host screen <key> --revoke
```

`--revoke`, yönetici masaüstünün varsayılan iznini de kaldırır. Eşleştirilmiş bir masaüstü botları,
sohbetleri, çalışmaları ve ekranı yönetebilir. Gizli bilgiler, yürütme politikası, MCP sunucuları,
beceriler, e-posta ve web arama kimlik bilgileri ve Telegram eşleştirmesi sunucuda kalır
(`sudo praxi setup` ve diğer yerel komutlar). Kurulum sırasındaki daha uzun notlar
[Sunucuyu masaüstü uygulamasından kullanma](remote-host.md) bölümünde.

Sunucunun kendi model anahtarı olana kadar sohbetlerin yanıtlanacağı bir şey yoktur. Ekran ve bot
listesi yine de çalışır.

Canlı ekran görüntüsünün doğrudan bir yol mu yoksa bir röle mi kullandığını (RTT ve kare hızı)
ölçmek için `bot --remote <ENDPOINT> proxy` ile bir tünel aç ve
[Canlı görüntü bağlantısını ölçme](../../design/live-view-link-measurement.md) sayfasını izle.

## Güvenlik duvarı ve portlar

Daemon'u loopback üzerinde bırak. Dışarıya açma.

| Port | Yön | Ne yapmalı |
| --- | --- | --- |
| 22/tcp (ya da kendi SSH portun) | gelen | Praxi için sunucunun ihtiyaç duyduğu tek gelen kural |
| 47821/tcp (ya da `BOT_PORT`) | herkese açık arayüzde yok | `bot serve` yalnızca `127.0.0.1` adresine bağlanır. `sudo ss -tlnp` o port için `127.0.0.1` göstermeli, asla `0.0.0.0` ya da `[::]` göstermemeli |
| sağlayıcı API'si, Telegram, eş röleleri | giden | Varsayılan çıkışı açık bırak. Eş bağlantısı dışarı doğru arar. Gelen bir eş portu gerekmez |

API'ye başka bir makineden bir SSH tüneliyle (`ssh -L 47821:127.0.0.1:47821 user@server`) ya da
eşleştirilmiş uygulamayla ulaş. Oracle güvenlik listesi notları
[Güvenlik duvarı notları](remote-host.md) bölümünde.

## Sorun giderme

Sunucudaki kodlama CLI’ları `praxi` servis hesabıyla kurulur ve denetlenir.
Sunucu ayarlarındaki giriş seçeneğini kullan. CLI kendi terminalinde kurulum
istiyorsa sunucuda bir terminal açıp aynı hesap ve yolları kullan.

```sh
sudo -Hu praxi env HOME=/var/lib/praxi NPM_CONFIG_PREFIX=/var/lib/praxi/.npm-global \
  PATH=/var/lib/praxi/.local/bin:/var/lib/praxi/.npm-global/bin:/usr/local/bin:/usr/bin:/bin \
  /bin/bash --noprofile --norc
```

| Ne görüyorsun | Ne yapmalı |
| --- | --- |
| Sunucu çevrimdışı (uygulama ya da `bot status` onu göremiyor) | Sunucuda `systemctl is-active praxi.service` çalıştır. Etkin değilse `journalctl -u praxi -n 200 --no-pager` çıktısını oku ve yukarıdaki Başlatma, durdurma ve yeniden başlatma bölümündeki gibi başlat. Sonra loopback `/v1/health` adresine `curl` at. Dışarıdan 47821 portuna değil, SSH'e bak. Eşleştirilmiş bir masaüstünün de kendi daemon'unun çalışıyor olması gerekir. |
| Yavaş açılış | `journalctl -u praxi` içindeki son `startup:` satırını izle. Süre, `startup: begin` satırından beri geçen milisaniyedir. API `startup: listening` satırında ayaktadır. Bileşenler `startup: leader components started in background` satırına kadar kapalı kalır. Eşleştirilmiş cihazı olan bir sunucu, o satırdan önce eş kalp atışları için on saniyeye kadar bekler. Bak: [Sunucu liderliği](host-leadership.md). |
| Eş bağlanamıyor | Sunucu biriminin `bot serve --host` olduğunu ve `praxi.service` biriminin etkin olduğunu doğrula. Masaüstü daemon'u çalışıyor olmalı. Yapıştırılan bilet bir kez işe yarar ve iki dakika sonra geçersiz olur. `sudo praxi host invite --role desktop` ile başka bir tane oluştur. Eş rölelerine giden trafiğe izin verilmeli. Yerel daemon sunucuya ulaşamadığında `bot host repair` `Paired channel` başarısız diye bildirir ve bir dakika sonra yeniden denemeni söyler. SSH sunucu anahtarı uyuşmazlığı `add` / `repair` / `upgrade` komutlarını sunucuyu değiştirmeden durdurur. |

Ekran, Xauthority ve D-Bus sorunları
[uzak sunucu sorun giderme](remote-host.md) bölümünde.
Genel daemon hataları [Sorun giderme](troubleshooting.md) sayfasında.
