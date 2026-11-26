# Linux sunucu paketlerini yayımlama

Bir `v*` etiketi göndermek *Linux Server Packages* iş akışını çalıştırır: yerel x86_64 ve aarch64 derlemeleri, derleme kökeni (attestation) ve bağımsız bir doğrulama. Sonrasında olanlar etikete bağlıdır.

Bir etiket göndermeden önce paket sürümlerini denetle ve gruplanmış sürüm notlarının önizlemesini
[bakımcı yayın kılavuzundan](../../contributing/releasing.md) gör.

## Ön sürüm etiketleri (`v0.1.0-alpha.1`, `-beta.N`, `-rc.N`: herhangi bir sonek)
Tek bir bakımcı bunları yayımlayabilir. Çalışma iki mimariyi de derler, kökenini kanıtlar, doğrular ve dosyaları o etiketin GitHub ön sürümüne kendisi ekler. İncelemeci ya da ortam gerekmez. Yine de bu depoda çalışmayı (fork ya da çekme isteği olmaz), etiketin kendi ref değerini ya da `main` üzerinde elle çalıştırmayı şart koşar ve var olan bir sürüm dosyasının üzerine asla yazmaz. Var olan bir ön sürüm etiketinin dosyalarını (yeniden) yayımlamak için: `gh workflow run linux-release.yml --ref main -f tag=v0.1.0-alpha.1 -f dry_run=false`.
Koruma, son etiketlere göre daha düşüktür: yayımı kimse onaylamaz. `main` üzerinde elle çalıştırma, `main` derlemesi olarak kanıtlanır ve `install-linux.sh` ile yönetilen güncelleyici bunu kabul etmez: yalnızca kaynak ref'i sürüm etiketinin kendisi olan kanıtı kabul ederler, son sürümde de ön sürümde de (`main` üzerindeki eski bir derleme, yeni bir etiketin düzelttiği açığı taşıyabilir). Bu yüzden `main` üzerinde elle çalıştırmayla yayımlanan dosyalar kurulamaz; kurulabilir dosyalar için etiketi gönder.

## Son etiketler (`v1.0.0`, sonek yok)
Depoda `refs/tags/v*` için bir etiket kural seti yoksa ya da `release` ortamında (Settings > Environments) etiketi gönderen kişiden başka, “Prevent self-review” açık bir zorunlu incelemeci yoksa çalışma “Attest protected release tag” adımında başarısız olur. Yayım, etiketin kendisi üzerinde, `dry_run` kapalıyken elle yapılan bir çalışmadır.

## Sürüm notları ve dosyalar

Yeni bir sürümde iş akışı GitHub'ın ürettiği sürüm notlarını kullanır ve doğrulanmış Linux kurulum yönergelerini ekler. Var olan bir sürümde, Windows yükleyicisiyle ilk kez oluşturulmuş bir ön sürüm dahil, doğrulanmış Linux dosyalarını ekler ve sürüm gövdesini korur. Dosya adı çakışması yayımı durdurur.

[Linux deneme çalışması](remote-host.md) var olan bir etiket üzerinde derlemeyi, kök kanıtlamayı ve doğrulamayı dener. [Yerel not provası](../../contributing/releasing.md) etiket henüz yokken çalışır.

## Sunucu hangi etiketi kurar
`bot host add`, masaüstü uygulamasının derlendiği sürüm etiketini kurar (`PRAXI_RELEASE_TAG`, Windows yükleyici iş akışı tarafından ayarlanır). `install-linux.sh --version latest`, GitHub'ın “latest” değeri boşken ön sürümler dahil en yeni sürüme döner (sabitlenmiş deponun API'si üzerinden).
