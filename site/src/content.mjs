// All page text. Real translations; keep both locales in step (the test checks keys).
export const REPO = "https://github.com/emreocell/Praxi-Bot";
export const DOCS = `${REPO}/blob/main/docs`;
export const SITE = "https://emreocell.github.io/Praxi-Bot/";

export const shots = [
  { file: "approval-card.webp", w: 1600, h: 1122 },
  { file: "screen-permissions.webp", w: 1600, h: 1057 },
  { file: "screen-schedules.webp", w: 1600, h: 1057 },
  { file: "screen-live-view.webp", w: 1600, h: 1057 },
];

export const locales = {
  en: {
    lang: "en",
    title: "Praxi-Bot: a local-first AI agent you control",
    description:
      "Praxi-Bot is an open-source AI agent that runs on your own computer or your own server. You pick the model, every change asks you first, and web pages and emails are never treated as orders. Alpha.",
    skip: "Skip to content",
    nav: { features: "What it does", download: "Download", safe: "Safety", who: "Who it is for", help: "Contribute" },
    switchLabel: "Türkçe",
    switchHref: "tr/",
    badge: "Alpha: expect bugs",
    heroTitle: "An AI agent on your own computer. You stay in charge.",
    heroLead:
      "Praxi-Bot is an open-source AI agent that runs on your computer or on a server you own. It asks before it changes anything, and it treats web pages and emails as data, not as orders.",
    ctaDownload: "Download for Windows",
    ctaGithub: "View on GitHub",
    heroNote: "Free and open source (Apache-2.0). No Praxi account. Alpha software: things still change quickly.",
    featuresTitle: "What it does",
    features: [
      {
        title: "Ask first, then act",
        text: "By default, anything that changes files, runs commands, clicks in apps or sends messages shows an approval card: allow once, for this chat, always, or deny.",
        alt: "An approval card in the Praxi-Bot desktop app with the buttons Allow once, This chat, Always and Deny",
      },
      {
        title: "You decide what each bot may do",
        text: "Create bots, give each one a model and a permission level, and set spending limits. The rules are checked by code, not by the AI.",
        alt: "The permissions screen of the Praxi-Bot desktop app",
      },
      {
        title: "Jobs on a schedule",
        text: "Describe a repeating job in plain words or set it up with the builder. Your bots can work while you do something else.",
        alt: "The schedules screen of the Praxi-Bot desktop app",
      },
      {
        title: "Watch it work",
        text: "A live view shows what a bot does on its computer, on this machine or on an always-on Linux server, and a stop key ends a run.",
        alt: "The live view screen of the Praxi-Bot desktop app",
      },
    ],
    shotsNote: "Screenshots come from the desktop app running with demo data, not from a real account.",
    downloadTitle: "Download",
    downloadLead: "Windows installer, latest pre-release:",
    downloadBtn: "Download",
    downloadFallback: "Download from the releases page",
    releaseLink: "Release notes and all files",
    shaLabel: "SHA-256 of the installer:",
    shaHow: "Check it in PowerShell with Get-FileHash and compare.",
    smartscreen:
      "The installer is not code-signed yet, so Windows may show “Windows protected your PC”. After comparing the SHA-256, choose More info, then Run anyway.",
    linux: "Always-on Linux server? See the",
    linuxLink: "Linux host guide",
    privacy:
      "This page asks GitHub for the latest release when it loads, so GitHub sees your IP address, as with any GitHub link. Nothing else is loaded from other sites.",
    whoTitle: "Who it is for",
    who: [
      "People who want an AI assistant that works on their own computer and keeps chats and files there, without being a programmer.",
      "People who want to decide what the AI may touch, with clear approval cards instead of blind trust.",
      "People with a spare PC or a rented server who want bots to keep working when their main computer is off.",
    ],
    notFor:
      "Not for you yet if you need a stable, finished product: this is alpha, the interfaces change, and only the latest pre-release is supported.",
    safeTitle: "How it stays safe",
    safe: [
      {
        title: "Permissions are enforced by code",
        text: "The AI cannot talk its way past them. Reads are free; changes ask first. Money actions and a list of catastrophic commands always ask.",
      },
      {
        title: "Web pages and emails are data",
        text: "Text from the web, mail or tool output is never treated as an instruction. After a run has read such content, anything that could send data out asks first.",
      },
      {
        title: "Local by default",
        text: "The daemon listens on 127.0.0.1 only, there is no telemetry by design, and API keys go to your system keychain or an encrypted file.",
      },
    ],
    safeLink: "Read the full security model",
    safeHonest: "No software is perfectly safe. The security page also says what these protections do not cover.",
    helpTitle: "Help build it",
    helpText:
      "Praxi-Bot is built in the open. Small, well-described tasks are labelled for newcomers, and translations are welcome.",
    helpGfi: "Good first issues",
    helpContrib: "How to contribute",
    docsLink: "Documentation",
    footerLicense: "Apache-2.0 licensed.",
    footerNoTrack: "No tracking, no cookies, no third-party fonts.",
    ogAlt: "Praxi-Bot: a local-first AI agent you control",
  },
  tr: {
    lang: "tr",
    title: "Praxi-Bot: kontrolün sende olan, yerel öncelikli yapay zekâ ajanı",
    description:
      "Praxi-Bot, kendi bilgisayarında ya da kendi sunucunda çalışan açık kaynaklı bir yapay zekâ ajanıdır. Modeli sen seçersin, her değişiklik önce sana sorar, web sayfaları ve e-postalar asla emir sayılmaz. Alfa sürüm.",
    skip: "İçeriğe geç",
    nav: { features: "Ne yapar", download: "İndir", safe: "Güvenlik", who: "Kimler için", help: "Katkı ver" },
    switchLabel: "English",
    switchHref: "../",
    badge: "Alfa: hatalar olabilir",
    heroTitle: "Kendi bilgisayarında çalışan bir yapay zekâ ajanı. Kontrol sende.",
    heroLead:
      "Praxi-Bot, bilgisayarında ya da sahibi olduğun bir sunucuda çalışan açık kaynaklı bir yapay zekâ ajanıdır. Bir şeyi değiştirmeden önce sorar; web sayfalarını ve e-postaları emir değil, veri olarak görür.",
    ctaDownload: "Windows için indir",
    ctaGithub: "GitHub’da gör",
    heroNote: "Ücretsiz ve açık kaynak (Apache-2.0). Praxi hesabı yok. Alfa yazılım: işler hâlâ hızlı değişiyor.",
    featuresTitle: "Ne yapar",
    features: [
      {
        title: "Önce sorar, sonra yapar",
        text: "Varsayılan olarak dosya değiştiren, komut çalıştıran, uygulamalarda tıklayan ya da mesaj gönderen her işlem bir onay kartı gösterir: bir kez izin ver, bu sohbet için, her zaman ya da reddet.",
        alt: "Praxi-Bot masaüstü uygulamasında Bir kez izin ver, Bu sohbet, Her zaman ve Reddet düğmeleri olan bir onay kartı",
      },
      {
        title: "Her botun ne yapabileceğine sen karar verirsin",
        text: "Bot oluştur, her birine bir model ve bir izin düzeyi ver, harcama sınırları koy. Kuralları yapay zekâ değil, kod denetler.",
        alt: "Praxi-Bot masaüstü uygulamasının izinler ekranı",
      },
      {
        title: "Zamanlanmış işler",
        text: "Tekrarlayan bir işi düz cümlelerle anlat ya da oluşturucuyla kur. Sen başka şeyle uğraşırken botların çalışabilir.",
        alt: "Praxi-Bot masaüstü uygulamasının zamanlama ekranı",
      },
      {
        title: "Çalışırken izle",
        text: "Canlı görünüm, bir botun kendi bilgisayarında ne yaptığını gösterir: bu makinede ya da hep açık bir Linux sunucuda. Durdur tuşu çalışmayı bitirir.",
        alt: "Praxi-Bot masaüstü uygulamasının canlı görünüm ekranı",
      },
    ],
    shotsNote: "Ekran görüntüleri gerçek bir hesaptan değil, demo verisiyle çalışan masaüstü uygulamasından alındı.",
    downloadTitle: "İndir",
    downloadLead: "Windows yükleyicisi, en son ön sürüm:",
    downloadBtn: "İndir",
    downloadFallback: "Sürümler sayfasından indir",
    releaseLink: "Sürüm notları ve tüm dosyalar",
    shaLabel: "Yükleyicinin SHA-256 değeri:",
    shaHow: "PowerShell’de Get-FileHash ile hesaplayıp karşılaştırabilirsin.",
    smartscreen:
      "Yükleyici henüz kod imzalı değil; bu yüzden Windows “Windows bilgisayarınızı korudu” uyarısı gösterebilir. SHA-256 değerini karşılaştırdıktan sonra Daha fazla bilgi, ardından Yine de çalıştır’ı seç.",
    linux: "Hep açık bir Linux sunucu mu istiyorsun? Bak:",
    linuxLink: "Linux sunucu rehberi",
    privacy:
      "Bu sayfa yüklenirken GitHub’dan en son sürümü ister; yani her GitHub bağlantısında olduğu gibi GitHub IP adresini görür. Başka sitelerden hiçbir şey yüklenmez.",
    whoTitle: "Kimler için",
    who: [
      "Programcı olmadan, sohbetlerinin ve dosyalarının kendi bilgisayarında kaldığı bir yapay zekâ asistanı isteyenler.",
      "Yapay zekânın neye dokunabileceğine kör güvenle değil, açık onay kartlarıyla kendisi karar vermek isteyenler.",
      "Boşta bir bilgisayarı ya da kiralık bir sunucusu olup ana bilgisayarı kapalıyken de botların çalışmasını isteyenler.",
    ],
    notFor:
      "Kararlı, bitmiş bir ürün arıyorsan henüz sana göre değil: bu bir alfa sürüm, arayüzler değişiyor ve yalnızca en son ön sürüm destekleniyor.",
    safeTitle: "Nasıl güvende kalır",
    safe: [
      {
        title: "İzinleri kod uygular",
        text: "Yapay zekâ konuşarak bunları aşamaz. Okumak serbest; değiştirmek önce sorar. Para işlemleri ve yıkıcı komutların bir listesi her zaman sorar.",
      },
      {
        title: "Web sayfaları ve e-postalar veridir",
        text: "Webden, e-postadan ya da araç çıktısından gelen metin asla talimat sayılmaz. Bir çalışma böyle içerik okuduktan sonra, veriyi dışarı gönderebilecek her şey önce sorar.",
      },
      {
        title: "Varsayılan olarak yerel",
        text: "Servis yalnızca 127.0.0.1’i dinler, tasarım gereği telemetri yoktur ve API anahtarları sistem anahtarlığına ya da şifreli bir dosyaya gider.",
      },
    ],
    safeLink: "Güvenlik modelinin tamamını oku",
    safeHonest: "Hiçbir yazılım mükemmel güvenli değildir. Güvenlik sayfası bu korumaların neleri kapsamadığını da söyler.",
    helpTitle: "Birlikte geliştirelim",
    helpText:
      "Praxi-Bot açıkta geliştiriliyor. Yeni gelenler için küçük, iyi anlatılmış görevler etiketli; çeviriler de memnuniyetle karşılanır.",
    helpGfi: "İlk katkı için konular",
    helpContrib: "Nasıl katkı verilir",
    docsLink: "Belgeler",
    footerLicense: "Apache-2.0 lisanslı.",
    footerNoTrack: "İzleme yok, çerez yok, üçüncü taraf yazı tipi yok.",
    ogAlt: "Praxi-Bot: kontrolün sende olan, yerel öncelikli yapay zekâ ajanı",
  },
};
