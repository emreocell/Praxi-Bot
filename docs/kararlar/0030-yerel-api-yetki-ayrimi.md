# ADR-0030 — Yerel API kimlikleri ve bot alt süreçlerinin sınırı

- **Durum:** Uygulanıyor; platform izolasyonu tamamlanmış değildir
- **Tarih:** 2026-11-12
- **İlgili kararlar:** ADR-0012, ADR-0013, ADR-0027, ADR-0029; görev 192

## Bağlam

`<veri>/api-token` aynı OS kullanıcısının dosyasıdır. Botun onaylanmış tek
`shell_run` çağrısı bu dosyayı okuyabiliyordu. Eski bearer kontrolü bu kimliği
onay/politika/kontrol yetkilisi kabul ediyordu. Komut filtreleri ve dosya adı
listeleri keyfi program çalıştırmaya karşı güvenlik sınırı değildir.

Bu dalın tabanında PR #30'un Actor ve canlı görüntü yolları henüz yoktur.
Yeni yolların kendiliğinden düşük yetki kazanması engellenmelidir. Eşleşme
yönetiminin bir bölümü HTTP yerine CLI üzerinden doğrudan dosyaya yazar;
HTTP yetki ayrımı bu dosyaların bütünlüğünü tek başına korumaz.

### Tehdit modeli ve kimlikler

Saldırgan model çıktısını ve onaylanan shell komutunu seçebilir; daemon'un
çocuğunda aynı kullanıcı olarak keyfi kod çalıştırır. Yönetici/root, kernel,
çalınmış gerçek eş anahtarı ve ele geçirilmiş UI bu ilk düzeltmenin dışında.

| Kimlik | Kaynak | Yetki |
|---|---|---|
| Masaüstü UI | Her sidecar açılışında rastgele 256 bit, yalnız bellek ve anonim stdin borusu | Yönetim, onay, kontrol |
| Terminalde kullanıcının çalıştırdığı CLI | Kalıcı `api-token` | Durum/liste/sohbet; OS aynı kullanıcıyı bot çocuğundan ayıramaz |
| Bot shell/MCP/CLI engine çocuğu | Ortamı temizlenmiş süreç; disk token'ını okuyabilir | En çok CLI yetkisi; token dosyasını okumayı engellediğimiz iddia edilmez |
| Eşleşmiş peer | iroh taşımasının doğruladığı anahtar/rol | Mevcut peer rol kontrolleri; yerel HTTP başlığı peer olamaz |
| Telegram | Daemon içindeki bağlayıcı ve kendi eşleşmiş Telegram kullanıcısı | Mevcut kanal kararı; disk token'ına yönetim yetkisi verilmez |

### OS gerçekte neyi engeller?

**Windows:** Aynı SID, varsayılan dosya DACL'ları altında aynı dosyaları
okuyup değiştirebilir. Loopback portuna her yerel süreç bağlanabilir.
`OpenProcess` istenen hakları süreç DACL'ına göre denetler;
`PROCESS_VM_READ` bellek okumaya, `PROCESS_DUP_HANDLE` handle çoğaltmaya
yarayabilir. Aynı kullanıcı sırf farklı PID olduğu için güvenilmez sayılmaz.
Miras alınabilir handle'lar CreateProcess sırasında sınırlandırılmalıdır.
Rust std Command varsayılanında başka inheritable handle'lar da geçebilir;
bu nedenle tüketilmiş bootstrap stdin handle'ının HANDLE_FLAG_INHERIT biti
açıkça temizlenir (hata startup'ı durdurur). Unix tarafında stdin CLOEXEC
yapılır. Araç çocukları kendi standart I/O kanallarını alır.
Job object süreç ağacının yaşam döngüsünü yönetir, bellek gizliliği sağlamaz.
Low integrity tek başına genel bir no-read-up garantisi değildir; restricted
token/AppContainer ve uygun süreç/dosya DACL'ları birlikte değerlendirilmelidir.

**Linux:** Aynı UID, 0600 dosyayı okuyabilir ve yazabilir. `/proc/PID/environ`,
`/proc/PID/mem`, `process_vm_readv` ve ptrace erişimi dumpable, Yama/LSM,
yetenekler ve ptrace erişim türüne bağlıdır. Child→parent ptrace bazı
kurulumlarda Yama tarafından reddedilir; tüm Linux'larda varsayılmaz.
`CLOEXEC` tanıtıcı mirasını keser; standart I/O ayrıca seçilir. Süreç grubu
öldürme/iptal sınırıdır, izolasyon değildir. `no_new_privs` setuid/capability
yükseltmesini engeller, aynı UID'nin dosya veya süreç belleği erişimini
kendiliğinden kaldırmaz. hidepid, başka UID'leri gizleyen kurulumlarda aynı UID
için çözüm değildir. Ayrı UID veya user/mount namespace + sandbox gerekir.

## Seçenekler

1. **Ayrı UI kimliği:** Küçük ve geriye uyarlanabilir. Disk token'ı yönetim
   yetkisini kaybeder; UI sırrı argv/ortam/dosya/log içinde bulunmaz. Bellek
   okuma, kod enjeksiyonu ve disk bütünlüğü saldırıları tek başına çözülmez.
2. **OS izolasyonu:** Windows restricted token/AppContainer, Linux ayrı UID
   veya bwrap/Landlock. Dosya, süreç, ağ ve masaüstü kaynakları ayrı
   tasarlanmalıdır. Normal araçların çalışma dizini, git/SSH, paket yöneticisi,
   MCP secret_env ve CLI sağlayıcısının login dizini ihtiyaçları bozulabilir.
   Job/process group yararlıdır fakat tam sandbox adıyla sunulamaz.
3. **Kullanıcı varlığı:** OS tarafından doğrulanan her onay için imza/anahtar.
   Güçlü bir sonraki katman olabilir; sıradan bir Tauri IPC çağrısı veya
   aynı kullanıcının otomasyonla tıklayabildiği pencere yeterli kanıt değildir.
   Windows Hello ve Linux polkit/ayrı servis entegrasyonu, UX ve kurtarma
   maliyeti vardır. Kapsamsız genel bir terminal yönetim token'ı üretilemez.

## Karar

Önce (1), sonra ölçülüp açık sınırlara sahip (2). (3) bu değişikliğin dışında.
Kalıcı bearer **yönetim kimliği değildir**. Yanlış/eksik yüksek kimlik
yetkiyi artırmaz. Bilinmeyen route/metot varsayılan olarak reddedilir.
Route kayıtları aynı yerde açık yetki sınıfı taşır; OpenAPI kapsam testi yeni
bir yolun sınıflandırılmadan eklenmesini yakalar. HEAD, GET sınıfını izler.

### Yol sınıfları

| Sınıf | Yollar |
|---|---|
| Public | GET health; POST webhooks/{id} yalnız kendi HMAC doğrulamasıyla |
| CLI | OpenAPI; bot/grant/chat/message/run/usage/policy/provider/schedule/channel/MCP katalog ve araç listelerinin GET'leri; event akışı/cursor/status; chat oluşturma ve mesaj gönderme; run cancel/resume |
| Yönetim | Bot/grant mutasyonları; approval ve question kararları; run remember; politika yazma; provider detect/reload; tüm control ve gelecekteki view yolları; schedule oluştur/değiştir/sil/run-now; MCP oluştur/değiştir/sil/reconnect; email yolları; secret yazma; Telegram pair-code; gelecekteki pairing/devices/host yönetimi |

Onay listesi salt okunur CLI durum komutuna açıktır; karar verme yetkisi vermez. Email hesap listeleme kişisel
sunucu/hesap yapılandırması içerdiği için yönetim sınıfındadır. Sohbet
başlatmak araç onaylarını aşmaz; mevcut permission-engine uygulanır. CLI
kimliği özel sohbetleri okuyabilir, koşu başlatabilir ve durdurabilir: bu
tasarım gizlilik veya hizmet sürekliliği izolasyonu değildir.

Masaüstü sidecar'ı her başlattığında yeni sırrı anonim stdin borusundan
verir. Daemon boruyu araçlar başlamadan tüketir. Yüksek sır yalnız HTTP
Authorization başlığında kabul edilir, events URL parametresinde kabul
edilmez. Ortam değişkeni/komut satırıyla yüksek sır almak desteklenmez.
Daemon'un redactor'ı yüksek sırrı da maskeler. UI bellekten mevcut fetch
başlığını kullanır; diske yazılan CLI token'ına geri düşüp yetki kazanamaz.

### Geçiş

Eski kurulumun veri dosyaları ve CLI token'ı korunur; masaüstünün başlattığı
yeni daemon otomatik yeni UI kimliği alır. Kullanıcıdan yeniden eşleşme
istenmez. Masaüstünün başlatmadığı zaten çalışan daemon'a yalnız düşük
yetkiyle bağlanılabilir; yönetim için onu kapatıp masaüstünden başlatma
mesajı gerekir. Başka PID zorla öldürülmez. Headless host'ta eşleşmiş peer
yolu kullanılmalıdır. `bot approve`, bot/schedule/MCP yönetimi gibi HTTP
komutları yerelde desktop-only olur; 403 açık geçiş mesajı verir. Diskte
doğrudan ayar değiştiren CLI komutları OS kullanıcısının gücünü taşımaya
devam eder; OS sandbox olmadan bot çocuğunun bunları çağırması önlenemez.

## Sonuçlar

- Token dosyasının okunması artık HTTP üzerinden kendine onay, politika
  değişikliği veya kontrol yetkisi vermez.
- UI ve daemon yeniden başlayınca önceki yüksek sır geçersizdir.
- Eşleşmiş peer ve Telegram kimlikleri düşük disk kimliğine indirgenmez.
- Başsız ve haricen başlatılmış daemon'un yerel yönetim UX'i daha kısıtlıdır.
- Ek izolasyon, dosya korumaları ve doctor çıktısı ayrı kilometre taşlarıdır;
  doğrulanmayan platformlar raporda açıkça belirtilir.

## Riskler ve açık sınırlar

Bu ilk katman **aynı kullanıcıya karşı tam güvenlik sınırı değildir**.
İzolasyonsuz çocuk daemon/UI belleğini okuyabilir, handle çoğaltabilir,
UI otomasyonu yapabilir, yapılandırma/SQLite/policy/eş anahtarlarını
değiştirebilir, program veya çalışma dizininden DLL/modül enjekte edebilir.
Veri dizinindeki devices.json, identity.key/peer-node, vault/secrets,
bot.db (WAL/SHM dahil), policy.json, control-latch.json, email.toml,
providers.toml, web.toml ve MCP token'ları ayrıca hassastır. Dosya araçları
koruması doğrudan shell veya aynı SID/UID ile çalışan CLI'yi sınırlamaz.
OS keychain de aynı kullanıcıya her zaman uygulama kimliği izolasyonu vermez.
Swap, hibernation, crash dump ve debug ayrıcalıklı kullanıcı bu bellekte sır
tasarımının dışında kalır. Yüksek sır ortamda veya diskte tutulmasa bile
bu riskler bitmiş sayılmaz.

### Birincil kaynaklar

- [Windows süreç erişim hakları](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights)
- [Restricted tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens)
- [Mandatory Integrity Control](https://learn.microsoft.com/en-us/windows/win32/secauthz/mandatory-integrity-control)
- [Linux ptrace erişim denetimi](https://man7.org/linux/man-pages/man2/ptrace.2.html)
- [Linux no_new_privs](https://docs.kernel.org/userspace-api/no_new_privs.html)
