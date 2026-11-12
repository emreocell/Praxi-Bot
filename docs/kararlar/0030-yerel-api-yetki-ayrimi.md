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
| Eşleşmiş peer | iroh taşımasının doğruladığı anahtar/rol | Her route için Phone/Desktop kapsamı; host yönetimi varsayılan kapalı; yerel HTTP başlığı peer olamaz |
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
Route kayıtları aynı yerde yerel seviye + Phone + Desktop kapsamını zorunlu
olarak taşır. Yeni metot üç kapsam olmadan derlenmez; kayıt/OpenAPI testi
yeni yolları ve peer scope metadata'sını da denetler. HEAD, GET sınıfını izler.

### Yol sınıfları

| Sınıf | Yollar |
|---|---|
| Public | GET health; POST webhooks/{id} yalnız kendi HMAC doğrulamasıyla |
| CLI | OpenAPI; bot/grant/chat/message/run/usage/policy/provider/schedule/channel/MCP katalog ve araç listelerinin GET'leri; event akışı/cursor/status; chat oluşturma ve mesaj gönderme; run cancel/resume |
| Yönetim | Bot/grant mutasyonları; approval ve question kararları; run remember; politika yazma; provider detect/reload; tüm control ve gelecekteki view yolları; schedule oluştur/değiştir/sil/run-now; MCP oluştur/değiştir/sil/reconnect; email yolları; secret yazma; Telegram pair-code; gelecekteki pairing/devices/host yönetimi |

Onay listesi düşük CLI kimliğine kapalıdır. `GET /v1/approvals/count` yalnız
bekleyen sayısını verir; `bot status` bunu kullanır. Email hesap listeleme kişisel
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
mesajı gerekir. Başka PID zorla öldürülmez. Headless host'ta açıkça yetkilendirilmiş paired Desktop
yolu kullanılmalıdır (operatör tarafından verilen anahtar allowlist'i). `bot approve`, bot/schedule/MCP yönetimi gibi HTTP
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

## İkinci kilometre taşı: uyumlu süreç koruması (tam sandbox değil)

Windows'ta rastgele bir komutu restricted token/AppContainer ile, bütün kullanıcı
çalışma dizinlerine yazma ve CLI login dizinlerini kullanma davranışını koruyarak
çalıştırdığımız iddia edilmiyor. Bunun yerine credential tutan **daemon ve
masaüstü**, sır üretilmeden/okunmadan önce kendi process object DACL'ını daraltır.
`OWNER RIGHTS` ACE, aynı kullanıcının örtük `WRITE_DAC` hakkıyla DACL'ı geri
almasını engeller. Owner yalnız query-limited, synchronize, terminate ve
READ_CONTROL alır; SYSTEM tam erişimi korur. Bellek okuma/yazma, handle çoğaltma,
uzak thread/parent-process oluşturma ve DACL/owner değiştirme için yeni process
handle açılması reddedilir. Mevcut trusted ebeveynin sidecar handle'ı geçerlidir.
Sentez testte çocuk, koruma öncesi okuyabildiği parent process'e koruma sonrası
VM_READ / DUP_HANDLE / WRITE_DAC ile erişemez; query-limited çalışır.

Bu **restricted-token veya low-integrity çocuk izolasyonu değildir**. Önceden
alınmış handle'lar iptal edilmez. Thread object DACL'ları bu katmanda
korunmaz; thread-context/APC/code-injection saldırıları, DLL/modül arama yolu
ve UI otomasyonu halen değerlendirilmelidir. SeDebugPrivilege/yönetici ve
SYSTEM kapsam dışıdır. Aynı kullanıcının disk dosyalarını değiştirmesi sürer.
Bu nedenle tam hedefin OS sandbox bölümü açık kalır.

Linux'ta daemon ve masaüstü `PR_SET_DUMPABLE=0` uygular. Böylece sıradan aynı
UID'nin ptrace/process_vm_readv ve erişim kontrollü /proc bellek/ortam yolları
engellenir (CAP_SYS_PTRACE/root hariç). Daemon, worker thread'leri oluşmadan
`no_new_privs` ayarlar; shell, embedded script'in çağırdığı shell, MCP stdio ve
CLI-provider çocukları bunu miras alır. setuid/file capability ile yükseltme
olmaz. Mevcut process group/job yaşam döngüsü korunur. bwrap/Landlock ve ayrı
UID henüz uygulanmadı; hidepid aynı UID için güvenlik sınırı değildir.

`isolation.toml` policy dosyasında `memory_guard = false` veya
`linux_no_new_privs = false` uyumluluk için açıkça seçilebilir. Varsayılanlar
true; bozuk/bilinmeyen alan startup'ı durdurur. `no_new_privs` sudo/setuid
araçlarını ve bazı eski Chromium sandbox kurulumlarını bozabilir. Windows
memory guard, debugger/crash-dump erişimini kısıtlar. Kapatılan memory guard
başlangıçta açık uyarı verir; çalışan sürecin flag'i araç parametresi veya
ortam değişkeniyle değiştirilemez. Aynı kullanıcının bu policy dosyasını
sonraki restart için değiştirebilmesi ayrı disk bütünlüğü riskidir.

Windows native subprocess testi çalıştırıldı. Linux için cross-target check
çalıştırılır; runtime, paketlenmiş Tauri/WebView2 ve gerçek CLI sağlayıcıları
ile uyumluluk doğrulaması PR birleşmeden önce gereklidir. Bu sonuçlar bir
sandbox doğrulaması diye sunulamaz.

## Üçüncü kilometre taşı: dosya araçları ve tanılama

PR #30'un `under_data_sensitive` / `daemon_state_name` uygulaması ve testleri
bu dala aynı yapıda geri taşınır; ikinci bir credential-name tablosu kurulmaz.
Tek ek korunan ad `isolation.toml` olur. Kontrol/politika, bot/memory/schedule
SQLite dosyaları ve yan dosyaları, provider/email/MCP/web/host yapılandırması,
peer anahtarları, vault/secrets ve daemon kayıt dizinleri aynı çözümleyiciden
geçer. Windows trailing dot/space ve ADS adları kanonikleştirilir. Aynı adlı
normal proje dosyaları veri dizini dışında engellenmez. Mevcut açık/exact
credential grant davranışı ve hard-link sınırı korunur; shell için sandbox
olduğu iddia edilmez.

Kimlik doğrulamalı düşük yetkili `GET /v1/isolation`, sır içermeyen gözlemleri
döndürür: privilege split, embedder'ın doğruladığı child-env wiring'i,
süreç memory guard'ı, no-new-privs ve Praxi-managed OS sandbox durumu.
Embedder child-env durumunu açıkça bildirmedikçe true sayılmaz. Tam child
sandbox henüz uygulanmadığından `os_sandbox` false kalır. `bot doctor`
sürüme veya token dosyasının varlığına bakıp izolasyon var demez; çalışan
daemon'dan bu raporu alır. Eski/erişilemeyen daemon veya eksik alanlar
“unverified” olarak gösterilir; split kapalıysa fail, yalnız kısmi koruma
varsa sınırı belirten warn üretilir.

Native handle bayrakları ve process DACL API'leri tek küçük `process-guard`
Windows modülünde toplanır. Bootstrap inheritance testi aynı içerikle oraya
taşınır; başka crate'te yeni unsafe kod tutulmaz.

## Kart ekleri: peer kapsamları ve karar olayları

39a4683 yalnız yerel bearer seviyelerini ayırıyordu; eski `peer_router` geniş
rol iznini daraltmıyordu. Bu boşluk kapatılır: her HTTP metodu için aynı
registry, Phone ve Desktop kapsamını ayrıca zorunlu tutar. Bilinmeyen yol,
bilinmeyen metot, Node ve Host API rolü her zaman deny olur. Host yapılandırma
mutasyonları (provider reload/CLI enable, policy, bot/grant, MCP, email, schedule,
secret ve pairing) Phone için deny; Desktop için yalnız operatörün açıkça
allowlist'e koyduğu **eşleşmiş** anahtar ile `HostManagement` olur. Rol tek
başına yetmez; peer'in bildirdiği capabilities veya HTTP başlığı bu hakkı vermez.
Böylece normal bir paired Desktop'ın varsayılan yetkisi de host yönetimi değildir.

Phone/Desktop sohbet, durum ve tek seferlik onay/question akışını kendi taşıma
kimliğiyle sürdürebilir. Kalıcı `always`/`always_category` grant oluşturmak ayrıca
host yönetimi ister. `POST /v1/control/{request_id}` ise allowlist'teki remote
manager'a bile kapalıdır: yerel overlay proceed/refuse yalnız LocalManagement
aktörünündür; handler içinde de ikinci kontrol bulunur.

SSE'de `control_requested` yalnız LocalManagement'a gider. Approval-needed,
approved/denied ve question olayları düşük CLI kimliğine gitmez; paired karar
kimlikleri kendi izinli onay akışı için bunları alır. Filtre aynı noktada hem
canlı yayın hem global/run replay için uygulanır; yeni Event varyantı eklenince
exhaustive match görünürlük kararı verilmeden derlenmez. Düşük veya peer global
akışı yerel overlay uygulaması bağlıymış gibi subscriber kaydı oluşturamaz.

**UI teknik ayrıntısı:** Yerleşik native `EventSource` başlık ekleyemez, fakat
Praxi UI bu API'yi kullanmıyor: `subscribeEvents` fetch + ReadableStream ile
Authorization ve Last-Event-ID başlıklarını her bağlantıda gönderir. UI yüksek
sırrı URL'ye koymaz. Geriye uyum için yalnız events yolundaki düşük disk-token
query istisnası kalır; bu akış filtrelidir ve yüksek token query'de reddedilir.
Diğer yollarda query token kimlik doğrulaması değildir.
