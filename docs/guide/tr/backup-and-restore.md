# Eski şifresiz yedekleri geri yükleme

Güncel sürümler **şifreli** yedek oluşturur; bkz. [Şifreli yedekleme ve geri yükleme](../backup.md).
Eski sürümler `bot backup <DOSYA>` ile şifresiz arşivler yazıyordu. `bot restore` bunları hâlâ okur:

```sh
bot restore ./praxi-backup.tar            # önce daemon'u durdur
bot restore ./praxi-backup.tar --replace  # mevcut botları ve sohbetleri değiştir
```

- Veri klasörünü bir daemon kullanırken ya da servis portu yanıt verirken çalışmayı reddeder.
- Zaten botları ya da sohbetleri olan bir kurulum için `--replace` gerekir; değiştirilen eski durum,
  veri klasörünün içindeki bir `backup-*` klasöründe saklanır.
- `--allow-policy-relaxation`, izin politikası mevcut olandan daha gevşek olan bir yedeği kabul eder.
- Şifresiz arşivlerde API anahtarı bulunmaz: anahtarları yeniden gir (`bot secrets set`, `bot setup`).
- `--json` ile başarıda tek bir JSON nesnesi yazılır: `file`, `files`, `app_version`,
  `previous_state`. Hatalar stderr'e metin olarak gider.

Eski `.tar` yedeklerini hassas saymanı ve `bot backup create` ile yeniden oluşturmayı düşünmeni öneririz.
