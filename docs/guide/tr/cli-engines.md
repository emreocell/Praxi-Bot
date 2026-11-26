# Kodlama CLI motorları

Ayarlar bölümündeki sağlayıcılar sayfası, **daemon'ın çalıştığı bilgisayardaki** CLI motorlarını listeler. **Yeniden tara** düğmesi, sınırlı sürüm ve kimlik doğrulama yoklamalarını yeniden çalıştırır. Algılama daemon başlarken de önceden ısıtılır. Terminalin PATH girdilerine kullanıcıya özel, npm, Node sürüm yöneticisi ve Windows'a özgü kurulum klasörleri de eklenir. Göreli PATH girdileri, çalışma klasörü ve oraya geri dönen bağlantılar reddedilir. Çözülen shim programları ve betikleri de aynı sınıra göre bir kez daha denetlenir. PATH üzerindeki ilgisiz bir `agent`, Cursor'ın satıcı klasöründeki takma adını gizleyemez.

| Motor | Komutlar (Windows'ta PATHEXT kullanılır) | Kimlik doğrulama kanıtı | Kurulum komutu (belirtilmedikçe her işletim sistemi için) | Beyin |
| --- | --- | --- | --- | --- |
| Claude Code | `claude` | `auth status --json` | `npm install -g @anthropic-ai/claude-code` (yerel kaynakla doğrulandı) | Evet, en az 2.1.257 ve çalışma zamanı yetenek denetimleri |
| Codex | `codex` | `login status` | `npm install -g @openai/codex` (yerel kaynakla doğrulandı) | Engelli: `unified_exec` kapatılamıyor |
| Gemini CLI | `gemini` | Yalnızca kimlik bilgisi dosyasının üst verisi, içeriği hiç açılmaz | `npm install -g @google/gemini-cli` (**doğrulanmadı**, yalnızca kopyalamak için) | Yalnızca algılama; en düşük sürüm doğrulanmadı |
| Antigravity | `agy` | Yok: durum komutu yok (giriş durumu ilk turda görünür) | bilinen yok | Kendi araçlarıyla çalışır (`agy acp`); modeller `agy models` ile gelir |
| Qwen Code | `qwen` | Yalnızca kimlik bilgisi dosyasının üst verisi (`~/.qwen/oauth_creds.json`), içeriği hiç açılmaz | `npm install -g @qwen-code/qwen-code` (kaynağıyla 2026-11-26'da doğrulandı; Kur düğmesi onayından sonra çalıştırır) | Kendi araçlarıyla çalışır (`qwen --acp`, deneysel); en düşük sürüm doğrulanmadı |
| Kiro CLI | `kiro-cli` | Yok: `whoami` çıkış kodları belgelenmemiş; `KIRO_API_KEY` kanıt sayılmaz | Unix: `curl -fsSL https://cli.kiro.dev/install \| bash` (kaynağıyla 2026-11-26'da doğrulandı; Kur düğmesi onayından sonra çalıştırır) | Kendi araçlarıyla çalışır (`kiro-cli acp --agent-engine v3`, deneysel); en düşük sürüm doğrulanmadı |
| Qoder CLI | `qodercli` | `status -o json` (`logged_in`) | `npm install -g @qoder-ai/qodercli` (kaynağıyla 2026-11-26'da doğrulandı; Kur düğmesi onayından sonra çalıştırır) | Kendi araçlarıyla çalışır (`qodercli --acp`, deneysel); en düşük sürüm doğrulanmadı |
| GitHub Copilot CLI | `copilot` | Yok: durum komutu yok (giriş `copilot login` ile yapılır ve işletim sisteminin kimlik bilgisi deposunda durur; ortamdaki bir GitHub token'ı kanıt sayılmaz) | `npm install -g @github/copilot` (kaynağıyla 2026-11-26'da doğrulandı; Kur düğmesi onayından sonra çalıştırır) | Kendi araçlarıyla çalışır (`copilot --acp`, deneysel); en düşük sürüm doğrulanmadı |
| Kimi Code CLI | `kimi` | Yalnızca kimlik bilgisi dosyasının üst verisi (`~/.kimi-code/credentials/kimi-code.json`), içeriği hiç açılmaz | `npm install -g @moonshot-ai/kimi-code` (kaynağıyla 2026-11-26'da doğrulandı; Kur düğmesi onayından sonra çalıştırır) | Kendi araçlarıyla çalışır (`kimi acp`, deneysel); en düşük sürüm doğrulanmadı. Python ile yazılmış `kimi-cli` 1.x, bunun emekli olmuş öncülüdür |
| Mistral Vibe | `vibe-acp` | Yok: durum komutu yok; API anahtarı ortamda, `~/.vibe/.env` içinde, işletim sisteminin anahtarlığında ya da tarayıcıyla girişte olabilir ve hiçbiri kanıt sayılmaz | `uv tool install mistral-vibe` (**doğrulanmadı**, yalnızca kopyalamak için) | Kendi araçlarıyla çalışır (`vibe-acp`, deneysel); en düşük sürüm doğrulanmadı. CLI'nin kendi modelini çalıştırır: Vibe, ACP üzerinden seçilen modeli kullanıcının varsayılanı olarak kaydeder, bu yüzden başka bir model reddedilir |
| Goose | `goose` (aynı adı taşıyan, `goose version: ...` yazdıran veritabanı geçiş aracı başka bir program olarak bildirilir) | Yok: durum komutu yok; anahtarlar işletim sisteminin anahtarlığında ya da `secrets.yaml` içinde durur (`goose configure` ile ayarlanır) ve hiçbiri kanıt sayılmaz | Unix: `curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh \| CONFIGURE=false bash` (**doğrulanmadı**, yalnızca kopyalamak için) | Kendi araçlarıyla çalışır (`goose acp`, deneysel); en düşük sürüm doğrulanmadı. Yapılandırılmış bir sağlayıcı yoksa çalışma, oturum kapalıymış gibi biter |
| OpenCode | `opencode` | Yalnızca kimlik bilgisi dosyasının üst verisi, içeriği hiç açılmaz | `npm install -g opencode-ai` (yerel kaynakla doğrulandı) | Yalnızca algılama ve kurulum: incelenen CLI'da yerleşik araçları kapatacak bir seçenek yok |
| Cursor | `cursor-agent`; `agent` yalnızca bir `cursor-agent` klasörünün içindeyse | `status --format json`, sonra `status` | Windows: `irm 'https://cursor.com/install?win32=true' \| iex`; Unix: `curl https://cursor.com/install -fsS \| bash` (yerel kaynakla doğrulandı) | Yalnızca algılama; en düşük sürüm doğrulanmadı |
| ChatGPT planı | Aynı Codex çalıştırılabilir dosyası | Bilinmiyor; Praxi'nin PKCE akışı yok | Codex satırını kullan | Yalnızca algılama |

“Doğrulandı”, komutun yerel bir kaynakta bulunduğu anlamına gelir. Bu özellik geliştirilirken hiçbir kurucu çalıştırılmadı ve gerçek bir model istemi gönderilmedi. En düşük sürümler, yukarıda yazılmadıkça bilinmiyor. Kimlik bilgisi dosyasının olmaması, bir motorun işletim sisteminin anahtarlığını kullanamayacağını kanıtlamaz. Arayüz bu yüzden durumu “bilinmiyor” olarak ayrı gösterir. Ortamdaki API anahtarları motora verilmez ve kullanılabilir bir CLI girişinin kanıtı sayılmaz.

**Kur**, sabit komutun kendisini ve süresi dolan bir **Bir kez izin ver / Reddet** kartını gösterir. Komut ancak kimliği doğrulanmış yerel sahip onayladıktan sonra çalışır. Kalıcı bir izin, bot politikası, araç çağrısı, zamanlama ya da eşleştirilmiş cihazın rolü bunu onaylayamaz. Yönlendirmeden önce, kodlanmış ve eğik çizgisi düzeltilmiş biçimler dahil tüm eş rolleri reddedilir. Kurucu bir ajan aracı değildir. Bu yüzden yerel taşıyıcı kimlik bilgisi sahip yetkisi taşır ve gizli kalmalıdır.

`POST /v1/providers/cli/install` onayı hazırlar. Ayrı olan `POST /v1/providers/cli/install/{approval_id}/decision` onayı bir kez tüketir. Yanıt, `output` ve `done` SSE olaylarını akıtır. Kurucunun metni güvenilmez sayılır ve metin olarak gösterilir; hiçbir zaman Markdown, HTML ya da komut olarak işlenmez. Denetim kayıtları, süreç başlamadan önce yazılır. Aynı anda tek bir kurucu çalışır; en fazla on dakika sürer, çıktı sınırı 64 KiB'tır ve yetki yükseltmesi istenmez. Akış kapanırsa ya da daemon kapatılırsa, o kurucunun başlattığı tüm süreçler sonlandırılır; sürüm ve kimlik doğrulama algılaması sonra yenilenir. Kurulum, satıcının genel komutunu mevcut kullanıcı olarak çalıştırır. Sisteme ait npm önekleri izin hatası verebilir; Praxi yönetici hakkıyla yeniden denemez.

Windows'ta npm, `node.exe <canonical npm-cli.js>` olarak başlatılır; `cmd.exe`'ye hiçbir `.cmd` verilmez. Cursor, Windows'ta satıcının sabit PowerShell komutunu, Unix'te sabit kabuk komutunu kullanır. Bu dizeler kayıt defterinden gelir; isteğin ya da modelin hiçbir değeri araya eklenmez. Doğrulanmamış komutlar bu çalıştırıcıya ulaşamaz. Bazı npm JavaScript shim'leri normal motor algılamasında reddedilmeye devam eder. Bu girdiler, doğrulanmış bir sürüm ya da kullanılabilir bir beyin bildirmek yerine `shim_refused` bildirir.

Girişi ayrıca terminalinde, Ayarlar'da gösterilen komutla yap. Praxi senin yerine giriş yapmaz, token okumaz ya da kopyalamaz ve CLI kimlik bilgilerini API anahtarına çevirmez. Kota pencereleri bilinmiyor; mevcut kota hataları turları durdurmaya devam eder. Kullanmayı düşündüğün amaç için satıcının abonelik koşullarına bak.

Algılanan bir Claude motorunu etkinleştirdikten sonra, bir botun yürütücü model seçicisinde `cli:claude` seç, ardından `default`, `sonnet` ya da `opus` değerlerinden birini seç. Modele erişimi CLI hesabı belirler. CLI motorları, eskisi gibi sınıflandırıcı ve planlayıcı seçiminin dışında tutulur. Yeni bir Tauri çağrısı kullanılmaz.

## Güvenlik sınırı

Claude, `--tools ""`, sıkı MCP yapılandırması, yalıtılmış ayarlar ve çalışma zamanı araç denetimleriyle çalışmaya devam eder. Codex'in mevcut salt okunur korumalı alanı her yerleşik aracı kapatmaz ve engelli kalır. CLI yapılandırma klasörleri (kancalar ve MCP ayarları dahil), kimlik bilgisi dosyaları ve ana klasör geçersiz kılmaları dosya araçlarıyla korunur. Buna `.opencode`, `opencode.json`/`opencode.jsonc`, `OPENCODE_CONFIG_DIR` ve `OPENCODE_CONFIG` ile gösterilen dosya da dahildir. Alt süreçlerin ortamı, işletim sisteminin gerekli değişkenlerinden ve CLI ana klasör yollarından oluşan açık bir izin listesiyle kurulur. Praxi gizli bilgileri, sağlayıcı anahtarları, vekil sunucu kimlik bilgileri ve `NODE_OPTIONS` gibi koda müdahale eden değişkenler devralınmaz.

OpenCode bilerek bot beyni olarak kapalı tutulur. Kurulu OpenCode **v2.2.5**'in `--help`, `run --help`, `acp --help` ve `auth --help` çıktıları, giriş yapılmadan ve model istemi gönderilmeden incelendi. Başsız `run --format json`, `provider/model#variant` biçimli model, `--session` ve stdio `acp` destekleniyor görünüyor, ama yerleşik araçları kapatan doğrulanmış bir olanak sunulmuyor. Bunu açabileceğimiz, daha önce var olan bir yetkilendirilmiş kodlama aracı bütünleşmesi de yok.

`cli-provider/src/opencode.rs`, deneysel ACP istek oluşturmayı ve hata durumunda kapalı kalan bir çerçeve çözücüyü içerir; ikisi de süreç çalıştırmaya bağlı değildir. Tam model kimliklerini, oturumları, metin ile düşünce ayrımını ve tamamlanmayı korur; yerleşik araç olaylarını ve isteklerini reddeder. Kayıt **sentetiktir**, yerel protokol kaynaklarına dayanır ve canlı bir oturumun kaydı değildir. Claude `--help` çıktısı da istem göndermeden `sonnet`/`opus` takma adlarını, `--tools ""`, `--setting-sources` ve `--strict-mcp-config` seçeneklerini doğruladı.

OpenCode çalıştırmasını açmak için, kısıtlamanın satıcının kaynağından doğrulanması ve kurulu CLI için kaydedilmiş, kimlik bilgisi içermeyen uyumluluk örnekleri gerekir.

## Yerel kanıtlar ve kaynak belirtme

Uygulama özgün Rust/React koduyla yazıldı. Tasarım fikirleri [OpenMausBot](https://github.com/milind-soni/OpenMausBot) projesinden öğrenildi (yerel kopya, Apache-2.0 LICENSE incelendi; hiçbir kaynak kod kopyalanmadı):

- `docs/cli-onboarding.md`, `docs/custom-engines.md`, `docs/opencode-go.md`, `docs/cursor.md`, `docs/headless-cli.md`: ilk kurulum ve CLI/ACP anlamları.
- `server/env-path.ts`, `server/cli-probe.ts`, `server/harness/registry.ts`: arayüz için yol genişletme, güvenli yoklamalar ve anlık görüntüler.
- `server/engine-install.ts`, `server/cli-setup.ts`: sabit npm argümanları, onay, zaman aşımı, sınırlı çıktı ve yeniden algılama.
- `server/drivers/claude.ts`, `server/drivers/codex.ts`, `server/drivers/acp/cursor.ts`, `server/drivers/acp/opencode-go.ts`: kurulum ve giriş tanımları.
- `server/drivers/acp/core.ts`: session/new/load, set_config_option, session/prompt, session/update ve stopReason.

Praxi PR #97'nin `fix/c244a-mcp-start-and-tool-availability` dalındaki `crates/mcp-bridge/src/transport.rs` dosyası da incelendi: üst sürecin çalıştırılabilir dosyasını, alt sürecin ortamını temizlemeden önce çözer ve yalnızca açıkça belirtilen işletim sistemi değişkenlerini korur. CLI keşfi, oturum açma kabuğunun başlangıç betiklerini çalıştırmadan, belirli ve sabit arayüz kurulum yollarını ekler.

## Bir CLI'yı kendi araçlarıyla çalıştırma (motor kipi)

Her botun bir motor kipi vardır. Bunu yalnızca yerel sahip ayarlayabilir (bot ayarlarında **Motor**):

- **Praxi yönetir** (varsayılan): Praxi dosyaları ve komutları kendi araçları ve izin motoruyla yürütür; CLI'nin yerleşik araçları kapalıdır (bugün yalnızca Claude Code için).
- **Kendi araçlarıyla**: CLI, senin seçtiğin bir çalışma klasöründe kendi dosya ve komut araçlarını korur. Botun onay düzeyi CLI'nin kendi izin kipine eşlenir ve CLI'nin ilettiği her izin isteği normal bir Praxi onay kartına dönüşür. Praxi'nin kendi araçları (tarayıcı, uygulamalar, gizli bilgiler, hafıza) CLI'ye çalışma başına bir yerel MCP uç noktasıyla sunulur ve Praxi onaylarını korur.

Varsayılan düzey **Sor**'dur. **Tam erişim** için her bot başına ayrı bir onay gerekir. Bir botu kendi araçlarıyla çalışmaya geçirmek de onay ister; ikisi de denetim günlüğüne yazılır.

| CLI | Sor | Düzenlemeleri kabul et | Otomatik | Tam erişim |
| --- | --- | --- | --- | --- |
| Claude Code | `--permission-mode default` (yeni sürümlerde `manual`) | `acceptEdits` | `auto` | `bypassPermissions` |
| Codex | `on-request` + `workspace-write` | Sor gibi çalışır | + `auto_review` denetçisi | `never` + `danger-full-access` |
| Cursor (`acp`) | varsayılan | Sor gibi çalışır | `--auto-review` | `--force` |
| OpenCode (`acp`) | her istek sorar | Sor gibi çalışır | Sor gibi çalışır | istekler otomatik onaylanır |
| Gemini CLI (`--acp`, deneysel) | varsayılan | `--approval-mode auto_edit` | Sor gibi çalışır | `--yolo` |
| Antigravity (`agy acp`, deneysel) | `default` kipi | `accept-edits` kipi | `auto` kipi; Tam erişim: `--dangerously-skip-permissions` |
| Qwen Code (`--acp`, deneysel) | `--approval-mode default` | `--approval-mode auto-edit` | `--approval-mode auto` | `--approval-mode yolo` |
| Kiro CLI (`acp`, deneysel) | varsayılan izinler | Sor gibi çalışır | Sor gibi çalışır | istekler otomatik onaylanır |
| Qoder CLI (`--acp`, deneysel) | `default` kipi | `acceptEdits` kipi | `auto` kipi | `yolo` kipi |
| GitHub Copilot CLI (`--acp`, deneysel) | varsayılan | `--allow-tool write` | Sor gibi çalışır | istekler otomatik onaylanır (`--allow-all-tools` hiçbir zaman kullanılmaz) |
| Kimi Code CLI (`kimi acp`, deneysel) | Windows: `default` kipi (Always Ask). Linux ve macOS: sunulmaz, çünkü bu kip bir git deposunun içindeki düzenlemeleri onaylar | Sor gibi çalışır | `yolo` kipi (Ask When Needed) | `auto` kipi (Never Ask) |
| Mistral Vibe (`vibe-acp`, deneysel) | `ask` kipi | `accept-edits` kipi | `smart-approve` kipi; oturum sunmuyorsa reddedilir (kademeli açılış kapısının arkasında) | `auto-approve` kipi; oturum sunmuyorsa reddedilir (`enable_auto_approve = false`) |
| Goose (`goose acp`, deneysel) | `approve` kipi | Sor gibi çalışır | `smart_approve` kipi | `auto` kipi |

Bir CLI'nin karşılığı olmayan düzey, hiçbir zaman daha geniş bir izinle değil, Sor gibi çalışır. Bir ACP CLI kiplerini `session/new` içinde listeliyorsa ve düzeyin kipi bunların arasında yoksa, istem gönderilmeden tur reddedilir. Çünkü bir CLI bilinmeyen bir kipi sessizce kabul edebilir. Çalışma klasörü bir sürücü kökü, senin ana klasörün, Praxi'nin veri klasörü ya da bir kimlik bilgisi veya CLI ayar klasörü olamaz. CLI, izin listesiyle sınırlanmış bir ortamda çalışır (Praxi gizli bilgisi ve sağlayıcı anahtarı yok). Durdur düğmesi ya da bota özel acil durdurma anahtarı, CLI'nin tüm süreç ağacını sonlandırır.

Goose, kullanıcının ayarları aksini söylemedikçe yeni bir oturumu `auto` kipinde ("Araç çağrılarını otomatik onayla") başlatır. Bu yüzden Praxi her zaman düzeyin kipini gönderir ve bu kip yalnızca o oturum için geçerlidir. Goose hakkında bilmen gereken üç şey var:

- **Otomatik kip bir modele sorar.** `smart_approve` kipinde, MCP sunucusunun salt okunur olarak işaretlediği çağrılar sormadan çalışır. Gerisine Goose'un kendi modeli karar verir (`permission_judge`); bu da ek model çağrılarına yol açabilir. Salt okunur olmadığına karar verdiği çağrılar için onay ister ve bu karar `permission.yaml` içinde saklanır.
- **Kayıtlı kurallar yine geçerlidir.** Kullanıcının `permission.yaml` içinde `always_allow` olarak işaretlediği bir araç, her kipte sormadan çalışır. Praxi izin isteklerine yalnızca tek seferlik seçeneklerle yanıt verir, hiçbir zaman "her zaman" ile değil.
- **Başlangıçta ağ erişimi.** Yayımlanmış ikili dosya, model kataloğunu her başlangıçta `https://models.dev/api.json` adresinden indirir. Bunu yalnızca bu özelliği içermeyen bir derleme kapatır.

Praxi'de seçilen model, oturumun `model` yapılandırma seçeneği olarak gönderilir. Goose bunu o oturumda tutar ve kullanıcının varsayılanı olarak kaydetmez.
