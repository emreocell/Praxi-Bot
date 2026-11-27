# CLI başvurusu

Komutun adı `bot`. Bu sayfa `apps/cli/src/main.rs` içindeki komut tanımlarını izler. Elindeki
sürümün tam çıktısını her zaman `bot --help` ve `bot <command> --help` gösterir.

```
bot [GLOBAL OPTIONS] <COMMAND>
```

`bot` komutunu hiçbir komut vermeden çalıştırırsan, `bot setup` çalıştırmanı söyleyen bir ipucu yazar.

## Genel seçenekler

| Seçenek | Ortam değişkeni | Varsayılan | Anlamı |
| --- | --- | --- | --- |
| `--data-dir <PATH>` | `BOT_DATA_DIR` | işletim sisteminin uygulama veri klasörü | Veri klasörü |
| `--catalog-dir <PATH>` | `BOT_CATALOG_DIR` | yerleşik küme | Sağlayıcı katalog `*.toml` dosyalarının klasörü |
| `--secrets-backend <auto\|keyring\|file>` | `BOT_SECRETS_BACKEND` | `auto` | `auto` = varsa şifreli dosya, yoksa işletim sisteminin anahtarlığı (dosya arka ucu yalnızca `BOT_SECRETS_PASSPHRASE` ayarlıysa kendiliğinden seçilir, aksi halde `--secrets-backend file` ver). Dosyanın parolası `BOT_SECRETS_PASSPHRASE` değişkeninden ya da bir istemden gelir |
| `--json` | | kapalı | Makinenin okuyacağı çıktı (JSON, `chat` için NDJSON). `status`, `chat`, `bots`, `answer`, `approve`, `grants`, `schedules`, `email`, `mcp add`, `doctor`, `serve`, `backup`, `restore`, `providers list`, `secrets list`, `skills list` ve `channels telegram pair` / `status` için geçerlidir. `host status`, `devices`, `revoke`, `standby`, `activate` ve `workflow` zaten JSON yazdırır. `--json`, `host invite` komutunun QR yerine JSON yazdırmasını sağlar. Hatalar her zaman stderr'e metin olarak gider |
| `--port <PORT>` | `BOT_PORT` | `47821` | `serve` komutunun `127.0.0.1` üzerinde bağlandığı port. İstemciler buna bağlanır |
| `--url <URL>` | `BOT_URL` | | İstemci komutları için tam daemon URL'si (`--port` değerini geçersiz kılar) |
| `--remote <ENDPOINT>` | | | Eşleşmiş bir daemon'un eş uç nokta kimliği (iroh). `--url` ile birlikte kullanılamaz |
| `--version`, `--help` | | | |

`keychain`, `--secrets-backend` için `keyring` değerinin takma adı olarak kabul edilir.

Çalışan bir daemon ile konuşan komutlar (`status`, `chat`, `bots`, `answer`,
`approve`, `grants`, `schedules`, `workflow`, `email`, `mcp add`, `token`,
`channels telegram pair` ve `join` dışındaki her `host` komutu) API token'ını,
ayarlıysa `BOT_API_TOKEN` değişkeninden, değilse `<data dir>/api-token` dosyasından okur.
`bot serve` ile aynı veri klasörünü kullan.
`--remote`, eşleşmiş bir daemon'a `status`, `proxy`, `chat`, `bots`,
`answer`, `approve`, `grants`, `schedules`, `workflow`, `email`,
`channels telegram pair` ve `host invite`, `status`, `devices`, `revoke`,
`standby` ile `activate` komutları için ulaşır.
`bot --remote <ENDPOINT> proxy` tüneli açar, loopback `url` ve `token` değerlerini yazdırır ve
Ctrl-C'ye kadar açık tutar ([bağlantı ölçüm kılavuzu](../../design/live-view-link-measurement.md) tarafından kullanılır).
`host join`, `screen`, `admin`, `add`, `repair`, `upgrade` ve `remove` ile
`token`, `mcp`, `doctor` ve `setup` bu makinede kalır.
`screen`, `admin` ve SSH ana makine komutları yine yerel daemon'u kullanır.

## Çıkış kodları

`bot --help` içinde belgelenmiştir ve istemci komutları arasında sabittir:

| Kod | Anlamı |
| --- | --- |
| 0 | Tamam |
| 1 | Genel hata (herhangi bir denetim başarısız olduğunda `bot doctor` da bunu döndürür) |
| 2 | Kullanım hatası |
| 3 | Daemon'a ulaşılamıyor |
| 4 | Kimlik doğrulama reddedildi (ya da API token'ı yok, ya da Host/Origin reddedildi) |
| 5 | Bulunamadı |
| 6 | İstek reddedildi (geçersiz girdi, çakışma, `setup_required` ya da başarısız bir `bot email test`) |
| 7 | Çalışma bir hatayla bitti ya da iptal edildi |

## Komutlar

### `bot setup`

Etkileşimli ilk çalıştırma kurulumu (dil, sağlayıcı, anahtar, model, ilk bot,
izin hazır ayarı). `--provider` verilirse betikle çalışır.

| Seçenek | Anlamı |
| --- | --- |
| `--provider <ID>` | Sağlayıcı kimliği. Etkileşimsiz kipe geçer |
| `--base-url <URL>` | `--provider` değerini özel bir OpenAI uyumlu sağlayıcı yapar (`--provider` gerekir) |
| `--key-env <VAR>` | API anahtarını tutan ortam değişkeni (`--provider` gerekir) |
| `--model <MODEL>` | Varsayılan model (verilmezse listedeki ilk model, `--provider` gerekir) |
| `-y`, `--yes` | Hiçbir zaman sormaz (`--provider` ile zorunludur) |
| `--lang <en\|tr>` | Botla birlikte kaydedilen iletilerin dili (varsayılan `en`) |
| `--bot-preset <general\|browser\|desktop>` | İlk bot hazır ayarı (varsayılan `general`) |
| `--bot-name <NAME>` | İlk botun adı |
| `--permission <read-only\|read-and-draft\|approve-destructive\|full>` | Varsayılan izin hazır ayarı (varsayılan `approve-destructive`) |

### `bot serve`

Yerel daemon API'sini Ctrl-C ya da SIGTERM'e kadar `127.0.0.1` üzerinde çalıştırır. Henüz
sağlayıcı ayarlanmamış olsa bile başlar ve `bot setup` ya da `bot providers add <ID>`
çalıştırana kadar `setup_required` bildirir.

| Seçenek | Anlamı |
| --- | --- |
| `--host` | Her zaman açık bir ana makine olarak çalıştırır (eş ağı varsayılan olarak açık) |
| `--peer` | iroh üzerinden eşleşmiş uzak API erişimini açar |
| `--shutdown-grace <SECONDS>` | Kapanışta çalışan araç adımlarının bitirmek için aldığı süre (varsayılan 10). Sonra canlı çalışmalar bir sonraki açılış için duraklatılır |
| `--owner-key-stdin` | Sahip anahtarını (tek satır) standart girdiden okur, böylece argv'de, bir dosyada ya da ortamda hiç görünmez |
| `--safe` (takma ad `--safe-mode`, ya da ortam `BOT_SAFE_MODE=1`) | Güvenli kip: sohbet araçsız yanıt verir. Araçlar, zamanlamalar, kanallar, MCP sunucuları, uygulamalar, hafıza çalışması, masaüstü ve eş ağı kapalı kalır. Hiçbir şey silinmez ya da değiştirilmez. `--host` ya da `--peer` ile birleştirilemez. Bkz. [Güvenli kip](troubleshooting.md) |
| `--fake-provider` | Yalnızca `fake-provider` özellikli derlemelerde: yerleşik bir yankı sağlayıcısı kullanır (gösteriler, testler) |

### `bot logs`

Bu bilgisayarın yakın zamanlı, maskelenmiş işletim günlüklerini, daemon durmuşken bile okur.
Çıktı her zaman JSON satırlarıdır. Ağ bağlantısı ya da gizli bilgi deposu erişimi gerekmez.
Servisle aynı `--data-dir` değerini kullan. Bkz. [Sorun giderme](troubleshooting.md).

| Seçenek | Anlamı |
| --- | --- |
| `--follow` | Yeni kayıtları bekler. Ctrl-C ile durdur |
| `--level <trace\|debug\|info\|warn\|error>` | Yalnızca bu düzeyi gösterir |
| `--since <TIME>` | UTC Unix milisaniyesi ya da `15m`, `2h`, `1d` gibi bir yaş |

### `bot doctor`

Kurulumun sağlığını denetler (veri klasörünün yazılabilirliği ve boş disk alanı, yapılandırma,
sağlayıcılar, daemon, tarayıcı ve daha fazlası). Hiçbir zaman sormaz. Herhangi bir denetim
başarısız olursa 1 ile çıkar.

| Seçenek | Anlamı |
| --- | --- |
| `--online` | Ağ üzerinden sağlayıcı bağlantılarını ve eş röle erişilebilirliğini de sınar |
| `--report <FILE>` | Bu dosyaya maskelenmiş bir tanılama paketi (JSON, gizli bilgi yok) yazar |
| `--time-url <HTTPS_URL>` | Bu bilgisayarın saatini bu https adresinin `Date` başlığıyla karşılaştırır ve büyük bir farkta (2 dakikadan fazla) uyarır. Varsayılan olarak kapalıdır: verilmezse hiçbir istek yapılmaz. Adrese ulaşılamazsa denetim atlanır ve doctor'ı hiçbir zaman başarısız yapmaz |

`--json`, denetimleri JSON olarak yazdırır.

### `bot browser install`

Sağlama toplamı doğrulanmış bir Chrome for Testing'i veri klasörüne indirir ve çalıştığını
görmek için bir kez başlatır. Botun kullanabileceği tarayıcısı olmayan bir Linux bilgisayar için
(bir sunucu ya da tek Chromium'u, sertleştirilmiş servisin çalıştıramadığı snap paketi olan
Ubuntu). Arşiv, `storage.googleapis.com` adresinden https üzerinden gelir; sürümü ve SHA-256
değeri sabitlenmiştir, eşleşmeden hiçbir şey açılmaz. Yaklaşık 1 GB boş disk alanı gerekir, root
gerekmez. Bot bu tarayıcıyı yalnızca sistemde Chrome, Chromium ya da Edge yoksa kullanır. Chrome
for Testing kendini güncellemez; yeni bir Praxi yeni sabitlenmiş sürümü getirir.
`install-linux.sh` ile kurulmuş bir sunucuda servisin kullanıcısıyla çalıştır:
`sudo praxi browser install`. Ayarlar > Tanılama'da aynı işlem **Tarayıcıyı kur** düğmesidir.
`--json`, `{"installed", "path", "already_there"}` yazdırır.

### `bot cli status | install | login`

Komutu çalıştıran **kullanıcının** kodlama CLI'ları (Claude Code, Codex, Antigravity, ...): o kullanıcının ev
klasöründe ve arama yolunda aranırlar; uygulama ve daemon da böyle yapar. Bir sunucuda bu kullanıcı Praxi'nin
çalıştığı kullanıcı olmalıdır; bu yüzden ona geçen kurucu komutunu kullan: `sudo praxi cli status`. Başka biri
olarak çalıştırılırsa komut bunu söyler (*Sen ubuntu'sun ama Praxi praxi olarak çalışıyor*) ve kullanılacak biçimi gösterir.

| Komut | Anlamı |
| --- | --- |
| `bot cli status` | Baktığı kullanıcı ve ev klasörü, sonra her CLI: kurulu mu, sürüm, nerede bulundu, girişli mi, girişsiz mi ya da "giriş dosyası bulundu (denetlenmedi)". `--json` aynısını JSON olarak yazar. Giriş dosyasını asla okumaz, yalnızca var olup olmadığına bakar |
| `bot cli install <ENGINE>` | Motorun sabit kurucusunu bu kullanıcı olarak çalıştırır (uygulamadaki **Sunucuya kur**'un çalıştırdığı aynı kurucu). Bir npm paketi gerekiyor ve npm yoksa önce Node.js'i veri klasörüne indirir. Yalnızca kopyala-çalıştır komutu olan motorlar bunu söyler |
| `bot cli login <ENGINE>` | CLI'ın kendi girişini bu terminalde başlatır (`claude auth login`, `codex login`, `agy`, `vibe --setup`, ...); yanıtları ve yazdığı bağlantı dahil, bu kullanıcı olarak |

`<ENGINE>`, `bot cli status`'un gösterdiği kimliktir (`claude`, `codex`, `cursor`, `opencode`, `agy`, `qwen`,
`copilot`, `kiro`, `kimi`, `qoder`, `vibe`, `goose`). Bkz. [Sunucuda kodlama CLI'ları](remote-host.md#sunucuda-kodlama-cliları-claude-code-codex-antigravity-).

### `bot backup create | verify | restore`

`bot backup create <FILE>`; botlarını, sohbetlerini, hafızanı, becerilerini, iş akışlarını,
zamanlamalarını ve sağlayıcı ayarlarını içeren, parolayla şifrelenmiş tek bir dosya yazar. Var olan
bir dosyanın üzerine hiçbir zaman yazmaz. `verify`, veri klasörüne dokunmadan dosyanın şifresini çözer
ve denetler. `restore`, dosyayı bu kuruluma açar. Önce daemon'u durdur (`bot serve` veri klasörünü
tutuyorsa ya da `--port` üzerinde bir daemon yanıt veriyorsa reddeder). Bkz.
[Şifreli yedekleme ve geri yükleme](backup.md).

Parola, gizli bir istemden, `--passphrase-file <FILE>` ile ya da `--passphrase-env <VAR>` ile
(değişkenin adı) gelir. Hiçbir zaman bir komut satırı değeri olmaz.

| Seçenek | Komutlar | Anlamı |
| --- | --- | --- |
| `--passphrase-file <FILE>` | hepsi | Parolayı bir dosyanın ilk satırından okur |
| `--passphrase-env <VAR>` | hepsi | Parolayı bu ortam değişkeninden okur |
| `--include-secrets` | `create` | API anahtarlarını ve token'ları da (şifreli olarak) yedeğe koyar |
| `--include-secrets` | `restore` | Yedekteki API anahtarlarını gizli bilgi deposuna yazar (varsayılan: atla) |
| `--replace` | `restore` | Var olan botları ve sohbetleri yedektekilerle değiştirir (eski durum bir `backup-*` klasörüne gider) |
| `--allow-policy-relaxation` | `restore` | İzin politikası daha serbest olan bir yedeği kabul eder |

### `bot restore <FILE>`

Eski bir sürümün yaptığı düz metin arşivini geri yükler. Önce daemon'u durdur.
`bot serve` veri klasörünü tutuyorsa ya da `--port` üzerinde bir daemon yanıt veriyorsa
reddeder. Şifreli yedekler için `bot backup restore` kullanılır. Bkz.
[Eski düz metin yedekleri geri yükleme](backup-and-restore.md).

| Seçenek | Anlamı |
| --- | --- |
| `--replace` | Var olan botları ve sohbetleri yedektekilerle değiştirir. Eski durum veri klasörünün içindeki bir `backup-*` klasörüne gider |
| `--allow-policy-relaxation` | İzin politikası şu anki politikadan daha serbest olan bir yedeği kabul eder |

### `bot status`

Daemon'un sağlığını ve sürümünü, çalışma süresini ve bekleyen onayların sayısını gösterir ve
API token'ını doğrular. Sağlayıcı ayarlanmadan önce `bot status`, `setup required` bildirir ve
çalıştırılacak sonraki komutu yazdırır (`bot setup` ya da `bot providers add <id>`).

### `bot proxy`

Eşleşmiş bir uzak daemon'a loopback HTTP vekil tüneli açar ve `Ctrl-C`'ye kadar açık tutar.
`--remote <ENDPOINT>` gerekir (`--url` ile birlikte kullanılamaz):

```sh
bot --remote <ENDPOINT> proxy
```

`bot proxy`, iroh tünelinin bağlanması için en fazla 20 saniye bekler, sonra loopback `url:` ve
`token:` değerlerini yazdırır. **Güvenlik:** yazdırılan token, eşleşmiş ana makineye her yolda
(`/v1/secrets` dahil) masaüstü rolünde erişim verir. Onu paylaşma ya da günlüğe yazma, araçlara
komut satırı argümanıyla değil `BOT_API_TOKEN` ile ver ve işin biter bitmez `Ctrl-C`'ye bas.
[Canlı görünüm bağlantı ölçüm kılavuzu](../../design/live-view-link-measurement.md) tarafından kullanılır
(`BOT_API_TOKEN=<token> node scripts/tools/measure-live-link.mjs --url <url> --seconds 30`).

### `bot chat <BOT_ID> <MESSAGE>`

Bir ileti gönderir ve yanıtı akıtarak gösterir.

| Seçenek | Anlamı |
| --- | --- |
| `--chat <CHAT_ID>` | Yeni bir sohbet başlatmak yerine var olan bir sohbete devam eder |
| `--mode <steer\|queue>` | Sohbette çalışan bir tur varsa ne yapılacağı (varsayılan `queue`) |

### `bot chats`

Kayıtlı sohbetleri listeler ve dışa aktarır. Dışa aktarma yalnızca sahibe açıktır ve masaüstü
uygulamasıyla aynı Markdown ve JSON dosyalarını kullanır.

| Alt komut | Anlamı |
| --- | --- |
| `bot chats list [--bot <BOT_ID>]` | Sohbetleri listeler. `--json` API dizisini yazdırır |
| `bot chats export <CHAT_ID>` | Bir sohbeti stdout'a yazar. Seçenekler: `--format md\|json` (varsayılan `md`, genel `--json` JSON'a zorlar), `-o, --output <FILE>` özel bir dosyaya yazar (Unix'te `0600` kipi, var olan bir yol ya da sembolik bağ reddedilir) ve `--force` var olan düz bir dosyanın yerine yazar |
| `bot chats import <FILE> --bot <BOT_ID>` | Sürüm 1 JSON dışa aktarmasından yeni bir sohbet oluşturur. İçe aktarılan iletiler güvenilmez geçmiştir, araç adımları ve onaylar hiçbir zaman çalıştırılmaz |

İçe aktarılan iletiler `user` ya da `assistant` rolünü korur, ama `imported` kaynağıyla işaretlenir ve
güvenilmez içerik olarak saklanır. Bu yüzden sonraki veri çıkışı ve araç kullanımı yine sorar. Bir dosyadaki
araç adımları ve onaylar, tek bir etkisiz `imported_note` iletisinde listelenir ve çalışma ya da onay
tablolarına hiçbir zaman girmez. İçe aktarma yalnızca `format_version: 1` kabul eder. Bozuk, daha yeni
ya da çok büyük dosyalar 6 çıkış koduyla başarısız olur.

### `bot bots`

| Alt komut | Anlamı |
| --- | --- |
| `bot bots list` | Botları listeler |
| `bot bots add <FILE>` | Bir profil TOML dosyasından bot oluşturur |

### `bot profile`

Bir botu gizli bilgi içermeyen tek bir dosya olarak paylaş (bkz.
[Botlar ve profiller](bots-and-profiles.md)).

| Alt komut | Anlamı |
| --- | --- |
| `bot profile export <BOT_ID> [--out FILE] [--schedules]` | Botun paylaşılabilir profilini yazar (bir dosyanın üzerine hiçbir zaman yazmaz) |
| `bot profile import <FILE> [--level LEVEL] [--model M] [--name N] [--computer-files] [--yes]` | Profilin neler yapabileceğini gösterir. `--yes` ile botu oluşturur, hiçbir zaman `--level` değerinin üstünde değil (varsayılan `read-only`) |

### `bot import`

Claude Desktop ve Claude Code, Cursor, VS Code, Codex, OpenClaw, Hermes ya da beceri ve komut istemi
içeren düz bir markdown klasöründen MCP sunucularını, becerileri ve yönergeleri getirir.
Önce her zaman bir önizleme gelir (içe aktarılacaklar / atlananlar ve nedenleri). Sunucular kapalı gelir.
Gizli bir değer ancak o değeri onayladıktan sonra gizli bilgi deposuna gider (terminalde her değer için sorulur,
ya da `--move-secrets`) ve hiçbir zaman düz metin olarak bir yapılandırmaya yazılmaz. Yalnızca tarayıcının düşük
riskli bulduğu beceriler içe aktarılır, diğerleri `bot skills install` için bırakılır. `CLAUDE.md` / `AGENTS.md`
yalnızca `--bot` ile bir botun yönergelerine eklenir. Sohbetler, notlar, API anahtarları ve oturum açma bilgileri
hiçbir zaman kopyalanmaz. Masaüstü uygulamasında aynı akış Ayarlar altındadır. `run` işini daemon yapar;
`--server` ile sunucunun kendi dosyalarından içe aktarılır. Önizleme bir sunucunun komut satırının ya da
adresinin tamamını gösterir (çok uzunsa `…` ile kesilir); yönerge dosyasında yalnızca boyutunu gösterir.
`--folder` o makinenin herhangi bir klasörü olabilir, ama `/proc`, `/sys`, `/dev` ya da bir aygıt yolu olamaz;
klasörün içindeki bağlantılar izlenmez ve çok büyük dosyalar okunmaz.

| Alt komut | Anlamı |
| --- | --- |
| `bot import detect [--from APP] [--folder DIR]` | Diğer uygulamaların kurulumlarını arar ve önizlemeyi gösterir. Hiçbir şey yazmaz, daemon gerektirmez |
| `bot import run [--from APP] [--folder DIR] [--yes] [--move-secrets] [--bot BOT_ID]` | Önizlemeyi gösterir. `--yes` ile daemon üzerinden içe aktarır. `APP`: `claude`, `cursor`, `vscode`, `codex`, `openclaw`, `hermes` ya da `folder` |

### `bot approve <TOOL_CALL_ID>`

Bekleyen bir araç çağrısını onaylar ya da reddeder.

| Seçenek | Anlamı |
| --- | --- |
| `--scope <once\|this_chat\|always\|always_category\|run_control>` | Onayın ne kadar yayıldığı (varsayılan `once`). `run_control` yalnızca kartın sunduğu yerde geçerlidir |
| `--deny` | Onaylamak yerine reddeder |
| `--reason <TEXT>` | Modele verilen neden. Yalnızca `--deny` ile |

### `bot answer <CALL_ID> <TEXT>`

Bekleyen bir soru kartını yanıtlar.

### `bot grants`

| Alt komut | Anlamı |
| --- | --- |
| `bot grants list <BOT_ID>` | Botun kalıcı “her zaman izin ver” izinlerini listeler |
| `bot grants revoke <BOT_ID> --tool <NAME>` | Tek bir aracın iznini kaldırır |
| `bot grants revoke <BOT_ID> --category <NAME>` | Bir izin kategorisinin iznini kaldırır (`fs`, `shell`, ...) |

`--tool` ve `--category` birbirinin alternatifidir. Tam olarak birini ver.

### `bot schedules`

| Alt komut | Anlamı |
| --- | --- |
| `bot schedules list` | Zamanlamaları listeler |
| `bot schedules add <BOT_ID> <WHEN> <PROMPT>` | Bir zamanlama ekler. `<WHEN>`, “every 15 minutes”, “in 2 hours” ya da “her gün 09:00” gibi bir ifadedir (İngilizce ya da Türkçe, daemon ayrıştırır). Seçenekler: `--name <NAME>` (varsayılan: istemin başı), `--tz <IANA_TZ>` (varsayılan UTC), `--chat <CHAT_ID>` (varsayılan: zamanlamanın adını taşıyan bir sohbet) |
| `bot schedules rm <ID>` | Bir zamanlamayı siler |
| `bot schedules run-now <ID>` | Bir zamanlamayı şimdi, arka planda çalıştırır |

### `bot email`

Çalışan daemon'un e-posta hesaplarını yönetir (bkz. [e-posta](email.md)). Değişiklikler bir sonraki araç çağrısında geçerli olur.

| Alt komut | Anlamı |
| --- | --- |
| `bot email list` | Hesapları listeler (parolaları hiçbir zaman göstermez) |
| `bot email add <NAME> <ADDRESS>` | Bir hesap ekler (`--provider` varsayılanı `imap_smtp`). Ad yerine `--name <NAME>` de verilebilir (ikisi birden olmaz). Seçenekler: `--preset gmail\|outlook\|yahoo\|icloud`, `--imap-host` (port 993, TLS), `--imap-port`, `--smtp-host`, `--smtp-port` (örtük TLS ile 465, `start_tls` ile 587), `--smtp-security implicit\|start_tls`, `--username` (kullanıcı adı adresten farklıysa), `--bot <NAME_OR_ID>` (yalnızca bu bot) ya da `--all-bots` (ikisinden biri zorunlu), `--allowed-sender <ADDR>` (tekrarlanabilir), `--password-env <VAR>` (verilmezse gizli istem) |
| `bot email add --provider agentmail --name <NAME> --bot <NAME_OR_ID>\|--all-bots` | **Kendi** AgentMail API anahtarınla bir AgentMail gelen kutusu oluşturur ve onu hesap olarak ekler. Seçenekler: `--name-hint <TEXT>` (`@` işaretinden önceki bölüm, varsayılan: hesap adı), `--domain <VERIFIED_DOMAIN>` (varsayılan: paylaşılan `agentmail.to`), `--allowed-sender`, `--api-key-env <VAR>` (verilmezse gizli istem, anahtar hiçbir zaman argüman olmaz) |
| `bot email rm <NAME>` | Bir hesabı ve saklanan parolasını ya da anahtarını kaldırır. `--delete-inbox` gelen kutusunu AgentMail'de de siler (geri alınamaz, varsayılan olarak kapalı) |
| `bot email test <NAME>` | IMAP girişi ile SMTP kimlik doğrulamasını ya da AgentMail anahtarını sınar. Hiçbir şey gönderilmez. Denetim geçmezse 6 ile çıkar |

### `bot providers`

| Alt komut | Anlamı |
| --- | --- |
| `bot providers list` | Ayarlı sağlayıcıları listeler (`*` = genel varsayılan). `--json`: `{id, default, enabled, kind: "api"\|"cli", model, key}` dizisi (`key` gizli bilginin adıdır, değeri hiçbir zaman değil) |
| `bot providers detect-local` | Loopback üzerinde yerel bir model sunucusu (Ollama, LM Studio, llama.cpp) arar ve bulduğunu JSON olarak yazdırır. Yalnızca `127.0.0.1` ile iletişim kurar |
| `bot providers add <ID>` | Bir sağlayıcı ekler ya da günceller. Yerleşik katalog kimlikleri: `anthropic`, `deepseek`, `gemini`, `gemini-openai-compat`, `lmstudio`, `ollama`, `openrouter`. Seçenekler: `--base-url <URL>` (özel OpenAI uyumlu sağlayıcı), `--key-env <VAR>`, `--model <MODEL>`, `--no-test`. `--json`, `{id, default, enabled, kind, model, key, tested}` yazdırır ve anahtarı hiçbir zaman yazdırmaz |
| `bot providers test [ID]` | Bir sağlayıcının bağlantısını sınar (verilmezse varsayılan sağlayıcı). `--json`, `{id, ok, models}` yazdırır |
| `bot providers default <ID>` | Genel varsayılan sağlayıcıyı ayarlar. Seçenek: `--model <MODEL>`. `--json`, `{default, model}` yazdırır |

### `bot catalog`

Özenle hazırlanmış model kataloğunu doğrular, güncelliğini denetler ya da bir sağlayıcının model
listesiyle karşılaştırır (bkz. [model kataloğu](model-catalog.md)). Varsayılan olarak çevrimdışıdır.
Ağa çıkan tek çağrılar `--live` ve `update` komutlarıdır (`--live` isteğe bağlıdır. `update` sabit bir
https adresinden imzalı bir katalog çeker ve senin hakkında hiçbir şey göndermez).

| Alt komut | Anlamı |
| --- | --- |
| `bot catalog check` | `model_metadata.json` dosyasını, sağlayıcı katalog girdilerini ve isteğe bağlı olarak belge tablosunu doğrular. Seçenekler: `--metadata <FILE>`, `--docs <FILE>`, `--max-age-days <DAYS>` (varsayılan 90), `--strict` (eskimişse sıfırdan farklı kodla çıkar) |
| `bot catalog diff <PROVIDER> [SNAPSHOT]` | Bir sağlayıcı için işlenmiş kataloğu bir JSON ya da TOML model listesi anlık görüntüsüyle karşılaştırır. Seçenekler: `--live` (ayarlı sağlayıcıyı saklanan anahtarıyla sorgular), `--metadata <FILE>` |
| `bot catalog update` | İmzalı, daha yeni model kataloğunu indirir ve imzası geçerliyse ve elindekinden eski değilse kurar. Seçenek: `--check` (yalnızca bildirir, hiçbir şey kurmaz) |
| `bot catalog auto-update on\|off\|status` | Günlük otomatik güncellemeyi açar ya da kapatır (varsayılan açık) veya durumunu gösterir. `BOT_CATALOG_UPDATES=off` da onu kapatır |
| `bot catalog render-docs` | `model_metadata.json` dosyasından üretilen Markdown sağlayıcı tablosunu yazdırır. Seçenek: `--metadata <FILE>` |

### `bot secrets`

| Alt komut | Anlamı |
| --- | --- |
| `bot secrets list` | Gizli bilgi adlarını listeler (değerleri hiçbir zaman göstermez). `--json`: ad dizisi |
| `bot secrets set <NAME>` | Bir gizli bilgi saklar. Değer `--value-env <VAR>` ile ya da gizli bir istemden gelir, komut satırından hiçbir zaman |
| `bot secrets delete <NAME>` | Bir gizli bilgiyi siler |

### `bot channels telegram`

| Alt komut | Anlamı |
| --- | --- |
| `setup [--token-env <VAR>]` | BotFather token'ını saklar (gizli istem) ve kanalı açar. `--json`, `{channel, configured, enabled, restart_required}` yazdırır |
| `pair` | Çalışan daemon'dan tek kullanımlık, 6 haneli bir eşleştirme kodu yazdırır (10 dakika geçerli) |
| `status` | Kanalın ayarlı ve çalışıyor olup olmadığını ve kaç kullanıcının eşleştiğini gösterir |
| `disable` | Kanalı kapatır (token saklı kalır). `--json`, `{channel, enabled, restart_required}` yazdırır |

### `bot channels discord`

| Alt komut | Anlamı |
| --- | --- |
| `setup [--token-env <VAR>] [--lang en\|tr\|de]` | Adımları anlatır, Discord bot token'ını saklar (gizli istem), kanalı açar ve token'ı sınar. `--json`, `{channel, configured, enabled, restart_required}` ile sınama sonucunu yazdırır |
| `test` | Saklanan token'ı Discord ile denetler ve neyin yanlış olduğunu sade sözlerle söyler |
| `pair` | Tek kullanımlık, 6 haneli bir eşleştirme kodu yazdırır (10 dakika geçerli). Bota özel mesajla `!pair <code>` gönder |
| `status` | Kanalın ayarlı ve çalışıyor olup olmadığını ve bir sahibin eşleşip eşleşmediğini gösterir |
| `disable` | Kanalı kapatır (token saklı kalır) |

### `bot completions <SHELL>`

`bash`, `zsh`, `fish`, `powershell` ya da `elvish` için bir tamamlama betiğini standart çıktıya yazdırır.
Betiği kurduktan sonra yeni bir kabuk oturumu aç, `bot` yaz, bir boşluk bırak ve Tab'a bas.

#### Bash

```bash
mkdir -p "$HOME/.local/share/bot"
bot completions bash > "$HOME/.local/share/bot/bot.bash"
```

Şu satırı `~/.bashrc` dosyasına ekle. Oturum açma kabuğu kullanıyorsan, oturum açma profilinin
`~/.bashrc` dosyasını yüklediğinden emin ol.

```bash
. "$HOME/.local/share/bot/bot.bash"
```

#### Zsh

```zsh
mkdir -p "$HOME/.zfunc"
bot completions zsh > "$HOME/.zfunc/_bot"
```

`fpath=("$HOME/.zfunc" "${fpath[@]}")` satırını `~/.zshrc` dosyasında `compinit` çalışmadan önce ekle.
Yapılandırman tamamlamaları henüz başlatmıyorsa, `fpath` ayarlandıktan sonra şu satırları ekle.

```zsh
autoload -Uz compinit
compinit
```

#### Fish

Fish, `bot.fish` dosyasını tamamlamalar klasöründen gerektiğinde yükler. `XDG_CONFIG_HOME`
ayarladıysan, iki komutta da `$XDG_CONFIG_HOME/fish/completions` kullan.

```fish
mkdir -p ~/.config/fish/completions
bot completions fish > ~/.config/fish/completions/bot.fish
```

#### PowerShell

```powershell
$profileDir = Split-Path -Parent $PROFILE
New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
bot completions powershell | Set-Content -Encoding utf8 (Join-Path $profileDir 'bot-completions.ps1')
```

`$PROFILE` konumundaki dosyayı düzenleyicinde aç (yoksa oluştur) ve şu satırı ekle.

```powershell
. (Join-Path (Split-Path -Parent $PROFILE) 'bot-completions.ps1')
```

#### Elvish

```elvish
bot completions elvish > ~/.bot-completions.elv
```

Şu satırı `rc.elv` dosyana ekle. Konumu için
[Elvish başlangıç dosyası başvurusuna](https://elv.sh/ref/command.html#rc-file) bak.

```elvish
eval (slurp < ~/.bot-completions.elv)
```

### `bot man-pages <OUTPUT_DIR>`

`bot` ve görünür alt komutları için, iç içe komutlar dahil, 1. bölüm kılavuz sayfaları üretir.
Komut adları kısa çizgiyle birleştirilir, örneğin `bot-channels-telegram-setup.1`.

```sh
bot man-pages target/man
man -l target/man/bot-channels-telegram-setup.1
```

macOS'ta dosyayı önizlemek için `man ./target/man/bot-channels-telegram-setup.1` kullan.

Kaynaktan derlerken aynı sayfaları üretmek için depo kökünden
`cargo run -p cli -- man-pages target/man` çalıştır. Bir paket için `DESTDIR` değişkenini
hazırlık klasörüne ayarla ve dosyaları `usr/share/man/man1` altına kur.

```sh
install -d "${DESTDIR:?Set DESTDIR to the package staging directory}/usr/share/man/man1"
install -m 0644 target/man/*.1 "$DESTDIR/usr/share/man/man1/"
```

Paket kurulduktan sonra sayfaları `man 1 bot` ya da `man 1 bot-channels-telegram-setup` ile aç.

### `bot skills`

Becerileri gözden geçirir, kurar ve yönetir. Bunlar bu makinedeki yerel, açık komutlardır.
Daemon komutu değildirler ve hiçbir zaman ajan aracı olmazlar, bu yüzden bir bot beceri kuramaz.

| Alt komut | Anlamı |
| --- | --- |
| `bot skills review <NAME>` | Her dosyayı tam olarak ve inceleme parmak izini yazdırır. Seçenek: `--bot-id <BOT_ID>` |
| `bot skills trust <NAME> --reviewed <FINGERPRINT>` | Tam olarak `review` komutunun yazdırdığı içeriğe güvenir. Seçenek: `--bot-id <BOT_ID>`. Sonraki bir değişiklik yeni bir inceleme gerektirir |
| `bot skills install <SOURCE>` | Bir `https://`/`ssh://` git URL'sinden, bir https dizininden (`--name <SKILL>`) ya da yerel bir klasörden kurar. Önce risk raporunu yazdırır. Seçenekler: `--rev`, `--subdir`, `--name`, `--accept <FINGERPRINT>` |
| `bot skills update [NAME]` | Merkezden kurulan becerileri yeniden çeker, nelerin değiştiğini gösterir (yeni bulgular, risk, commit) ve değiştirmeden önce sorar. Bir NAME ile `--accept <FINGERPRINT>` |
| `bot skills uninstall <NAME>` | Beceri klasörünü, kilit girdisini ve kullanım sayaçlarını kaldırır |
| `bot skills list` | Ad, sürüm, kaynak, sabitlenmiş commit, risk, kullanım. Eksik ya da yerelde değiştirilmiş becerileri işaretler. `--json`: `{name, version, source, commit, risk, used, state: "ok"\|"missing"\|"modified"}` dizisi |
| `bot skills sync` | Klasörü eksik olan kilitli becerileri, kilitlenen özeti doğrulayarak yeniden kurar |
| `bot skills index validate <SOURCE>` | Bir dizin dosyasını ya da https URL'sini [dizin biçimine](../../skills-index.md) göre denetler. Hata varsa sıfırdan farklı kodla çıkar. Seçenek: `--check-content` (her girdiyi çeker, hiçbir zaman kurmaz ve `sha256` değerini karşılaştırır) |

Bkz. [Beceriler: bir beceri kur](skills.md).

### `bot token`

Yerel daemon üzerinde kapsamlı MCP kimlik bilgileri oluşturur ya da iptal eder (bkz.
[MCP sunucusu](mcp-server.md)). JSON yazdırır. `--remote` ile kullanılamaz.

| Alt komut | Anlamı |
| --- | --- |
| `bot token create --bot <UUID>[,UUID...]` | Bir kimlik bilgisi saklar ve herkese açık kimliğini yazdırır (taşıyıcı token'ı hiçbir zaman değil). `--bot` zorunludur (virgülle ayrılmış). `--scope <mcp:tasks\|mcp:computer>[,...]` (varsayılan `mcp:tasks`). `--ttl <DURATION>` (varsayılan `30d`, örneğin `12h` ya da `600s`, 60 saniyeden 30 güne kadar) |
| `bot token revoke <ID>` | Bir kimlik bilgisini iptal eder ve çalışan görevlerini iptal eder |

### `bot mcp`

Daemon'un MCP sunucularını yönetir (bkz. [MCP sunucuları](mcp-servers.md) ve
[MCP sunucusu](mcp-server.md)).

| Alt komut | Anlamı |
| --- | --- |
| `bot mcp add [OPTIONS] <PASTE>...` | Yapıştırılan bir metinden sunucu ekler: bir sunucu URL'si, bir `claude mcp add ...` satırı, bir `npx` / `uvx` komut satırı ya da MCP JSON (`{"mcpServers": ...}`). Yapıştırdığın metni tırnak içine al ya da stdin'den okumak için `-` ver. Yapıştırmadaki değerler (`-e KEY=value`, başlıklar, `env`) gizli bilgi deposuna gider, yapılandırmaya hiçbir zaman. Bayrakları yapıştırılan metnin önüne koy. `--name <NAME>` (varsayılan: URL'den ya da paketten türetilir). `--dry-run` nelerin ekleneceğini gösterir ve hiçbir yerle iletişim kurmaz |
| `bot mcp serve --bot <UUID>` | O botu stdio üzerinde kapsamlı bir Praxi MCP sunucusu olarak sunar, `bot token create` ile alınan bir token kullanır. Yalnızca loopback daemon (`--remote` değil). `--allow <tasks\|computer>[,...]` (varsayılan `tasks`, `computer` açıkça istenmelidir). `--token-id <UUID>`, birden fazla kimlik bilgisi eşleştiğinde `token create` çıktısındaki herkese açık kimliktir (hiçbir zaman taşıyıcı token değil) |

### `bot host`

Etkin ana makineyi ve eşleşmiş cihazları yönetir (bkz. [uzak ana makine](remote-host.md)
ve [ana makine işletimi](host-operations.md)).

| Alt komut | Anlamı |
| --- | --- |
| `bot host join <TICKET>` | Servisi başlatmadan önce bu yeni kurulumu bir masaüstüyle eşleştirir. Yalnızca yerel |
| `bot host invite --role <phone\|desktop\|node\|host> [--ttl-secs <1-1800>] [--ticket-only]` | Tek kullanımlık bir davet oluşturur ve QR kodunu ile onay kodunu yazdırır. `--role` zorunludur. `--ttl-secs` davetin ne kadar süre geçerli kalacağını belirler (1 ile 1800 saniye). `--ticket-only` yalnızca eşleştirme biletini yazdırır, tek komutla sunucu kurulumu gibi betikler için |
| `bot host status` | Ana makine durumunu gösterir (JSON) |
| `bot host devices` | Eşleşmiş cihazları listeler (JSON) |
| `bot host revoke <KEY>` | Eşleşmiş bir cihazı iptal eder |
| `bot host standby` | Otomatik bileşenleri durdurur ve bekleme sahipliğini kalıcı yapar |
| `bot host activate` | Bilinen her epoch değerinin üstünde bir epoch ile sahipliği alır. `--force`, bu kurulumun son yerel verisini kullanır. Komut, diğer liderdeki daha yeni değişikliklerin eksik olabileceği ve kaçırılan zamanlamaların eylemleri yineleyebileceği konusunda uyarır |
| `bot host screen [KEY]` | Eşleşmiş bir masaüstünün bu ana makinenin ekranını izlemesine ve devralmasına izin verir. Ana makinede çalıştır. İzin yeniden başlatmalardan sonra da korunur. `bot host devices` çıktısındaki bir cihaz anahtarını ya da `--all-desktops` ver (ikisinden tam olarak biri). `--view-only` devralma ve girdi olmadan yalnızca izler. `--revoke` izni kaldırır |
| `bot host admin [KEY]` | Eşleşmiş bir masaüstünün bu ana makineye model anahtarları, botlar ve ayarlar göndermesine izin verir. Ana makinede çalıştır. Bir cihaz anahtarı ya da `--all-desktops` ver. `--revoke` izni geri alır. Yeni masaüstü davetleri bunu zaten verir |
| `bot host add <TARGET>` | Praxi'yi SSH üzerinden kendi Ubuntu/Debian sunucuna kurar ve bu bilgisayarla eşleştirir (tekrarlanabilir, kaldığı yerden sürdürmek için yeniden çalıştır). `<TARGET>`, `user@server` ya da kayıtlı bir addır. `--ssh-port <PORT>` (varsayılan 22, daemon'un `--port` değeri değil). `--key <FILE>` (verilmezse ssh-agent, parolalar desteklenmez). `--name <NAME>` (varsayılan: adresten türetilir). `--yes` yeni bir ana makine anahtarını sormadan kabul eder, değişmiş bir anahtarı hiçbir zaman kabul etmez. `--no-desktop` sanal ekranı ve masaüstü oturumunu atlar |
| `bot host repair <TARGET>` | Kurulumu yeniden çalıştırır, düz bir `bot serve` kurulumunu yerinde ana makine kipine taşır (veri korunur ve önce anlık görüntüsü alınır). Hedef seçenekleri `add` ile aynıdır: `--ssh-port <PORT>`, `--key <FILE>`, `--name <NAME>`, `--yes`, `--no-desktop` |
| `bot host upgrade <TARGET>` | Bu uygulamanın sürümünü, daha eski bir sürüm çalıştıran bir sunucuya kurar. Hedef seçenekleri `add` ile aynıdır: `--ssh-port <PORT>`, `--key <FILE>`, `--name <NAME>`, `--yes`, `--no-desktop` |
| `bot host remove <NAME>` | Bir sunucuyu unutur ve bu bilgisayarın oradaki iznini iptal eder. `<NAME>` kayıtlı bir ad ya da `user@server` olabilir. `--uninstall` Praxi'yi sunucudan da kaldırır ve verisini korur. `--purge` kaldırır ve sunucunun verisini, yapılandırmasını ve praxi kullanıcısını siler (`--uninstall` ile birlikte kullanılamaz). `--yes`, `--purge` için yazılan onayı atlar. `--ssh-port` ve `--key`, `add` ile aynıdır |

`add`, `repair`, `upgrade` ve `remove` bu terminalde SSH üzerinden çalışır. Kendi başlarına
daemon alt komutu değildirler ve `--remote` onlara uygulanmaz.

### `bot workflow`

Başarılı çalışmalardan iş akışlarını kaydeder, gözden geçirir, saklar ve yeniden oynatır (bkz.
[iş akışları](workflows.md)). JSON yazdırır.

| Alt komut | Anlamı |
| --- | --- |
| `bot workflow list --bot <BOT_ID>` | Bir botun kayıtlı iş akışlarını listeler |
| `bot workflow show <NAME> --bot <BOT_ID>` | Kayıtlı bir iş akışının her adımını ve parametresini gösterir |
| `bot workflow propose <RUN_ID>` | Tamamlanmış bir çalışmadan, gözden geçirmek için kaydedilmemiş bir iş akışı önerisi yazdırır |
| `bot workflow save <FILE>` | Gözden geçirilmiş bir öneri JSON dosyasını kaydeder (`--name <NAME>`, `--description <TEXT>`) |
| `bot workflow delete <NAME> --bot <BOT_ID>` | Kayıtlı bir iş akışını siler |
| `bot workflow run <NAME> --chat <CHAT_ID>` | Kayıtlı bir iş akışını var olan bir sohbette yeniden oynatır (`--params-stdin` stdin'den bir JSON parametre nesnesi okur) |

### `bot self-update`

Linux kurulumunu günceller. Yeniden başlatma ayrıdır, bu komut bir servisi yeniden başlatmaz.
Diğer işletim sistemlerinde, platformun yükleyicisini kullanmanı söyleyen bir not yazdırır ve 0 ile çıkar.

| Seçenek | Anlamı |
| --- | --- |
| `--check` | Mevcut sürümü indirmeden ya da kurmadan gösterir. `--rollback` ile birlikte kullanılamaz |
| `--version <TAG>` | Sürüm etiketi (baştaki `v` isteğe bağlıdır). `--rollback` ile birlikte kullanılamaz |
| `--rollback` | Daha önce kurulu olan sürüme geçer |
| `--prefix <PATH>` | Kurulum öneki (normalde kurulu çalıştırılabilir dosyadan çıkarılır) |

## Diğer ikili dosyalar

- `bot-node`: başka bir makineyi denetlemek için düğüm ajanı (deneysel, bkz.
  [SSS](faq.md)). Komutlar: `join <bot-pair:...> [--name]`, `run`, `devices`.
  Genel seçenekler: `--data-dir`, `--relay <default|none|URL>` (varsayılan `default`) ve
  `--display <X11>` (örneğin `:99`).
- `bot-desktop`: Windows masaüstü uygulaması.
- `bot-eval`: çevrimdışı ve canlı değerlendirme çalıştırıcısı (`crates/evals`, alt komutlar
  `run`, `compare`, `report`, `list`). [Prompt injection değerlendirme paketini](security.md)
  de içerir (`bot-eval run --tier 1 --filter inj- --repeat 3`, bkz.
  [`evals/baselines/README.md`](../../../evals/baselines/README.md)).

## HTTP API

CLI'ın ve masaüstü uygulamasının yaptığı her şey, daemon'un HTTP API'sinden geçer.
OpenAPI açıklaması [`docs/api/openapi.json`](../../api/openapi.json) dosyasındadır. CLI komutu olmayan
önemli yollar: politika (`/v1/policy`), kullanım ve maliyet (`/v1/usage`), çalışma denetimi
(`/v1/runs/{id}/cancel`, `/resume`, `/remember`), sağlayıcı yeniden yükleme, bot güncelleme/silme
ve zamanlama güncelleme.
