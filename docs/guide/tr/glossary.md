# Sözlük

Praxi'de, kullanım kılavuzunda ve kodda geçen temel terimlerin alfabetik listesi.

- **Onay** — Bir bot izin gerektiren bir işlem yapmak istediğinde gösterilen onay kartı. Her birini sen onaylarsın ya da reddedersin. Bkz. [Sohbet ve onaylar](../chatting-and-approvals.md). Kod: `crates/permission-engine/`.
- **Bot** — Kendi kişiliği, modeli, araçları ve izin kuralları olan adlandırılmış bir yardımcı. Her botun kendi sohbetleri ve izinleri vardır. Bkz. [Botlar ve profiller](../bots-and-profiles.md).
- **Daemon** — Ajan döngüsünü çalıştıran, verileri SQLite'ta saklayan, izinleri uygulayan ve zamanlamaları tetikleyen arka plan servisi (`bot serve`). Yalnızca `127.0.0.1` adresini dinler. Bkz. [CLI başvurusu](../cli-reference.md).
- **Çıkış kapısı (egress gate)** — Güvenilmeyen içerik okumuş bir çalışmanın verileri bilgisayarın dışına göndermeden önce onay istemesini sağlayan kural. Bkz. [Güvenlik modeli](../security.md). Kod: `crates/permission-engine/src/egress.rs`.
- **Ana makine (host)** — Daemon'u ana makine kipinde çalıştıran, sürekli açık bir Linux sunucusu; masaüstü kapalıyken de botlar çalışmaya devam eder. Bkz. [Uzak ana makinede çalıştırma](../remote-host.md).
- **Yasak listesi (never-list)** — Diskleri biçimlendirmek, sistem klasörlerini silmek gibi yıkıcı kabuk komutlarının sabit listesi. Politika ne olursa olsun bu komutlar her zaman onay ister. Bkz. [Güvenlik modeli](../security.md). Kod: `crates/permission-engine/src/never_list.rs`.
- **Node-agent** — Bir botun başka bir bilgisayarı yönetmesini sağlayan deneysel yardımcı (`bot-node`). Bkz. [SSS](faq.md). Kod: `apps/node-agent/`.
- **İzin hazır ayarı (preset)** — Bir bot için hazır varsayılan izin kuralları: `read-only`, `read-and-draft`, `approve-destructive` (varsayılan) ve `full`. Bkz. [Sohbet ve onaylar](../chatting-and-approvals.md). Kod: `apps/cli/src/presets.rs`, `crates/permission-engine/src/modes.rs`.
- **Çalışma (run)** — Tek bir iş turu: bot mesajı alır, düşünür, araçları çağırır ve yanıtını akıtır. "Tur" da denir. Bkz. [Sohbet ve onaylar](../chatting-and-approvals.md).
- **Zamanlama** — Bir botun kendiliğinden çalışma başlatmasını sağlayan zamanlı tetikleyici (örneğin hafta içi her gün saat 9'da). Bkz. [Zamanlanmış görevler](../scheduled-tasks.md).
- **Beceri (skill)** — Bir botun zor bir görevi, sayfayı ya da uygulamayı nasıl ele alacağını öğreten kısa bir Markdown kılavuzu. Kod değildir; bot, ilgili olduğunda okuyacağı yönergelerdir. Bkz. [Beceriler](../skills.md).
- **İş akışı (workflow)** — Model çağırmadan yeniden oynatılabilen, kaydedilmiş ve parametreli bir araç çağrıları dizisi. Bkz. [İş akışları](../workflows.md).
