# Klavye kısayolları

Praxi masaüstü uygulaması, fareye uzanmadan sohbetler arasında gezinmen, bot çalışmalarını yönetmen, pencereleri açman ve mesaj yazman için klavye kısayolları sunar.

Bu listeyi uygulamada istediğin zaman da görebilirsin: hiçbir metin alanı odakta değilken **`?`** tuşuna bas, profil menüsünden **Yardım**'ı seç ya da yazı alanına `/help` yaz. Bkz. [Klavye kısayolları ve “Yenilikler”](chatting-and-approvals.md).

> **Komut satırı kullananlar:** Klavye kısayolları masaüstü uygulaması içindir. Praxi ile bir terminalden konuşuyorsan komut satırı karşılıkları (`bot chat`, `bot approve`, `bot status` vb.) için [CLI başvurusu](cli-reference.md) sayfasına bak.

---

## Genel kısayollar

Genel kısayollar masaüstü penceresinin her yerinde çalışır.

| Kısayol | Ne yapar |
| --- | --- |
| `Ctrl/Cmd+K` | Komut paleti |
| `?` | Bu listeyi göster (hiçbir metin alanı odakta değilken) |
| `Ctrl/Cmd+,` | Ayarları aç |
| `Ctrl+Alt+Esc` *(varsayılan, değiştirilebilir)* | Tüm bot çalışmalarını durdur |
| `Ctrl/Cmd+F` | Bu sohbette ara |
| `Enter / Shift+Enter, Esc` | Arama çubuğunda sonraki / önceki eşleşme, çubuğu kapat |
| `Esc` | Yan paneli kapat |
| `Shift+Esc` | Canlı görünümün klavye denetiminden çık |

---

## Mesaj yazma alanı

Bu kısayollar, sohbetin yazı alanı odaktayken çalışır.

| Kısayol | Ne yapar |
| --- | --- |
| `Enter` *(varsayılan)* | Mesajı gönder |
| `Shift+Enter` *(varsayılan)* | Yeni satır |
| `Up / Down` | Bu sohbetin önceki mesajları (kutu boşken ya da ilk satırdayken; `Esc` taslağını geri getirir) |
| `Up / Down` | Eğik çizgi / bahsetme menüsünde gez |
| `Enter / Tab` | Vurgulanan öğeyi seç |
| `Esc` | Menüyü kapat |

### Ayarlanabilir yazma ve durdurma kısayolları

Üç kısayol (`@send`, `@newline` ve `@stop`) ayarlarına bağlıdır:

- **Gönderme ve yeni satır (`@send`, `@newline`)**: **Mesaj gönder** ayarı (Ayarlar → Genel) sohbet mesajını hangi tuşun göndereceğini belirler.
  - **Enter (varsayılan)**: `Enter` mesajı gönderir, `Shift+Enter` yeni satır ekler.
  - **Ctrl+Enter**: Shift'e basmadan çok satırlı istemler yazmayı tercih ediyorsan `Ctrl+Enter`'ı seç. O zaman `Ctrl/Cmd+Enter` mesajı gönderir, `Enter` ise yeni satır ekler.
- **Durdurma kısayolu (`@stop`)**: **Tümünü durdur kısayolu** ayarı, etkin bot çalışmalarını iptal eden tuşu değiştirir. Varsayılan `Ctrl+Alt+Esc`'tir. Hazır seçenekler `Ctrl+Alt+Esc`, `Ctrl+Shift+Esc` ve `Alt+Shift+S`'tir, istersen kendi kısayolunu da kaydedebilirsin. Windows masaüstü uygulamasında başka bir uygulama odaktayken de çalışır. Diğer sistemlerde önce Praxi'yi odağa al.

Mesaj yazma, ek dosyalar ve çalışma yönetimi hakkında daha fazlası için [Sohbet ve onaylar](chatting-and-approvals.md) sayfasına bak.

---

## Eğik çizgi komutları

Tuş kombinasyonlarının yanında yazı alanında hazır eğik çizgi komutları da vardır. Boş bir yazı alanının başına `/` yazarsan mevcut komutların olduğu bir tamamlama menüsü açılır:

- **Daemon komutları**:
  - `/stop`: Şu an etkin olan çalışmayı durdurur.
  - `/new`: Mevcut botla yepyeni bir sohbet başlatır.
  - `/workflow <name> [key=value ...]`: Kayıtlı bir iş akışını dil modelini çağırmadan yeniden oynatır (bkz. [İş akışları](workflows.md)).
- **Uygulama içi komutlar**:
  - `/export`: Açık sohbeti Markdown (`.md`) dosyası olarak indirir (bkz. [Mesajları kopyalama ve Markdown dışa aktarma](chatting-and-approvals.md)).
  - `/help`: Klavye kısayolları penceresini açar.
  - `/settings`: Ayarlar ekranını açar.
  - `/models`: Yapay zekâ modeli ayarlarına atlar.
  - `/apps`: Bağlı uygulamalar görünümünü açar.
  - `/schedules`: Zamanlanmış görevler görünümünü açar.

Önerilerde gezinmek için `Up` / `Down`, birini seçmek için `Enter` ya da `Tab` kullan.

---

## Pencereler

Bu kısayollar, açılır pencereler, açılır menüler ya da komut paleti açıkken çalışır.

| Kısayol | Ne yapar |
| --- | --- |
| `Esc` | Pencereyi kapat |
| `Tab / Shift+Tab` | Pencerenin içinde odağı taşı |
| `Up / Down, Enter` | Komut paleti: gez ve çalıştır |
| `Up / Down, Home / End` | Açılır menülerde gez |
| `Esc` | Açılır menüyü kapat |
| `Esc` | Sesli aramayı bitir ([Sesli arama kipi](voice-call.md)) |
| `Space / Enter (basılı tut)` | Bas-konuş aramasında konuşmak için basılı tut ([Sesli arama kipi](voice-call.md)) |

---

## İlgili sayfalar

- [Erişilebilirlik](accessibility.md): klavyeyle gezinme sırası, atlama bağlantıları, ekran okuyucu bölgeleri ve canlı alanlar, odak halkaları, yüksek kontrastlı temalar ve azaltılmış hareket.
- [Sohbet ve onaylar](chatting-and-approvals.md): turları, yönlendirmeyi, araç çağrılarını onaylamayı ve sohbetleri yönetmeyi öğren.
- [CLI başvurusu](cli-reference.md): Praxi'yi komut satırından kullanmak için komut başvurusu.
- [İş akışları](workflows.md): `/workflow` ile otomatik diziler oluştur ve çalıştır.
