# Praxi'yi uzak sunucuda çalıştırma (VPS / VM)

Bu rehber Praxi'yi ucuz bir Linux sunucuda her an açık bir sunucu olarak kurar. Örneğin Oracle Cloud
Always Free VM'i (Ampere A1, `aarch64`) ya da herhangi bir `x86_64` VPS olur. Daemon, systemd altında
yetkisiz bir kullanıcı olarak çalışır ve yalnızca makinenin kendisinden erişilebilir.

> **Durum.** Daemon (`bot serve`) ve Telegram kanalı Linux'ta çalışır.
> Düğüm ajanı (`bot-node`) ve iroh eş ağı **deneyseldir**:
> Linux masaüstü arka ucu (`desktop-linux`, AT-SPI2'li X11) vardır, yani
> `bot-node run` Linux'ta bir ekran sunabilir, ama Linux düğüm yolu uçtan uca
> doğrulanmadı. Xvfb ve Chromium masaüstü ve tarayıcı otomasyonu için kurulur, her aracın
> onları şimdiden kullandığı için değil.

> **Sürüm yayımlama (bakımcılar için).** Ön sürüm etiketi gönderimi, onaylama ve doğrulamadan sonra
> yayımlar. Son sürüm, korumalı etiket üzerinde elle başlatmayı ve `release` ortamının onayını
> gerektirir. Bak: [Linux sürüm iş akışı](release-linux.md) ve
> [sürüm kılavuzu](../../contributing/releasing.md).

## Ne gerekir

- Tarayıcı otomasyonlu, her an açık bir sunucu için 6 GB RAM'li (1 çekirdek) ya da 12 GB'lı (2 çekirdek)
  bir sanal makine. Ubuntu 22.04+, Debian 12+ ya da systemd'li bir Oracle Linux / Fedora ailesi sistemi
  ve root erişimi.
- `x86_64` ya da `aarch64` mimarisi (kurulum programı bunu kendisi algılar).
- Bir model sağlayıcısının API anahtarı (bak: [Sağlayıcılar ve modeller](providers-and-models.md)).

## 1. Sürüm paketini al ve doğrula

Bağımsız olarak kurulmuş **GitHub CLI 2.80.0 ya da daha yenisini** kullan. Kurulum programı
doğrulamadan ya da kurulumdan önce bu desteklenen en düşük sürümü denetler. Gerekli `--source-ref`,
`--source-digest`, `--custom-trusted-root` ve imzalayan politikası bayrakları o sürümde bulunur.
`gh` programını [cli.github.com](https://cli.github.com) adresinden al, bir Praxi sürüm dosyasından değil.
Kurulum (`--verify-only` dışındaki her şey) yalnızca `/usr/bin/gh` programını kullanır; bu dosya
root'a ait olmalı ve yalnızca root'un yazabildiği klasörlerde durmalı. Program temiz bir `PATH`,
`HOME` ve `gh` yapılandırmasıyla çalıştırılır; `PATH` içinde başka yerde duran bir `gh` kullanılmaz.
`--verify-only` ise `PATH` içindeki `gh` programını kullanmaya devam eder.

**Linux Server Packages** iş akışı, yanında `.sha256` dosyaları olan `praxi-linux-x86_64.tar.gz` ve
`praxi-linux-aarch64.tar.gz` dosyalarını derler. Son sürümler, korumalı bir etiketten gelen onaylamalar
gerektirir. Kurulum programı yalnızca kaynak ref'i sürüm etiketinin kendisi olan (`refs/tags/<etiket>`)
kaynak kanıtını kabul eder, ön sürümlerde de; `main` üzerinde elle başlatılan çalışma kabul edilmez.
Bir sürüm; arşivleri, onaylanmış `install-linux.sh` dosyasını ve
**`praxi-linux.sigstore.json`** dosyasını içerir. İmzasız dal ya da elle derleme çıktıları bu kurulum
programıyla kurulamaz. Bir sağlama toplamı bütünlüğü denetler, imzalı kaynağın yerini asla tutmaz.

Kurulum programını çalıştırmadan önce doğrula. Güvenilmeyen bir yerden aldığın kurulum programında
`--help` çalıştırmadan önce de bunu yap. Aşağıdakini gözden geçirilmiş bir kopyadan al ve kurmak
istediğin sürüm etiketini yerine yaz:

```bash
(
  set -euo pipefail
  version=v0.1.0
  work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
  base="https://github.com/emreocell/Praxi-Bot/releases/download/$version"
  for asset in install-linux.sh praxi-linux.sigstore.json; do
    curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 "$base/$asset" -o "$work/$asset"
  done
  gh attestation verify "$work/install-linux.sh" --bundle "$work/praxi-linux.sigstore.json" \
    --repo emreocell/Praxi-Bot --hostname github.com \
    --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
    --source-ref "refs/tags/$version" --deny-self-hosted-runners \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256
  sudo bash "$work/install-linux.sh" --version "$version"
)
```

Doğrulanmış kurulum programı, aynı sabit depo, iş akışı, etiket, OIDC sağlayıcısı, yüklem ve
barındırılan çalıştırıcı kısıtlarını arşive, dosyaları açmadan, paketleri kurmadan ya da servisleri
değiştirmeden **önce** uygular. Doğrulamayı atlamanın bir yolu yoktur. `latest` somut bir etikete
çözülür. Tekrarlanabilir kurulum için `--version` değerini sabitle.

## 2. Kurulum, çevrimdışı sunucular dahil

Yerel bir arşiv için her zaman tam sürüm etiketini ver. Yanında bir `praxi-linux.sigstore.json`
dosyası olması gerekir, yolunu `--bundle` ile vermediysen. Yanındaki `.sha256` dosyası ya da `--sha256`
isteğe bağlı bir bütünlük denetimi ekler:

```sh
sudo bash ./install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz
# İmzalı arşivi root, dosya açma ya da kurulum olmadan denetle:
bash ./install-linux.sh --verify-only --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz
```

### Çevrimdışı güven hazırlığı

Güvenilir, çevrimiçi bir makinede, güvendiğin `gh` kurulumuyla güncel Sigstore güvenilir kök anlık
görüntüsünü bağımsız olarak al:

```sh
gh attestation trusted-root > trusted_root.jsonl
```

Güvenilir GitHub CLI'ı, doğrulanmış kurulum programını, mimariye özel arşivi, kaynak bilgisi paketini
ve bu anlık görüntüyü çevrimdışı makineye aktar. Kökü güvendiğin yönetim kanalından aktar. **İndirilen
bir arşivin yanında gelen bir köke kendiliğinden güvenme.** Kurulum programı özel bir kökü yalnızca sen
açıkça verirsen kabul eder. Yeni sürümleri içe aktarırken anlık görüntüyü yeniden oluştur, çevrimdışı
bir makine sonradan yapılan anahtar iptalini keşfedemez. Çevrimdışı paket doğrulaması, depo ve sahip
adlarını imzalandıkları andaki haliyle doğrular, bir yeniden adlandırma ya da ad yeniden kullanımından
sonraki güncel sahipliği değil. Gözden geçirdiğin depo kimliğini kendi sürüm yönetiminde koru. Bak:
GitHub'ın [çevrimdışı doğrulama rehberi](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline).

Aktarılan kurulum programını çalıştırmadan önce onu da çevrimdışı doğrula:

```sh
gh attestation verify ./install-linux.sh --bundle ./praxi-linux.sigstore.json \
  --custom-trusted-root /trusted/trusted_root.jsonl \
  --repo emreocell/Praxi-Bot --hostname github.com \
  --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
  --source-ref refs/tags/v0.1.0 --deny-self-hosted-runners \
  --cert-oidc-issuer https://token.actions.githubusercontent.com \
  --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256

bash ./install-linux.sh --verify-only --offline --version v0.1.0 \
  --file ./praxi-linux-aarch64.tar.gz --custom-trusted-root /trusted/trusted_root.jsonl

# Çalışma zamanı bağımlılıkları çevrimdışı sunucuda önceden kurulu olmalı.
sudo bash ./install-linux.sh --offline --skip-packages --version v0.1.0 \
  --file ./praxi-linux-aarch64.tar.gz --custom-trusted-root /trusted/trusted_root.jsonl
```

`--offline`; `latest` değerini, eksik bir yerel arşivi ya da kökü, paket kurulumunu ve
`--with-desktop` indirmelerini reddeder. `--verify-only` ne root yetkisi ne de çalışma zamanı paketleri
ister. `--custom-trusted-root` ile birlikte yerel bir `--file` de ağ olmadan doğrulayabilir. Açık
çevrimdışı kip ayrıca kurulum seçeneklerini denetler. Eksik, bozuk ya da uyuşmayan kaynak bilgisi veya
kök gereci, işlemi kurulumdan önce durdurur.

Diğer seçenekler: `--port`, `--display`, `--screen`, `--bundle`, `--sha256`,
`--skip-packages`, `--no-start`, `--with-desktop`, `--with-desktop-session`,
`--with-session-apps`, `--without-session-apps`, `--without-desktop-session`,
`--desktop-session <none|xfce>`, `--without-desktop`.
Yeniden çalıştırmak ikili dosyaları yükseltir ve `/var/lib/praxi`, `/etc/praxi/praxi.env` ile
`/etc/praxi/praxi-node.env` dosyalarını korur. Özel bir `BOT_PORT`, `--port` verilmedikçe kalır.
Var olan `praxi-xvfb` / `praxi-desktop` birimleri, `-screen 0` geometrisi ve `DISPLAY`, `--screen`,
`--display`, `--without-desktop-session` ya da `--without-desktop` verilmedikçe kalır (yalnızca çıplak
Xvfb olan bir sunucu da `--with-desktop-session` verilmedikçe çıplak Xvfb olarak kalır). Çevrimiçi bir
sunucuda `--with-desktop` Xvfb ve Chromium'u kurar ya da yeniler. `--with-desktop-session` ise Xvfb'nin
üstüne asgari bir Xfce masaüstü oturumu ekler (yeni bir masaüstü kurulumunda `1920x1080x24`, ya da var
olan Xvfb geometrisi korunarak).

### Sürüm yöneticisi kurulumu ve deneme çalıştırması

Depo ayarları, bir iş akışı birleştirilerek oluşmaz. Bir son sürümden önce yöneticinin şunları
ayarlaması gerekir:

1. `refs/tags/v*` ile eşleşen **etkin bir etiket kural seti**: etiket oluşturmayı sürüm
   bakımcılarıyla sınırlar, güncellemeyi ve silmeyi yasaklar. Atlama izinlerini yalnızca amaçlanan
   sürüm yöneticileriyle sınırlı tut.
2. Gerekli gözden geçirenleri olan, **Prevent self-review** açık ve dağıtım etiketleri `v*` ile
   sınırlı **`release`** adlı bir ortam. Desteklendiği yerde yönetici atlamasını kapat.

Onaylama işi bu ortama başvurur ve GitHub güncel etiketin korumalı olduğunu, ortam API'si de kendi
kendini gözden geçirmeyi engelleyen gerekli gözden geçirenleri bildirmedikçe kapalı kalır. Belirteç
bu politikayı okuyamazsa iş başarısız olur, onayı asla varsaymaz. Yayımlama işi aynı ortamla ve
bağımsız gerçek doğrulama işiyle kapılıdır.

Gerçek Sigstore/`gh` davranışını varlık yayımlamadan denemek için, var olan bir sürüm etiketi üzerinde
**Linux Server Packages** iş akışını **`dry_run: true`** ile başlat (varsayılan budur). Son etiket
çalışması yine `release` ortamının onayını gerektirir. İş akışı iki mimariyi de derler, etiketlenmiş
girdileri onaylar ve imzalı tüm varlıkları gerçek `gh` ile doğrular. Sonra arşivi, ağsız ve temiz bir
`gh` yapılandırması/önbelleği olan bir Linux ağ ad alanı içinde, bağımsız olarak alınmış bir kökle
doğrular ve kurcalanmış bir çıktıyı, yanlış etiketi ve yanlış depoyu reddeder. Dal ve PR doğrulaması
denetimli bir doğrulayıcı sahte düzeni kullanır. Etiketlemeden önce gruplanmış notları önizlemek için
[yerel sürüm provası](../../contributing/releasing.md) sayfasını kullan.

Yayımlama, doğrulanmış etiket için bir sürüm oluşturur ya da notlarını değiştirmeden var olan bir
sürüme eksik Linux varlıklarını ekler. Herhangi bir varlık adı zaten varsa yayımlama durur. Değişen
çıktılar için yeni bir sürüm yayımla. Aynı çalışmanın imzalı paketi iki tar dosyasının ve etiketli
kurulum programının yanında gönderilir.

Betik şunları yapar:

- ikili dosyaların bağlandığı çalışma zamanı kütüphanelerini kurar (`libdbus`, `libssl.so.3`);
- oturum açma kabuğu olmayan bir `praxi` sistem kullanıcısı oluşturur;
- ikili dosyaları `/opt/praxi` içine kurar ve veriyi `/var/lib/praxi` içinde tutar;
- tar üyelerini `tar -tzf` ile listeler, mutlak bir yolu ya da `..` bileşenini reddeder, sonra dosyaları
  yalnızca geçici bir dizine açar;
- `praxi.service` için `/etc/praxi/praxi.env` dosyasını yazar (kip 0640, root:`praxi`); içinde veri
  dizini, port ve şifreli dosya gizli bilgi deposu için rastgele bir parola bulunur (başsız
  sunucularda işletim sistemi anahtarlığı yoktur);
- `praxi-node.service` için `/etc/praxi/praxi-node.env` dosyasını yazar (kip 0640, root:`praxi`); içinde
  veri dizini, ev dizini ve sanal ekran kullanılıyorsa `DISPLAY` bulunur. Bu dosya
  `BOT_SECRETS_PASSPHRASE` içermez;
- `praxi.service` (`bot serve`, yalnızca 127.0.0.1) birimini kurar ve etkinleştirir, ayrıca
  `praxi-node.service` birimini kurar (etkinleştirmez, aşağıya bak);
- `--with-desktop` ile `praxi-xvfb.service` (sanal ekran `:99`, `-nolisten tcp`,
  `/var/lib/praxi/.Xauthority` içinde özel `0600` Xauthority, `-fbdir` kare dosyası yok) ve snap
  olmayan bir Chromium tarayıcısı kurar (kaynak seçimi aşağıda). Xvfb başlamadan önce, paylaşılan özel
  `/tmp` içinde root kullanıcısına ait, kip 1777 X11 soket dizinini hazırlar. Sonraki bir çalışma bu birimi
  `--without-desktop` verilene kadar tutar;
- `--with-desktop-session` ile `praxi-desktop.service` içinde asgari Xfce de kurar (`xfwm4
  --compositor=off`, Applications menülü `xfce4-panel`, `xfdesktop`, özel `praxi` D-Bus oturum yolu +
  AT-SPI2). Yeni masaüstü kurulumlarında başlangıç `1920x1080x24`'tür (yükseltmelerde var olan
  geometri korunur). Ekran kilidi, güç ve yazıcı otomatik başlatmaları kapalıdır ve koşulsuz bir
  tarayıcı otomatik başlatması yoktur (`--with-session-apps` açıkça verilmedikçe `xfce4-terminal` ve
  `thunar` gizlenir ve varsayılan olarak dışarıda bırakılır);
- masaüstü kipi etkinken `praxi.slice` uygular (`CPUQuota=100%`, `MemoryHigh=3G`, `MemoryMax=4G`);
  bu, `praxi.service`, `praxi-node.service`, `praxi-xvfb.service`, `praxi-desktop.service`, tarayıcı alt
  süreçleri ve menüden başlatılan uygulamalar için geçerlidir (masaüstü kipi olmayan sunucular
  `praxi.slice` sınırlarını almaz). Ayrıca systemd sertleştirmesini uygular: `NoNewPrivileges`,
  `ReadWritePaths=/var/lib/praxi` ile `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`,
  `PrivateDevices`, çekirdek ve cgroup korumaları;
- `praxi` kullanıcısı olarak çalışan `praxi` (`praxi.env` kullanan `bot` CLI'ı) ve `praxi-node`
  (`praxi-node.env` kullanan) sarmalayıcı komutlarını ekler.

Kurulum programı, art arda iki kez etkin birim ve geçerli loopback sağlık denetimi görene kadar en fazla
30 deneme bekler (her HTTP isteğinin iki saniyelik zaman aşımı vardır). Başarısızlıkta son 50 servis
journal kaydını yazar, ikili dosyaları, birimleri, sarmalayıcıları ve ortam dosyalarını önceki çalışan
kuruluma geri alır, önceden etkin olan servisleri yeniden başlatır ve sıfırdan farklı bir kodla çıkar.
`--no-start` yalnızca dosyaları kurar/etkinleştirir, başlatmayı ve hazır olma denetimini açıkça atlar.

Yeni bir daemon, HTTP 200 ve `status: "setup_required"` sağlık yanıtıyla çalışır durumda kalır.
Doctor, sağlayıcılar için **setup required** bildirir. Kurulum API'leri kullanılabilir. Bir sohbet turu
göndermek, yapılandırma adımlarıyla birlikte HTTP 409 `setup_required` döndürür. Yer tutucu bir
sağlayıcı yoktur. Çıkarımı açmak ve sağlığı `status: "ok"` yapmak için bir sağlayıcı ayarla ve daemon'u
yeniden başlat (`sudo systemctl restart praxi`) ya da `POST /v1/providers/reload` kullan.

## 3. Ayarla ve eşleştir

```sh
sudo praxi setup           # sağlayıcı, API anahtarı, ilk bot, izin hazır ayarı
sudo praxi doctor          # kurulumu denetle
sudo praxi telegram pair   # Telegram hesabını eşleştirmek için tek kullanımlık kod
journalctl -u praxi -f     # günlükler
```

Telefonundan [Telegram](telegram.md) ile ya da bilgisayarından bir SSH tüneliyle sohbet et:

```sh
ssh -L 47821:127.0.0.1:47821 user@your-server
```

API token'ı `/var/lib/praxi` içinde durur (yalnızca `praxi` okuyabilir). Tüneldeki istemciler, yerel bir
kurulumdaki gibi onunla kimlik doğrular.

### Sunucuyu masaüstü uygulamasından kullanma

Masaüstü uygulaması sunucunun ekranını (Xvfb masaüstünü ve botun tarayıcısını) gösterebilir, ekranı
devralıp geri verebilir ve **sunucuda** çalışan botlarla sohbet edebilir. Fazladan bir porta ihtiyaç
duymaz: uygulamanın kendi daemon'u sunucuya dışa doğru bir iroh kanalı açar ve her istek, sunucunun
eş koruması tarafından eşleştirilmiş bir *Masaüstü* olarak denetlenir.

Sahibi olan bilgisayardan kurmak ve eşleştirmek (`bot host add`) ile günlük denetimler
[Sunucu işletimi](host-operations.md) sayfasında. Bu rehberin elle kurduğu bir sunucuda:

1. Sunucunun sunucu olarak çalışması gerekir: `bot serve --host` (bu, eş ağını da açar). Kurulu bir
   `praxi.service`, `bot serve` çalıştırır. Bir drop-in ile (`sudo systemctl edit praxi`) bunu
   geçersiz kıl:

   ```ini
   [Service]
   ExecStart=
   ExecStart=/opt/praxi/bin/bot serve --host
   ```

2. Sunucuda masaüstü için tek kullanımlık bir eşleştirme bileti oluştur ve kopyala:
   `praxi host invite --role desktop`
3. Uygulamada üstteki bilgisayar seçiciyi aç, **Sunucu ekle** seçeneğini seç ve bileti yapıştır.
   Seçici bundan sonra sunucuyu “Bu bilgisayar” öğesinin yanında listeler.
4. Yönetici bir masaüstü (`sudo praxi host invite --role desktop` için varsayılandır; bu komut
   sunucunun sahip anahtarını gönderir), ek bir komut olmadan ekranı izleyebilir ve kullanabilir.
   Eşleştirilmiş diğer hiçbir cihaz, sen sunucuda izin verene kadar bunu yapamaz. İzin yeniden
   başlatmalardan sonra da kalır:
   `praxi host screen --all-desktops` (yalnızca izlemek için `--view-only` ekle, izni kaldırmak için
   `--revoke` ekle; bu yönetici masaüstünün varsayılan iznini de kaldırır).
   `praxi host devices` cihaz anahtarlarını listeler. `bot host add` eşleştirirken bu izni kendisi
   kaydeder.

Bir sunucuda sıradan, eşleştirilmiş bir Masaüstü botları, sohbetleri ve çalışmaları yönetebilir ve
ekranı kullanabilir, ama sahip değildir. Sahibin **yönetici** olarak onayladığı bir Masaüstü
(`sudo praxi host invite --role desktop` için varsayılandır, daha önce eşleştirilmiş biri için
`bot host admin --all-desktops`) o sunucunun sahibidir: orada sahibin yapabildiği her şeyi yapabilir.
Buna yürütme politikası, sağlayıcı ve posta/arama/dikte anahtarları (yalnızca yazılır: sunucuda
saklanır ve bir daha gösterilmez), gizli bilgiler, MCP sunucuları ve beceriler, posta kanalı ve
koruması, harcama limitleri, bir botun izinleri, motor ve bilgi ayarları, sohbet geçmişinde arama ve
dışa aktarma ile denetim günlüğünü okuma dahildir. Böyle her değişiklik sunucunun denetim günlüğüne;
masaüstü, değişikliğin türü, yöntem ve yol olarak kaydedilir (içerik asla).

Yönetici bir masaüstünün bile bilerek yapamadığı şeyler sunucunun kendisinde yapılır: cihazları
eşleştirmek ve kimin yönetici olduğunu değiştirmek, bekleme, etkinleştirme, güncellemeler, durum içe
aktarma ve devretme (`/v1/host…`); sunucunun diğer bilgisayarlara kendi bağlantıları (`/v1/hosts…`);
MCP cephesi için kimlik bilgileri; doctor onarımları; bir Telegram hesabını eşleştirmek; cihazların
ekran izinleri (`bot host screen`) ve imzalı webhook girişi.
Sahip anahtarının kendisi bağlantı üzerinden asla gitmez. Telefonlar, düğümler, diğer sunucular ve
onaylanmamış Masaüstleri bunların hiçbirini alamaz. Onaylanmamış bir Masaüstüne sunucuda
`bot host admin --all-desktops` çalıştırması söylenir. Ekran girdi izni hâlâ elinde olan bir yönetici
masaüstü, kendi girdi iznini kaybetmeden, sunucudaki duraklatılmış ekran devralmasını masaüstü
bannerından temizleyebilir (`POST /v1/control/recover`). Sunucunun kendi model anahtarı olana kadar
uygulama “bu sunucunun henüz model anahtarı yok” der: ekran ve bot yönetimi çalışır, sohbetler
yanıtlanamaz. Anahtarı sunucuya `praxi setup` ile koy (bak: 3. bölüm).

Masaüstünün canlı görüntüsünün doğrudan QUIC üzerinden mi bağlandığını yoksa bir röleye mi düştüğünü
anlamak (ve bir UDP portu açmadan önce kare hızını ve RTT'yi ölçmek) için `bot --remote <ENDPOINT> proxy`
ile bir tünel aç ve [Canlı görüntü bağlantısını ölçme](../../design/live-view-link-measurement.md)
sayfasını izle.

#### Eşleştirilmiş bir sunucuyu botun tarayıcı bilgisayarı olarak kullanma

**Yerel** bir botun (yerel model anahtarlarını, kodlama CLI'larını, dosyalarını ve kabuğunu
kullanan) bilgisayarın yerine her an açık Linux sunucundan gezinmesini istiyorsan, o botun profilinde
`[runtime]` altında `computer = "server:<key>"` ayarla (bak: [Bilgisayar hedefi](bots-and-profiles.md)):

- **Nasıl kullanılır**: Bu masaüstünü sunucuyla onun yönetici masaüstü olarak eşleştir (`bot host add`
  ya da `sudo praxi host invite --role desktop`; sunucu yönetimi var olmadan önce eşleştirilmiş
  sunucularda bir kez `sudo praxi host admin <key>` gerekir). Sunucunun cihaz anahtarını
  `[runtime] computer = "server:<key>"` içine yaz ve botu `bot bots add` ya da API ile oluştur/güncelle.
- **Neler yapabilir, neler yapamaz**: Sunucuda yalnızca `browser_*` araçları çalışır (bot başına yalıtılmış
  bir tarayıcıda, aynı anda en fazla 8 açık). `fs_*` ve `shell_run` yine yerel bilgisayarında çalışır,
  `desktop_*` araçları sunucuyu denetleyemez. Sunucu çevrimdışıysa ya da eşleşmemişse çalışma, bilgisayarsız
  olarak kapalı kalarak başarısız olur ve yerel tarayıcına asla düşmez.
- **Sunucudaki güvenlik sınırları**:
  - **Yalnızca yönetici masaüstü**: Sunucu her istekte eşleştirme dosyasını yeniden okur ve çağrıları
    yalnızca `admin` bayraklı ve eşleştirme kuşağı uyan eşleştirilmiş bir `desktop` cihazından kabul
    eder. Telefonlar, düğümler, diğer sunucular ve yönetici olmayan masaüstleri reddedilir.
  - **İç ağa erişim yok**: Yalnızca `http://` ve `https://` adresleri (en fazla 2.000 karakter) kabul
    edilir. Bir süzgeç proxy, yönlendirmeler ya da alt kaynaklar üzerinden bile olsa loopback'i
    (`127.0.0.1`, `[::1]`), link-local/bulut meta verisini (`169.254.169.254`) ve özel ağ aralıklarını
    engeller. Sunucuda bunu aşmanın bir yolu yoktur.
  - **Betik yok, parola alanı yok, saklı gizli bilgi yok**: `browser_eval` reddedilir, parola/hassas
    alanlara gezinme dışı tuşlar doldurmak ya da yazmak kapalı kalarak başarısız olur, saklı doldurma
    gizli bilgileri gönderilmez ve dönen metin sunucu tarafından kırpılır.
  - **Denetlenir ve yerelde onaylanır**: Her tarayıcı çağrısı (izin verilen ya da reddedilen) sunucunun
    denetim günlüğüne yazılır. Yazılamayan bir denetim günlüğü çağrıyı engeller. Onay istemleri yerel
    bilgisayarında kalır ve izinler `server:<key>` ile sınırlıdır.

### Başka bir çekirdeğin bu makineyi denetlemesine izin verme (deneysel)

`bot-node run` için ulaşılabilir bir X11 ekranı (fiziksel bir ekran ya da `--with-desktop` ile gelen
Xvfb ekranı; Wayland reddedilir) ve en az bir eşleştirilmiş çekirdek gerekir. Bunlar olmadan çıkar ve
`praxi-node.service` onu her 10 saniyede bir yeniden başlatırdı, bu yüzden birim kurulur ama
etkinleştirilmez. Önce eşleştir, sonra etkinleştir.

`bot-node`, iroh üzerinden (röle yedekli QUIC) **dışa doğru** bağlanır, yani gelen bir porta gerek
yoktur. Denetlenecek makinede:

```sh
sudo praxi-node join '<bot-pair:...>'     # çekirdekten alınan tek kullanımlık bilet
```

Bilet tek kullanımlıktır. Sonrasında cihazlar saklı anahtarları kullanır. Model için tasarım
belgelerindeki ADR-0006'ya bak. `praxi-node.service` birimini yalnızca `bot-node join` başarılı
olduktan ve `bot-node run` elle başladıktan sonra etkinleştir.

## 4. Güvenlik duvarı notları (Oracle Cloud)

Praxi için hiçbir portu açman **gerekmez**. Öyle de bırak.

- VCN güvenlik listesinde / ağ güvenlik grubunda yalnızca SSH (22) gelen olarak kalsın. 47821 için
  bir giriş kuralı ekleme.
- Oracle Ubuntu imajları iptables kurallarıyla da gelir. Daemon portu için bir `ACCEPT` kuralı
  da ekleme. `sudo ss -tlnp | grep 47821` ile denetle: `127.0.0.1` göstermeli, asla `0.0.0.0` ya da
  `[::]` göstermemeli.
- Giden trafiğe izin verilmeli (varsayılan budur): sağlayıcı API'si, Telegram ve iroh röleleri giden
  bağlantılar kullanır.
- Always Free ARM kapasitesi ve sınırları değişir. Güncel koşulları Oracle'dan doğrula. Boşta duran
  örnekler geri alınabilir, bu yüzden `/var/lib/praxi` dizininin yedeklerini tut.

## 5. Güvenlik

- **Daemon portunu asla herkese açma**: ne bir güvenlik listesiyle, ne bir ters proxy ile, ne de
  `ssh -R` ile. Daemon API'si sunucuda araç çalıştırabilir. Token'ı tek korumadır. `bot serve`,
  tasarım gereği 127.0.0.1 adresine bağlanır.
- Ona SSH tünelleri, Telegram eşleştirmesi ya da iroh eşleştirmesiyle ulaş. `praxi-node` bağlantısı
  yalnızca dışa doğrudur ve tek seferlik eşleştirmeden sonra cihaz başına anahtarlar kullanır.
- `/etc/praxi/praxi.env` dosyasını koru: gizli bilgi deposunun parolasını taşır.
  `praxi-node.service` bunun yerine `/etc/praxi/praxi-node.env` dosyasını okur ve o dosyada
  `BOT_SECRETS_PASSPHRASE` yoktur. İki dosyanın da kipi 0640'tır. Onları commit etme ya da yapıştırma.
- İzin hazır ayarını **Yıkıcı işlemlerde onay** ya da daha katı bir düzeyde tut (bak:
  [Sohbet ve onaylar](chatting-and-approvals.md)). Chromium ve sanal ekranı olan bir sunucu güçlü bir
  makinedir. Kişisel veri bulunmayan ayrı bir sanal makine kullan.
- Yalnızca SSH anahtarları kullan ve işletim sistemini güncel tut (Ubuntu'da `unattended-upgrades`).

### Tarayıcı kaynağı ve servis tanılama

`--with-desktop`, Debian'da Debian'ın yerel `chromium` adayını kullanır. Ubuntu'da (24.04 ARM64 dahil)
**Google Chrome for Testing 157.0.8103.38** sürümünü, Google'ın resmî
[Chrome for Testing arşivinden](https://github.com/GoogleChromeLabs/chrome-for-testing) doğrudan HTTPS
üzerinden `/opt/praxi-browser` içine indirir. İşlemci seçimi açıktır: x86_64 için `linux64`, aarch64 için
`linux-arm64`. [Resmî indirme meta verisi](https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json)
artık Linux ARM64 derlemelerini yayımlıyor (153 sürümünden beri). Google'ın Linux tarayıcı
derlemelerinin yalnızca x86_64 olduğunu söyleyen eski tavsiye bu dağıtım için artık geçerli değildir.
Sabitlenmiş arşiv HTTPS ile teslim edilir ve açılmadan önce her mimari için sabitlenmiş SHA-256
sağlama toplamlarıyla doğrulanır. Daha yeni bir tarayıcı sürümü almak için güncellenmiş bir kurulum
programını yeniden çalıştır. Chrome for Testing kendiliğinden güncellenmez.

`scripts/install-linux.sh` içindeki sabitlenmiş Chrome for Testing sürümünü ve sağlama toplamlarını
güncellemek için, yeni sürümün arşivlerini iki platform için de (`linux64` ve `linux-arm64`)
`https://storage.googleapis.com/chrome-for-testing-public/<version>/<platform>/chrome-<platform>.zip`
adresinden indir, `sha256sum` ile özetlerini hesapla ve `install_cft` işlevinin başındaki `version`,
`sha256_linux64` ve `sha256_linux_arm64` değerlerini birlikte güncelle; aynı sürümü, sağlama
toplamlarını ve arşiv boyutlarını `crates/daemon-api/src/browser_install.rs` içinde de güncelle
(iki yerdeki sağlama toplamları ya da sürüm farklıysa bir test başarısız olur). Kurulum betiğiyle
kurulmamış bir bilgisayarda `bot browser install` ve Ayarlar > Tanılama > Tarayıcıyı kur'un veri
klasörüne indirdiği, bu Rust kopyasıdır.

Fedora/RHEL, yöneticinin etkinleştirdiği RPM depolarından zayıf bağımlılıklar olmadan `chromium`
kullanır. Bu depolarda `chromium` yoksa (örneğin Oracle Linux), kurulum programı her iki işlemci için
de Ubuntu'daki aynı sabitlenmiş Chrome for Testing arşivine döner. Hiçbir PPA ya da snap eklenmez.
Bir xtradeb PPA'sı üçüncü taraf güveni ekler ve resmî ARM64 arşiviyle gerekmez. Playwright da ARM64'ü
destekler ama bu kurulum programının ihtiyaç duymadığı ayrı bir çalışma zamanı ve tarayıcı yaşam
döngüsü ekler.

Ubuntu'nun `chromium-browser` ve sanal `chromium` paketleri snap'e çözülebilir. Kurulum programı onları
asla seçmez. Yalnızca snap bulan keşif ve snap geçersiz kılmaları desteklenmez ve net bir tanılama
üretir. `--skip-packages` ile snap olmayan, çalışan bir tarayıcıyı kendin sağla. `BOT_BROWSER_PATH`
keşfi geçersiz kılar. `PRAXI_BROWSER_PATH` uyumlu bir yedek olarak kalır. Geçersiz bir geçersiz kılma,
sessizce başka bir tarayıcı seçmek yerine başarısız olur.

Kurulum programını yeniden çalıştırmak (`bot host repair`, yükseltmeler) tarayıcı için idempotenttir:
sabitlenmiş sürümde çalışan bir `/opt/praxi-browser` korunur ve yeniden indirilmez, paylaşılan
kütüphaneler, AppArmor profili ve yazı tipleri ise yeniden denetlenir. Kurulumdan sonra kurulum
programı tarayıcının başladığını ve tüm paylaşılan kütüphanelerinin bulunduğunu denetler, değilse eksik
adlarla başarısız olur. İndirme başarısız olursa (`storage.googleapis.com` adresine yol yok, bir proxy,
dolu bir disk), kurulum programı nedenle ve şu sonraki adımlarla durur: sunucudan ağı denetle, aynı
komutu yeniden çalıştır (kaldığı yerden sürer) ya da snap olmayan bir Chrome veya Chromium'u kendin
kur, `BOT_BROWSER_PATH` ayarla ve `--skip-packages` ile yeniden çalıştır.
`bot host add`/`repair`, sanal ekran satırının yanında bir **Botun bilgisayarı için tarayıcı** sağlık
satırı bildirir (hiçbiri kurulu değilse uyarı).

**Yazı tipleri.** `--with-desktop` ile kurulum programı, Noto ve Liberation ailelerinin yanında
`fonts-noto-color-emoji` ve `fonts-noto-cjk` paketlerini de kurar (Fedora/RHEL:
`google-noto-emoji-color-fonts`, `google-noto-sans-cjk-ttc-fonts`); böylece Türkçe, Japonca/Çince/Korece
metin ve emoji, tarayıcıda ve ekran görüntülerinde boş kutular yerine düzgün görünür. Bulunmayan ya da
kurulamayan bir yazı tipi paketi bir uyarıdır, kurulum hatası değil.

Tarayıcı **libcups** istemci kütüphanesini ister, yazdırma sunucusuna ihtiyaç duymaz. Paketler
`--no-install-recommends` ile kurulur. Kurulum programı `cups.service`, `cups.socket`, `cups.path`,
`cups-browsed.service` ve varsa kurulu bir CUPS snap'ini, paket hatasından sonra bile, devre dışı
bırakır ve durdurur. Bu kurulum programı ayrılmış bir bot sunucusu içindir: önceden var olan yazdırmayı
da kapatır. Bir yazdırma sunucusunda kullanma. Hiçbir tarayıcı ya da masaüstü TCP dinleyicisi
etkinleştirilmez.

`sudo praxi doctor`, daemon ile aynı kullanıcı, env dosyası, çalışma dizini, sertleştirme ve Xvfb özel
ad alanıyla kısa ömürlü geçici bir birim başlatır. Tarayıcı satırı gerçekten başsız olarak başlatır,
CDP bağlantısı kurar ve tarayıcıyı kapatır; beş saniyelik başlatma / sekiz saniyelik toplam zaman
aşımı vardır. Başarısızlık, snap olmayan / yol ipucuyla birlikte **fail** olarak bildirilir.
`host_display` satırı sırayla `DISPLAY` değerinin ayarlı olduğunu, ekranın (okunabilir bir
`XAUTHORITY` ile) yanıt verdiğini ve kullanılabilir, snap olmayan bir tarayıcının bulunduğunu denetler
ve ilk eksik parçayı adıyla söyler. `free_ram` Linux `MemAvailable` değerini bildirir ve 512 MiB altında
uyarır. Doğrudan `bot doctor` yalnızca o anki kullanıcıyı/ortamı yoklar, servis doğrulaması için
sarmalayıcıyı kullan. Canlı çalışmalar sırasında `browser_open` başlatmayı 40 saniyeyle sınırlar
(meşgul bir oturumu beklerken 45 saniye), erken çıkışta Chrome'un stderr çıktısının sonunu bir neden
ipucuyla gösterir ve ulaşılamayan bir Linux X11 ekranını her 15 saniyede bir yeniden yoklar; böylece
yavaş başlayan bir Xvfb daemon'u başsız kipte takılı bırakmaz.
Ubuntu 24.04'ün AppArmor kullanıcı ad alanı kısıtlaması, Ubuntu'nun Chrome profilini izleyerek
`/opt/praxi-browser/chrome` için `userns` izni veren, adı konmuş bir `praxi-browser` profiliyle
karşılanır. Chromium korumalı alanı ve systemd sertleştirmesi açık kalır, `--no-sandbox` ya da geniş bir
birim gevşetmesi uygulanmaz.

### Masaüstü oturumu, uygulama menüsü ve tarayıcı yaşam döngüsü

Tek başına Xvfb kullanılabilir bir masaüstü **sağlamaz** (pencere yöneticisi, panel ya da uygulama
başlatıcı yoktur). `--with-desktop-session` eklemek (ya da
`sudo bash scripts/linux/setup-headless.sh --system` çalıştırmak), Xvfb üzerinde
(yeni masaüstü kurulumlarında `1920x1080x24`, ya da `--screen` verilmedikçe var olan
`praxi-xvfb.service` geometrisi korunarak) aynı yetkisiz `praxi` kullanıcısı olarak çalışan asgari bir
Xfce oturumu (`praxi-desktop.service`) kurar:

- **Uygulama menüsü kullanımı ve güvenlik duruşu:** Canlı görüntü devralma sırasında tarayıcıyı
  başlatmak için masaüstündeki **Web Browser** simgesini ya da `xfce4-panel` üzerindeki sol üst
  **Applications** menüsünü aç (elle başlatma. Asla kendiliğinden başlamaz). `xfce4-terminal` ve
  `thunar` varsayılan olarak kurulmaz ve menüde gösterilmez. Açıkça ihtiyacın varsa
  `--with-session-apps`, onları yeniden gizlemek için `--without-session-apps` ver. Unutma:
  `--without-session-apps` yalnızca `Hidden=true` `.desktop` geçersiz kılmaları yazar ve onları
  `helpers.rc` dosyasından çıkarır. Daha önceki bir çalışma ya da temel işletim sistemi tarafından
  kurulmuşlarsa `xfce4-terminal` ve `thunar` programlarını **kaldırmaz** (ikili dosyaları,
  `apt-get remove` / `dnf remove` ile açıkça kaldırılana kadar diskte durur).
  Başlatılan tüm uygulamalar `praxi.slice` sınırlamasını devralır ve `DISPLAY`, `XAUTHORITY` ve
  `DBUS_SESSION_BUS_ADDRESS` ile `praxi` kullanıcısı olarak çalışır. Dikkat: `praxi-desktop.service`
  oturum ortamına `BOT_SECRETS_PASSPHRASE` koymasa da, `praxi` olarak çalışan her uygulama
  `/etc/praxi/praxi.env` dosyasını (`0640 root:praxi`, `BOT_SECRETS_PASSPHRASE` içerir) dosya
  sistemi üzerinden okuyabilir ve `/var/lib/praxi` dizinine okuma/yazma erişimine sahiptir.
  `--with-session-apps` ile bir terminal ya da dosya yöneticisi açığa çıkarmak, canlı görüntü
  operatörünün (ya da korumasız bir arayüz tıklamasının) bu dosyaları okumasına ve daemon'un kabuk
  izin motorunun dışında `praxi` olarak komut çalıştırmasına izin verir.
- **Kaynak ve ekran sınırlamaları:** Yalnızca X11/Xvfb (`XDG_SESSION_TYPE=x11`), Wayland desteklenmez.
  Her ekran için yalnızca bir ekran sahibine izin verilir (`:99`, `praxi-xvfb.service` tarafından
  sahiplenilir). VNC, RDP, X11 TCP (`-nolisten tcp`), `xhost +`, `-fbdir` kare arabelleği dökümü ya da
  varsayılan kare disk depolaması etkin değildir ve yeni dinleme portu açılmaz. `-auth` olmayan eski
  bir çıplak Xvfb birimini tutmak, Xauthority kimlik doğrulamasını dışarıda bırakır ve Xvfb'nin soyut
  Unix soketini diğer yerel hesapların erişimine açık bırakır. Onu özel `0600` Xauthority'ye yükseltmek
  için `--with-desktop` ya da `--with-desktop-session` ver.
- **Tembel, boşta kapanan tarayıcı politikası:** Chromium'u Xfce ya da systemd asla kendiliğinden
  başlatmaz. Yönetilen `CdpDriver` örnekleri daemon'da (`bot serve`) varsayılan olarak **120 sn** boşta
  kapanma süresiyle (`/var/lib/praxi/host.toml` içinde `[limits] browser_idle_seconds = 120`) ve RAM'e
  dayalı bir eşzamanlılık üst sınırıyla (8 GiB RAM altında `1`, 8 GiB ve üstünde `2` çalışma, ya da
  `host.toml` içinde `[limits] max_computer_runs`) çalışır. Kurulum programı `BOT_MAX_COMPUTER_RUNS` ya
  da `BOT_BROWSER_IDLE_SECONDS` değerlerini `/etc/praxi/praxi.env` içine sabitlemez ya da üzerine
  yazmaz; böylece `host.toml` ve daemon varsayılanları etkin kalır. Bu ortam değişkenlerini
  `praxi.env` içinde ayarlamak `host.toml` değerini geçersiz kılar. İzleme akışları tarayıcıyı başlatmaz
  ya da sabitlemez. Etkin bir bot bilgisayar çalışması ya da etkin bir devralma onu sabitler. Adlandırılmış
  tarayıcı profilleri `/var/lib/praxi/browsers` altında kalır ve boşta kapanmadan sonra da korunur.
  Bir insanın Applications menüsünden elle açtığı her tarayıcı `CdpDriver` boşta kapanmasının dışındadır
  ve yine `praxi.slice` RAM'ine sayılır.
  *(Not: `BrowserSettings::isolated`, başlı devralma/bilgisayar kullanımı hedefleri için geçersiz
  kılınmadıkça varsayılan olarak `headless: true` kullanır.)*

### Küçük sunucu kaynak denetimi

Üretim sunucuları önceden derlenmiş sürüm tar dosyalarını çalıştırır. Rust'ı 1 çekirdekli/6 GB'lık
(ya da 2 çekirdekli/12 GB'lık) makinede derleme. Chromium'u tembel tut: bu kurulum programı hiçbir
zaman kalıcı bir tarayıcı başlatmaz. Doctor yoklaması denetledikten sonra tarayıcısını kapatır.
Plan hedefleri şunlardır: ekran + oturum boştayken ≤500 MiB (yaklaşık 300 MB'lık Xfce rakamı bir plan
tahminidir, garanti değil), tüm Praxi süreç ağacı için en yüksekte **<2,5 GB** (`2.500.000.000` bayt /
yaklaşık 2,33 GiB) ve boşta CPU için bir çekirdeğin ≤%5'i. `praxi.slice`, `CPUQuota=100%`,
`MemoryHigh=3G`, `MemoryMax=4G` değerlerini sınırlama olarak uygular. Gerçek RAM'i ölçmenin yerine
geçmez.

Tekrarlanabilir ölçüm aracını sunucuda çalıştır (yalnızca metin/JSON ölçümleri çıkarır. Asla
`/proc/<pid>/cmdline`, `/proc/<pid>/environ`, kareleri, başlıkları ya da adresleri okumaz):

```sh
sudo python3 scripts/linux/measure-budget.py \
  --label desktop-idle --duration 30 --interval 0.5 \
  --require-slice-containment --check-budget --format both
```

cgroup sayaçlarını doğrudan da inceleyebilirsin:
`sudo systemctl show praxi.slice praxi praxi-desktop praxi-xvfb -p MemoryCurrent -p MemoryPeak -p CPUUsageNSec`.
Toplanan süreç `VmRSS` değerinin süreçler arasında paylaşılan sayfaları iki kez saydığını unutma.
`praxi.slice/memory.peak` yazılabilir olduğunda (root olarak Linux 6.12+), `measure-budget.py` onu
pencerenin başında sıfırlar ve gerçek çekirdek pencere tepe değerini (artı `praxi.slice` dışına kaçmış
Praxi süreçlerini) denetler. `memory.peak` değerinin salt okunur olduğu eski çekirdeklerde, bütçeyi
aşan ömür boyu `memory.peak` değeri `--check-budget` denetimini sonuçsuz diye başarısız kılar.
`praxi.slice` yokluğu, gözlenen sıfır Praxi süreci ya da `praxi.slice` dışına kaçan herhangi bir süreç
de `--check-budget` denetimini başarısız kılar.

### Masaüstü, Xauthority, D-Bus ve ad alanları sorun giderme

- **Tüm Praxi servislerini düzgünce durdurma:** systemd 249+ sürümünde `praxi-xvfb.service`,
  `Upholds=praxi-desktop.service praxi.service` ayarlar; bu yüzden `praxi-xvfb.service` etkin
  kaldığı sürece `sudo systemctl stop praxi` hemen yeniden başlatılır. Her şeyi düzgünce durdurmak
  için `sudo systemctl stop praxi-xvfb.service` kullan (bu, `BindsTo=` ile `praxi-desktop.service` ve
  `praxi.service` birimlerini de durdurur). systemd < 249 olan eski sunucularda `Upholds=` bir kurulum
  uyarısıyla atlanır ve `praxi-xvfb.service`, Xvfb başladıktan ya da bir çökmeden toparlandıktan sonra
  etkin bağımlıları yeni ad alanında başlatmak için bir `ExecStartPost=!` ayakta tutma yedeği
  (`praxi-xvfb-uphold.sh`) kullanır. `praxi.service`, bir masaüstü oturumu hatasının her an açık
  daemon'u engellememesi için `Wants=praxi-desktop.service` kullanır.
- **Xauthority izni / kimlik doğrulama hataları (`Invalid MIT-MAGIC-COOKIE-1`):**
  `/var/lib/praxi/.Xauthority` kip `0600` ve `praxi:praxi` sahipliğiyle oluşturulur ve zaten geçerliyse
  `praxi-xvfb.service` yeniden başlatmalarında yeniden kullanılır. `praxi-desktop.service` ve
  `praxi.service`, beklenmedik bir Xvfb çıkışı bağımlıları durdurup temiz biçimde geri getirsin diye
  `praxi-xvfb.service` birimine bağlanır (`BindsTo=`/`After=`, `praxi-xvfb.service` üzerinde
  `Upholds=` ile). Yeni bir çerez zorlamak için `/var/lib/praxi/.Xauthority` dosyasını sil ve
  `sudo systemctl restart praxi-xvfb.service` çalıştır.
- **X11 soketi / `PrivateTmp` ad alanı uyuşmazlığı (`Can't open display :99`):**
  `praxi-xvfb.service`, `praxi-desktop.service`, `praxi.service` ve `praxi-node.service`, Xvfb bağlama
  ad alanını `/etc/systemd/system/praxi*.service.d/desktop.conf` içindeki
  `JoinsNamespaceOf=praxi-xvfb.service` ile paylaşmalıdır. Doctor yoklaması aynı ad alanına ve dilime
  katılsın diye doğrudan `bot doctor` yerine her zaman `sudo praxi doctor` kullan.
- **D-Bus / AT-SPI2 eksik (`org.a11y.Bus` ya da `xfconf` hataları):**
  `DBUS_SESSION_BUS_ADDRESS=unix:path=/var/lib/praxi/.dbus-session` ayarının `/etc/praxi/praxi.env`
  içinde bulunduğunu ve kip `0600` soketin `/var/lib/praxi/.dbus-session` konumunda var olduğunu doğrula
  (`sudo systemctl status praxi-desktop.service`).
- **XTEST / RANDR eksik:** `praxi-xvfb.service` biriminin `+extension RANDR +extension XTEST` ile
  çalıştığını ve `xdpyinfo -display :99` komutunun (servis ad alanının içinde) iki eklentiyi de
  listelediğini doğrula.

## Güncelleme, günlükler, geri alma ve kaldırma

```sh
sudo ./scripts/install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz   # güncelle (var olan masaüstü kipini korur)
sudo ./scripts/install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz --without-desktop-session  # Xfce'yi kaldır, Xvfb'yi tut
sudo ./scripts/install-linux.sh --version v0.1.0 --file ./praxi-linux-aarch64.tar.gz --without-desktop          # Xfce ve Xvfb'yi kaldır
journalctl -u praxi -u praxi-desktop -u praxi-xvfb -n 100
sudo ./scripts/install-linux.sh --uninstall           # veriyi ve kullanıcıyı korur
sudo ./scripts/install-linux.sh --uninstall --purge   # veriyi de siler
```

### Var olan bir sunucunun ekran boyutunu değiştirme

Bir yansıma, sunucunun kendi ekranından asla daha net olamaz: akış ekranı kendi boyutunda gönderir
(en fazla 1920x1080) ve asla büyütmez. 1920x1080 varsayılanından önce kurulan sunucular hâlâ daha küçük
bir Xvfb çalıştırıyor olabilir (örneğin `1280x800x24`). Kurulum programı yükseltmede var olan geometriyi
korur, bu yüzden değiştirmek için `--screen` değerini açıkça ver:

```sh
sudo bash ./install-linux.sh --version <current> --file ./praxi-linux-x86_64.tar.gz   --screen 1920x1080x24
```

ya da `/etc/systemd/system/praxi-xvfb.service` içindeki `-screen 0 <WxHxD>` değerini düzenle, sonra
`sudo systemctl daemon-reload && sudo systemctl restart praxi-xvfb.service` çalıştır
(masaüstü oturumu ve daemon onunla birlikte yeniden başlar, yani açık oturumlar biter). Sonucu
`systemctl cat praxi-xvfb.service | grep -- -screen` ile denetle.

### Sunucuyu işletme

Günlükler, bilinçli başlatma ve durdurma, `bot host upgrade` ve `bot host repair`, geri alma,
`/var/lib/praxi` yedeği ve sorun tablosu [Sunucu işletimi](host-operations.md) sayfasında. Yukarıdaki
kurulum programı komutları, o sayfanın işaret ettiği elle güncelleme yoludur.

**Sunucu kipi etkin mi?** Sağlama denetimleri (`bot host add` / `repair` ön denetimi ve durumu), bir
systemd drop-in'inden gelen sunucu kipi `ExecStart` satırı dahil, etkin birimi okur. Elle:
`systemctl cat praxi.service | grep ExecStart`.

İmzalı sürüm arşivi olmayan, kaynaktan derlenmiş sunucularda `scripts/linux/setup-headless.sh`,
aynı sistem birimlerini sağlar ya da geri alır:

```sh
sudo bash scripts/linux/setup-headless.sh --system --display :99 --screen 1280x800x24
sudo bash scripts/linux/setup-headless.sh --system --without-desktop-session
sudo bash scripts/linux/setup-headless.sh --system --without-desktop
```

`:99` ile hiç çakışmayan ayrı bir ekranda kullanıcı düzeyi geliştirme oturumları için
`bash scripts/linux/setup-headless.sh --user --display :98` çalıştır.
