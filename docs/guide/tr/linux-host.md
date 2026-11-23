# Linux sunucuda 7/24 çalışma (sanal ekran, güncelleme, kaldırma)

Bu sayfa, Praxi'yi gece gündüz açık duran bir Linux sunucuda (örneğin ücretsiz ARM Ubuntu VM) çalıştırmak için kopyala-yapıştır komutlarını verir. [Sunucu ekle sihirbazı](../servers.md) aynı kurulum betiğini çalıştırır ve aynı mesajları gösterir; ayrıntılı anlatım için [Uzak sunucuda çalıştırma](../remote-host.md) sayfasına bak.

Tüm komutları sunucuda, SSH ile çalıştır. `v0.1.0` yerine istediğin sürümü yaz.

## 1. Önce önizle, sonra kur

Önce her zaman önizle. `--dry-run` hiçbir şeyi değiştirmez, `sudo` da gerekmez:

```bash
bash ./install-linux.sh --dry-run --headless --version v0.1.0
```

Kurulum (önce betiği [remote-host.md](../remote-host.md) sayfasındaki gibi doğrula):

```bash
sudo bash ./install-linux.sh --headless --version v0.1.0
```

`--headless` şunu yapar: 7/24 sunucu modu (`bot serve --host`, yalnızca `127.0.0.1` dinler, açık port yok) ve **sanal ekran** (Xvfb) ile Chromium tarayıcı; böylece kimse oturum açmamışken botlar internette gezebilir ve masaüstü kullanabilir. Ekran `:99` üzerinde `1920x1080x24` olur. `--screen` ile değiştir:

```bash
sudo bash ./install-linux.sh --headless --screen 1280x720x24 --version v0.1.0
```

## 2. Neler kurulur

| Parça | Ayrıntı |
| --- | --- |
| `praxi.service` | Çökerse yeniden başlar (5 saniye sonra, 10 dakikada en fazla 10 kez); yeni yetki alamaz, sistem salt okunur, geçici klasörü ayrıdır. |
| `praxi-xvfb.service` | Sanal ekran; ağa kapalı, özel erişim anahtarlı. |
| Günlükler | Sistem günlüğüne gider; günlük 500 MB ve bir ayla sınırlıdır, disk dolmaz. Okumak için `journalctl -u praxi -f`. |
| Veriler | `/var/lib/praxi` (botların, dosyaların, gizli bilgilerin). Ayarlar `/etc/praxi` içinde. |

## 3. Her şeyi denetle

```bash
sudo praxi doctor
```

Doctor ekranı, tarayıcıyı, boş diski, boş belleği, saati ve portu denetler; her sorunun yanında çalıştırılacak tam komut yazar. En sık görülenler:

| Doctor diyor ki | Çalıştır |
| --- | --- |
| ekran yanıt vermiyor | `sudo systemctl restart praxi-xvfb` sonra `sudo systemctl status praxi-xvfb` |
| tarayıcı bulunamadı | `sudo bash ./install-linux.sh --update --with-desktop` |
| disk az | `sudo journalctl --vacuum-size=200M` ve `sudo apt-get clean` |
| saat yanlış | `sudo timedatectl set-ntp true` |
| port kullanımda | `sudo ss -ltnp 'sport = :47821'` kimin kullandığını gösterir; başka port için `--port` kullan |

## 4. Güncelle (verilerin korunur)

```bash
bash ./install-linux.sh --dry-run --update --version v0.1.1
sudo bash ./install-linux.sh --update --version v0.1.1
```

`--update` yalnızca programı değiştirir. Botların, dosyaların, gizli bilgilerin, ekran boyutu ve sunucu modu olduğu gibi kalır. Praxi henüz kurulu değilse yeni bir şey kurmaz, açık bir mesajla durur.

## 5. Kaldır

```bash
sudo bash ./install-linux.sh --uninstall          # Praxi'yi kaldırır, VERİLERİNİ KORUR
sudo bash ./install-linux.sh --uninstall --purge  # tüm verileri de siler. Geri alınamaz.
```

İkisini de önce `--dry-run` ile önizleyebilirsin.

## 6. Günlükler

```bash
sudo journalctl -u praxi --since "1 hour ago"
sudo journalctl --disk-usage
```
