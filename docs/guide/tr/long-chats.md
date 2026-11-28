# Uzun sohbetler: bot konuyu nasıl kaybetmez

Başka dilde: [English](../long-chats.md).

Bilgisayarında ya da hep açık bir sunucuda yaşayan bir bot, bir sohbeti haftalarca açık tutabilir. Bir model aynı anda yalnızca belli miktarda okuyabildiği için Praxi **en yeni mesajları** kelimesi kelimesine tutar, **eski kısmı** ise kısa bir notla değiştirir: *"bu sohbette daha önce"*. Bu sayfa notun ne olduğunu, ne olmadığını ve nasıl kapatılacağını anlatır.

## Ne olur

Bir sohbette henüz hiçbir notun kapsamadığı yaklaşık 160'tan fazla mesaj (ya da yaklaşık 40 KB metin) birikince, botun modeli bir turdan sonra arka planda eski mesajlar için kısa bir not yazar. Bundan sonra bot **bu notu ve son mesajlarını** okur (en az son 40 mesaj ya da yaklaşık 16 KB). Sohbet yaklaşık aynı miktarda daha uzayınca not güncellenir: önceki not ve sıradaki eski mesajlar tek notta birleşir.

* **Hiçbir şey silinmez ya da değişmez.** Her mesaj sohbette, aramada ve dışa aktarmada durur. Yalnızca modele gönderilen şey değişir.
* **Ucuz ve sınırlı.** Bir özetleme en fazla yaklaşık 48 KB mesaj okur, en fazla yaklaşık 6 KB yazar. Yalnızca sohbet uzadıktan sonra, bir sohbet turundan sonra en çok iki kez çalışır ve aynı sohbet için asla aynı anda iki kez çalışmaz. Bir özetleme başarısız olursa (model yok, sınıra gelindi, kullanılamaz yanıt) o sohbet bir sonraki denemeden önce 30 dakika bekler ve senin sohbet turların etkilenmez. Her özetleme [çağrı defterinde](llm-call-ledger.md) `summary` amacıyla normal bir model çağrısıdır; bu yüzden [harcama sınırları](bot-spending-limits.md) geçerlidir. Denetim günlüğüne de kısa bir `chat.summary` kaydı düşer (sayılar ve mesaj kimlikleri, asla metin değil).
* **Hangi model.** Botun daha ucuz yardımcı modeli varsa o, yoksa normal modeli. Eski mesajlarda özel veri varsa (posta, açtığın dosyalar) yalnızca botun normal modeli kullanılır; o veriyi zaten görmüş olan model.
* **Düzenleme.** Sohbetteki herhangi bir mesajı düzenler ya da yeniden ürettirirsen not atılır, sohbet uzayınca yeniden yazılır. Sohbet değişirken süren bir özetleme çöpe gider.

## Not nedir, ne değildir

Not **alıntılanmış veridir**, asla senin sesin değildir:

* Bot onu açıkça etiketlenmiş *güvenilmeyen* bir blok olarak alır; sistem talimatı ya da senden gelen bir mesaj olarak değil. İçindeki talimatlara asla uymaması söylenir. Güncel isteğin her zaman notun altındaki mesajlardadır.
* Notun başlıklarını Praxi sabit yazar. **Senin sözlerini** Praxi, uygulamada yazdığın mesajlardan alıntılar (en yeni birkaçı, kısaltılmış); ne dediğini ya da ne istediğini model asla yazmaz. Model yalnızca kendi başlıkları altına kısa maddeler yazar: botun ne yaptığı ve bulduğu, nelerin açık kaldığı ve **dışarıdan gelenler** (web sayfaları, posta, dosyalar, başka botlar); bunlar doğrulanmamış diye işaretlenir. Eski mesajlarda dışarıdan gelen bir şey varsa not *UNTRUSTED* (güvenilmeyen) satırıyla başlar.
* Nottaki hiçbir şey senin yazdığın bir şey sayılmaz. Bir adresi ya da siteyi "sen yazdın" diye geçiren denetimler yalnızca bu sohbette senin yazdığın mesajlara bakar, nota asla bakmaz; yani bir web sayfası ya da posta nota bir emir sokup onu seninmiş gibi geçiremez.
* Not **yapıldığı şeyin işaretlerini devralır.** Eski mesajlar bir web sayfası ya da özel veri okumuş bir turdan geldiyse, notu okuyan tur da onu okumuş sayılır, aynı kaynaklarla; yani eski mesajlar hâlâ oradaymış gibi aynı onay soruları geçerlidir.
* Parolalar, anahtarlar, kart numaraları ve diğer gizli bilgiler, modelin not için okuduğu her şeyde (önceki not da dahil) ve yazdığı her şeyde, not saklanmadan önce maskelenir. Gizli bilgi maskeleyicisi olmadan (uygulama onu hep çalıştırır) hiç not yazılmaz.

## Kapat

**Ayarlar → Yapay zekâ ve hafıza → Uzun sohbetler**'i aç, bir bot seç ve **Uzun sohbetlerin eski mesajlarını özetle** düğmesini kapat. O zaman o bot eskisi gibi yalnızca en yeni mesajları görür (yaklaşık 256 mesaj ya da 64 KB) ve eski olanların dışarıda kaldığı kısa bir notla söylenir. Var olan not saklanır ama kullanılmaz; düğmeyi yeniden açınca tekrar kullanılır. Düğme botun `history.summary` profil alanıdır (kapatmak için `false`), bu yüzden profil düzenlemeleriyle ya da API ile de (`PATCH /v1/bots/{id}`) ayarlanabilir.

## Bilmekte fayda var

* Zamanlanmış görev notu okumaz (kendi önceki çalışmalarını da okumaz), eskiden olduğu gibi.
* Kodlama CLI motoruyla çalışan bot kendi oturum geçmişini tutar ve not almaz.
* Not, onu yazan model kadar iyidir. Önemli bir şeyi tekrar söyle ya da bottan önceki sohbeti aramasını iste: asıllar hep orada.
