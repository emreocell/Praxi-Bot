//! Everything the bot says in Discord itself, in English, Turkish and German,
//! in plain words. Commands and button ids never change with the language.

/// Reply language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// English.
    #[default]
    En,
    /// Türkçe.
    Tr,
    /// Deutsch.
    De,
}

impl Lang {
    /// `en`, `tr` or `de` (also `en-US`, `tr_TR`, `de-AT`...).
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        let lower = code.trim().to_ascii_lowercase();
        match lower.split(['-', '_']).next()? {
            "en" => Some(Self::En),
            "tr" => Some(Self::Tr),
            "de" => Some(Self::De),
            _ => None,
        }
    }

    /// The code stored in the state file.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Tr => "tr",
            Self::De => "de",
        }
    }
}

/// A line the bot can say. Arguments are plain data (already safe to show).
#[derive(Debug, Clone, Copy)]
pub enum Msg<'a> {
    /// A credential request is waiting in the desktop app.
    SecretRequestNotice,
    /// An MCP server's form is waiting in the desktop app.
    ElicitationNotice,
    /// A stranger wrote to the bot.
    NotPaired,
    /// Pairing worked.
    Paired,
    /// Wrong pairing code.
    PairInvalid,
    /// The pairing code ran out.
    PairExpired,
    /// A pairing code was posted in a server channel.
    PairInDm,
    /// This server channel is now allowed.
    Allowed,
    /// This server channel is no longer allowed.
    Disallowed,
    /// `!allow` was used in a private chat.
    AllowInServer,
    /// The daemon has no bots yet.
    NoBots,
    /// Start of the bot list.
    BotsHeading,
    /// Now talking to this bot.
    UseOk(&'a str),
    /// No bot with that name.
    UseUnknown,
    /// Which bot is in use here.
    UseUsage,
    /// Status line.
    Status { running: usize, pending: usize },
    /// Language changed.
    LangSet,
    /// Language usage.
    LangUsage,
    /// Help text.
    Help,
    /// Message too long.
    TooLong,
    /// A secret was cut out of the owner's message (`whole`: nothing was left to pass on).
    SecretRemoved { whole: bool },
    /// Too many messages too fast.
    TooFast,
    /// Message waits behind a running turn.
    Queued,
    /// The turn was stopped.
    RunStopped,
    /// The turn ended without text.
    RunFinished,
    /// The turn failed.
    RunFailed(&'a str),
    /// The daemon reported an error during the turn.
    RunError(&'a str),
    /// The connection to the daemon dropped.
    StreamLost,
    /// Too many turns at once.
    TooManyRuns,
    /// Attachments are not opened.
    AttachmentsNotOpened(&'a str),
    /// Something failed while handling the message.
    Failed(&'a str),
    /// Approval card heading.
    ApprovalHeading(&'a str),
    /// Approval must be done on the computer.
    ApprovalDesktopOnly(&'a str),
    /// Details hidden because they contained a secret.
    ApprovalMasked(&'a str),
    /// Details could not be shown.
    ApprovalDetailsUnavailable(&'a str),
    /// The request is too long to review here.
    ApprovalTooLarge(&'a str),
    /// Sending the card failed halfway.
    ApprovalIncomplete,
    /// Approve button.
    ButtonApprove,
    /// Deny button.
    ButtonDeny,
    /// Card result: approved.
    Approved,
    /// Card result: denied.
    Denied,
    /// Card is old or already used.
    UnknownOrExpired,
    /// Someone else pressed the button.
    NotYours,
    /// A question was answered.
    Answered,
}

impl Msg<'_> {
    /// The text in `lang`.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn render(self, lang: Lang) -> String {
        use Lang::{De, En, Tr};
        match (self, lang) {
            (Msg::SecretRequestNotice, En) => "A credential request is waiting. Open Praxi on your computer to answer. Keep credentials out of this chat.".into(),
            (Msg::SecretRequestNotice, Tr) => "Yanıtınızı bekleyen bir gizli bilgi isteği var. Yanıtlamak için bilgisayarınızda Praxi’yi açın. Gizli bilgilerinizi bu sohbette paylaşmayın.".into(),
            (Msg::SecretRequestNotice, De) => "Eine Anfrage nach Zugangsdaten wartet. Öffnen Sie Praxi am Computer, um zu antworten. Teilen Sie Zugangsdaten nicht in diesem Chat.".into(),
            (Msg::ElicitationNotice, En) => "An MCP server is asking for your input. Open Praxi on your computer to answer or decline. Do not answer it in this chat.".into(),
            (Msg::ElicitationNotice, Tr) => "Bir MCP sunucusu sizden bilgi istiyor. Yanıtlamak ya da reddetmek için bilgisayarınızda Praxi’yi açın. Bu sohbette yanıtlamayın.".into(),
            (Msg::ElicitationNotice, De) => "Ein MCP-Server bittet um Ihre Eingabe. Öffnen Sie Praxi am Computer, um zu antworten oder abzulehnen. Antworten Sie nicht in diesem Chat.".into(),
            (Msg::NotPaired, En) => "This bot is private. If it is yours, create a pairing code on your computer and send `!pair 123456` here in a private message.".into(),
            (Msg::NotPaired, Tr) => "Bu bot özeldir. Sizinse, bilgisayarınızda bir eşleştirme kodu oluşturun ve burada özel mesajla `!pair 123456` yazın.".into(),
            (Msg::NotPaired, De) => "Dieser Bot ist privat. Wenn es Ihrer ist, erzeugen Sie am Computer einen Kopplungscode und senden Sie hier privat `!pair 123456`.".into(),

            (Msg::Paired, En) => "Paired. Only your Discord account can talk to this bot now. Send `!help` to see what you can do.".into(),
            (Msg::Paired, Tr) => "Eşleştirildi. Artık bu botla yalnızca sizin Discord hesabınız konuşabilir. Yapabileceklerinizi görmek için `!help` yazın.".into(),
            (Msg::Paired, De) => "Gekoppelt. Nur Ihr Discord-Konto kann jetzt mit diesem Bot sprechen. Senden Sie `!help`, um zu sehen, was möglich ist.".into(),

            (Msg::PairInvalid, En) => "That code is not right. Check the code on your computer.".into(),
            (Msg::PairInvalid, Tr) => "Bu kod doğru değil. Bilgisayarınızdaki kodu kontrol edin.".into(),
            (Msg::PairInvalid, De) => "Dieser Code stimmt nicht. Prüfen Sie den Code am Computer.".into(),

            (Msg::PairExpired, En) => "That code has run out. Create a new one on your computer.".into(),
            (Msg::PairExpired, Tr) => "Bu kodun süresi doldu. Bilgisayarınızda yenisini oluşturun.".into(),
            (Msg::PairExpired, De) => "Dieser Code ist abgelaufen. Erzeugen Sie am Computer einen neuen.".into(),

            (Msg::PairInDm, En) => "Pairing only works in a private message with the bot. Never post a code in a server: create a new one and send it privately.".into(),
            (Msg::PairInDm, Tr) => "Eşleştirme yalnızca bot ile özel mesajda çalışır. Kodu asla bir sunucuda paylaşmayın: yeni bir kod oluşturup özel olarak gönderin.".into(),
            (Msg::PairInDm, De) => "Die Kopplung funktioniert nur in einer privaten Nachricht an den Bot. Posten Sie nie einen Code in einem Server: erzeugen Sie einen neuen und senden Sie ihn privat.".into(),

            (Msg::Allowed, En) => "OK. The bot will answer you in this channel, and every message you write here goes to the bot. Nobody else in the server can use it. A channel just for you works best.".into(),
            (Msg::Allowed, Tr) => "Tamam. Bot bu kanalda size cevap verecek ve burada yazdığınız her mesaj bota gider. Sunucudaki başka hiç kimse kullanamaz. Yalnızca size ait bir kanal en iyisidir.".into(),
            (Msg::Allowed, De) => "OK. Der Bot antwortet Ihnen in diesem Kanal, und jede Nachricht von Ihnen hier geht an den Bot. Niemand sonst im Server kann ihn nutzen. Ein Kanal nur für Sie ist am besten.".into(),

            (Msg::Disallowed, En) => "OK. The bot will no longer answer in this channel.".into(),
            (Msg::Disallowed, Tr) => "Tamam. Bot artık bu kanalda cevap vermeyecek.".into(),
            (Msg::Disallowed, De) => "OK. Der Bot antwortet in diesem Kanal nicht mehr.".into(),

            (Msg::AllowInServer, En) => "Use `!allow` in the server channel you want to use. In this private chat the bot already answers you.".into(),
            (Msg::AllowInServer, Tr) => "`!allow` komutunu kullanmak istediğiniz sunucu kanalında yazın. Bu özel sohbette bot zaten size cevap veriyor.".into(),
            (Msg::AllowInServer, De) => "Senden Sie `!allow` in dem Server-Kanal, den Sie nutzen möchten. In diesem privaten Chat antwortet der Bot Ihnen ohnehin.".into(),

            (Msg::NoBots, En) => "There is no bot on your computer yet. Create one in the app first.".into(),
            (Msg::NoBots, Tr) => "Bilgisayarınızda henüz bot yok. Önce uygulamada bir bot oluşturun.".into(),
            (Msg::NoBots, De) => "Auf Ihrem Computer gibt es noch keinen Bot. Erstellen Sie zuerst einen in der App.".into(),

            (Msg::BotsHeading, En) => "Your bots (choose one with `!use <name>`):".into(),
            (Msg::BotsHeading, Tr) => "Botlarınız (`!use <ad>` ile seçin):".into(),
            (Msg::BotsHeading, De) => "Ihre Bots (wählen mit `!use <Name>`):".into(),

            (Msg::UseOk(name), En) => format!("Now talking to {name} in this chat."),
            (Msg::UseOk(name), Tr) => format!("Bu sohbette artık {name} ile konuşuyorsunuz."),
            (Msg::UseOk(name), De) => format!("In diesem Chat sprechen Sie jetzt mit {name}."),

            (Msg::UseUnknown, En) => "No bot has that name. Send `!bots` to see the list.".into(),
            (Msg::UseUnknown, Tr) => "Bu adda bir bot yok. Listeyi görmek için `!bots` yazın.".into(),
            (Msg::UseUnknown, De) => "Es gibt keinen Bot mit diesem Namen. Senden Sie `!bots` für die Liste.".into(),

            (Msg::UseUsage, En) => "Say which bot: `!use <name>`.".into(),
            (Msg::UseUsage, Tr) => "Hangi bot olduğunu yazın: `!use <ad>`.".into(),
            (Msg::UseUsage, De) => "Nennen Sie den Bot: `!use <Name>`.".into(),

            (Msg::Status { running, pending }, En) => format!("Connected to your computer. Running now: {running}. Waiting for your OK: {pending}."),
            (Msg::Status { running, pending }, Tr) => format!("Bilgisayarınıza bağlı. Şu an çalışan: {running}. Onayınızı bekleyen: {pending}."),
            (Msg::Status { running, pending }, De) => format!("Mit Ihrem Computer verbunden. Läuft gerade: {running}. Wartet auf Ihr OK: {pending}."),

            (Msg::LangSet, En) => "Language set to English.".into(),
            (Msg::LangSet, Tr) => "Dil Türkçe olarak ayarlandı.".into(),
            (Msg::LangSet, De) => "Sprache auf Deutsch gestellt.".into(),

            (Msg::LangUsage, En) => "Use `!lang en`, `!lang tr` or `!lang de`.".into(),
            (Msg::LangUsage, Tr) => "`!lang en`, `!lang tr` veya `!lang de` yazın.".into(),
            (Msg::LangUsage, De) => "Nutzen Sie `!lang en`, `!lang tr` oder `!lang de`.".into(),

            (Msg::Help, En) => "Write normally to talk to your bot.\n`!bots` list bots · `!use <name>` choose one · `!stop` stop the current task · `!status` · `!lang en|tr|de` · `!allow` / `!disallow` use this server channel (owner only).".into(),
            (Msg::Help, Tr) => "Botunuzla konuşmak için normal yazın.\n`!bots` botları listele · `!use <ad>` birini seç · `!stop` görevi durdur · `!status` · `!lang en|tr|de` · `!allow` / `!disallow` bu sunucu kanalını kullan (yalnızca sahibi).".into(),
            (Msg::Help, De) => "Schreiben Sie normal, um mit Ihrem Bot zu sprechen.\n`!bots` Bots auflisten · `!use <Name>` einen wählen · `!stop` Aufgabe stoppen · `!status` · `!lang en|tr|de` · `!allow` / `!disallow` diesen Server-Kanal nutzen (nur Besitzer).".into(),

            (Msg::TooLong, En) => "That message is too long. Please send a shorter one.".into(),
            (Msg::TooLong, Tr) => "Mesaj çok uzun. Lütfen daha kısa bir mesaj gönderin.".into(),
            (Msg::TooLong, De) => "Die Nachricht ist zu lang. Bitte senden Sie eine kürzere.".into(),

            (Msg::SecretRemoved { whole }, En) => format!(
                "🔒 That message looks like a secret (a key, token, password or card number), so I {}. Discord keeps the original in this chat: please delete it. To give me a secret safely, store it in the vault in the desktop app.",
                if whole { "did not pass it on" } else { "cut that part out and passed on the rest" }
            ),
            (Msg::SecretRemoved { whole }, Tr) => format!(
                "🔒 Bu mesaj gizli bilgiye benziyor (anahtar, jeton, parola ya da kart numarası), o yüzden {}. Discord asıl mesajı bu sohbette tutuyor: lütfen sil. Gizli bir bilgiyi bana güvenle vermek için masaüstü uygulamasındaki kasaya kaydet.",
                if whole { "iletmedim" } else { "o kısmı çıkarıp mesajın geri kalanını ilettim" }
            ),
            (Msg::SecretRemoved { whole }, De) => format!(
                "🔒 Diese Nachricht sieht nach einem Geheimnis aus (Schlüssel, Token, Passwort oder Kartennummer), deshalb habe ich sie {}. Discord behält das Original in diesem Chat: bitte löschen Sie es. Um mir ein Geheimnis sicher zu geben, speichern Sie es im Tresor der Desktop-App.",
                if whole { "nicht weitergegeben" } else { "an dieser Stelle gekürzt und den Rest weitergegeben" }
            ),

            (Msg::TooFast, En) => "Too many messages at once. Please wait a moment.".into(),
            (Msg::TooFast, Tr) => "Aynı anda çok fazla mesaj. Lütfen biraz bekleyin.".into(),
            (Msg::TooFast, De) => "Zu viele Nachrichten auf einmal. Bitte warten Sie einen Moment.".into(),

            (Msg::Queued, En) => "Noted. I will start this after the current task.".into(),
            (Msg::Queued, Tr) => "Not aldım. Bunu mevcut görevden sonra başlatacağım.".into(),
            (Msg::Queued, De) => "Notiert. Ich starte das nach der aktuellen Aufgabe.".into(),

            (Msg::RunStopped, En) => "Stopped.".into(),
            (Msg::RunStopped, Tr) => "Durduruldu.".into(),
            (Msg::RunStopped, De) => "Gestoppt.".into(),

            (Msg::RunFinished, En) => "Done.".into(),
            (Msg::RunFinished, Tr) => "Bitti.".into(),
            (Msg::RunFinished, De) => "Erledigt.".into(),

            (Msg::RunFailed(e), En) => format!("The task failed: {e}"),
            (Msg::RunFailed(e), Tr) => format!("Görev başarısız oldu: {e}"),
            (Msg::RunFailed(e), De) => format!("Die Aufgabe ist fehlgeschlagen: {e}"),

            (Msg::RunError(e), Tr) => format!("Sorun: {e}"),
            (Msg::RunError(e), En | De) => format!("Problem: {e}"),

            (Msg::StreamLost, En) => "I lost the connection to your computer while working. Check the app.".into(),
            (Msg::StreamLost, Tr) => "Çalışırken bilgisayarınızla bağlantı koptu. Uygulamaya bakın.".into(),
            (Msg::StreamLost, De) => "Während der Arbeit ging die Verbindung zu Ihrem Computer verloren. Schauen Sie in die App.".into(),

            (Msg::TooManyRuns, En) => "Too many tasks are running. Try again in a moment.".into(),
            (Msg::TooManyRuns, Tr) => "Çok fazla görev çalışıyor. Birazdan tekrar deneyin.".into(),
            (Msg::TooManyRuns, De) => "Es laufen zu viele Aufgaben. Versuchen Sie es gleich noch einmal.".into(),

            (Msg::AttachmentsNotOpened(l), En) => format!("I cannot open attachments here yet, so I did not read them. {l}"),
            (Msg::AttachmentsNotOpened(l), Tr) => format!("Ekleri burada henüz açamıyorum, bu yüzden okumadım. {l}"),
            (Msg::AttachmentsNotOpened(l), De) => format!("Anhänge kann ich hier noch nicht öffnen, deshalb habe ich sie nicht gelesen. {l}"),

            (Msg::Failed(e), En) => format!("That did not work: {e}"),
            (Msg::Failed(e), Tr) => format!("Bu işe yaramadı: {e}"),
            (Msg::Failed(e), De) => format!("Das hat nicht geklappt: {e}"),

            (Msg::ApprovalHeading(t), En) => format!("🔐 Your OK is needed: {t}"),
            (Msg::ApprovalHeading(t), Tr) => format!("🔐 Onayınız gerekiyor: {t}"),
            (Msg::ApprovalHeading(t), De) => format!("🔐 Ihr OK wird gebraucht: {t}"),

            (Msg::ApprovalDesktopOnly(t), En) => format!("🔐 \"{t}\" needs your OK, and this kind of request can only be approved in the app on your computer."),
            (Msg::ApprovalDesktopOnly(t), Tr) => format!("🔐 \"{t}\" için onayınız gerekiyor ve bu tür bir istek yalnızca bilgisayarınızdaki uygulamadan onaylanabilir."),
            (Msg::ApprovalDesktopOnly(t), De) => format!("🔐 \"{t}\" braucht Ihr OK, und diese Art von Anfrage kann nur in der App auf Ihrem Computer bestätigt werden."),

            (Msg::ApprovalMasked(t), En) => format!("🔐 \"{t}\" needs your OK, but its details contain something secret, so they are not shown here. Decide in the app on your computer."),
            (Msg::ApprovalMasked(t), Tr) => format!("🔐 \"{t}\" için onayınız gerekiyor ama ayrıntılarında gizli bir şey var, bu yüzden burada gösterilmiyor. Bilgisayarınızdaki uygulamada karar verin."),
            (Msg::ApprovalMasked(t), De) => format!("🔐 \"{t}\" braucht Ihr OK, aber die Details enthalten etwas Geheimes und werden hier nicht gezeigt. Entscheiden Sie in der App auf Ihrem Computer."),

            (Msg::ApprovalDetailsUnavailable(t), En) => format!("🔐 \"{t}\" needs your OK, but I cannot show all its details here. Decide in the app on your computer."),
            (Msg::ApprovalDetailsUnavailable(t), Tr) => format!("🔐 \"{t}\" için onayınız gerekiyor ama tüm ayrıntıları burada gösteremiyorum. Bilgisayarınızdaki uygulamada karar verin."),
            (Msg::ApprovalDetailsUnavailable(t), De) => format!("🔐 \"{t}\" braucht Ihr OK, aber ich kann nicht alle Details hier zeigen. Entscheiden Sie in der App auf Ihrem Computer."),

            (Msg::ApprovalTooLarge(t), En) => format!("🔐 \"{t}\" needs your OK, but the request is too long to read properly here. Decide in the app on your computer."),
            (Msg::ApprovalTooLarge(t), Tr) => format!("🔐 \"{t}\" için onayınız gerekiyor ama istek burada düzgün okunamayacak kadar uzun. Bilgisayarınızdaki uygulamada karar verin."),
            (Msg::ApprovalTooLarge(t), De) => format!("🔐 \"{t}\" braucht Ihr OK, aber die Anfrage ist zu lang, um sie hier sauber zu lesen. Entscheiden Sie in der App auf Ihrem Computer."),

            (Msg::ApprovalIncomplete, En) => "I could not send the whole request, so there is no button for it here. Decide in the app on your computer.".into(),
            (Msg::ApprovalIncomplete, Tr) => "İsteğin tamamını gönderemedim, bu yüzden burada düğmesi yok. Bilgisayarınızdaki uygulamada karar verin.".into(),
            (Msg::ApprovalIncomplete, De) => "Ich konnte die ganze Anfrage nicht senden, daher gibt es hier keine Schaltfläche. Entscheiden Sie in der App auf Ihrem Computer.".into(),

            (Msg::ButtonApprove, En) => "Approve once".into(),
            (Msg::ButtonApprove, Tr) => "Bir kez onayla".into(),
            (Msg::ButtonApprove, De) => "Einmal erlauben".into(),

            (Msg::ButtonDeny, En) => "Deny".into(),
            (Msg::ButtonDeny, Tr) => "Reddet".into(),
            (Msg::ButtonDeny, De) => "Ablehnen".into(),

            (Msg::Approved, En) => "✅ Approved once.".into(),
            (Msg::Approved, Tr) => "✅ Bir kez onaylandı.".into(),
            (Msg::Approved, De) => "✅ Einmal erlaubt.".into(),

            (Msg::Denied, En) => "⛔ Denied.".into(),
            (Msg::Denied, Tr) => "⛔ Reddedildi.".into(),
            (Msg::Denied, De) => "⛔ Abgelehnt.".into(),

            (Msg::UnknownOrExpired, En) => "This request is no longer open (it ran out or was already decided).".into(),
            (Msg::UnknownOrExpired, Tr) => "Bu istek artık açık değil (süresi doldu veya zaten karara bağlandı).".into(),
            (Msg::UnknownOrExpired, De) => "Diese Anfrage ist nicht mehr offen (abgelaufen oder bereits entschieden).".into(),

            (Msg::NotYours, En) => "These buttons are not for you.".into(),
            (Msg::NotYours, Tr) => "Bu düğmeler sizin için değil.".into(),
            (Msg::NotYours, De) => "Diese Schaltflächen sind nicht für Sie.".into(),

            (Msg::Answered, En) => "Answer sent.".into(),
            (Msg::Answered, Tr) => "Cevap gönderildi.".into(),
            (Msg::Answered, De) => "Antwort gesendet.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGS: [Lang; 3] = [Lang::En, Lang::Tr, Lang::De];
    /// A marker that no catalog text contains, so "exactly once" is unambiguous.
    const ARG: &str = "ARG_x7";

    /// Every message, built with the marker as its text arguments. The match in
    /// [`arguments`] has no wildcard, so a new variant must be added to both.
    fn every_message() -> Vec<Msg<'static>> {
        vec![
            Msg::SecretRequestNotice,
            Msg::ElicitationNotice,
            Msg::NotPaired,
            Msg::Paired,
            Msg::PairInvalid,
            Msg::PairExpired,
            Msg::PairInDm,
            Msg::Allowed,
            Msg::Disallowed,
            Msg::AllowInServer,
            Msg::NoBots,
            Msg::BotsHeading,
            Msg::UseOk(ARG),
            Msg::UseUnknown,
            Msg::UseUsage,
            Msg::Status {
                running: 4_242,
                pending: 7_373,
            },
            Msg::LangSet,
            Msg::LangUsage,
            Msg::Help,
            Msg::TooLong,
            Msg::SecretRemoved { whole: false },
            Msg::SecretRemoved { whole: true },
            Msg::TooFast,
            Msg::Queued,
            Msg::RunStopped,
            Msg::RunFinished,
            Msg::RunFailed(ARG),
            Msg::RunError(ARG),
            Msg::StreamLost,
            Msg::TooManyRuns,
            Msg::AttachmentsNotOpened(ARG),
            Msg::Failed(ARG),
            Msg::ApprovalHeading(ARG),
            Msg::ApprovalDesktopOnly(ARG),
            Msg::ApprovalMasked(ARG),
            Msg::ApprovalDetailsUnavailable(ARG),
            Msg::ApprovalTooLarge(ARG),
            Msg::ApprovalIncomplete,
            Msg::ButtonApprove,
            Msg::ButtonDeny,
            Msg::Approved,
            Msg::Denied,
            Msg::UnknownOrExpired,
            Msg::NotYours,
            Msg::Answered,
        ]
    }

    /// What must appear exactly once in every language's text.
    fn arguments(msg: Msg<'_>) -> Vec<String> {
        match msg {
            Msg::UseOk(a)
            | Msg::RunFailed(a)
            | Msg::RunError(a)
            | Msg::AttachmentsNotOpened(a)
            | Msg::Failed(a)
            | Msg::ApprovalHeading(a)
            | Msg::ApprovalDesktopOnly(a)
            | Msg::ApprovalMasked(a)
            | Msg::ApprovalDetailsUnavailable(a)
            | Msg::ApprovalTooLarge(a) => vec![a.to_owned()],
            Msg::Status { running, pending } => vec![running.to_string(), pending.to_string()],
            Msg::NotPaired => vec!["!pair 123456".into()],
            Msg::SecretRequestNotice
            | Msg::ElicitationNotice
            | Msg::PairInDm
            | Msg::Paired
            | Msg::PairInvalid
            | Msg::PairExpired
            | Msg::Allowed
            | Msg::Disallowed
            | Msg::AllowInServer
            | Msg::NoBots
            | Msg::BotsHeading
            | Msg::UseUnknown
            | Msg::UseUsage
            | Msg::LangSet
            | Msg::LangUsage
            | Msg::Help
            | Msg::TooLong
            | Msg::SecretRemoved { .. }
            | Msg::TooFast
            | Msg::Queued
            | Msg::RunStopped
            | Msg::RunFinished
            | Msg::StreamLost
            | Msg::TooManyRuns
            | Msg::ApprovalIncomplete
            | Msg::ButtonApprove
            | Msg::ButtonDeny
            | Msg::Approved
            | Msg::Denied
            | Msg::UnknownOrExpired
            | Msg::NotYours
            | Msg::Answered => vec![],
        }
    }

    #[test]
    fn every_message_is_translated_and_keeps_its_arguments() {
        for msg in every_message() {
            let english = msg.render(Lang::En);
            for lang in LANGS {
                let text = msg.render(lang);
                assert!(!text.trim().is_empty(), "{msg:?} is empty in {lang:?}");
                for arg in arguments(msg) {
                    assert_eq!(
                        text.matches(&arg).count(),
                        1,
                        "{arg:?} in {msg:?} ({lang:?}): {text}"
                    );
                }
                if lang != Lang::En {
                    // "Problem: ..." is the same word in English and German.
                    let same_word = matches!(msg, Msg::RunError(_)) && lang == Lang::De;
                    assert!(
                        same_word || text != english,
                        "{msg:?} has no real {lang:?} text"
                    );
                }
            }
        }
    }

    #[test]
    fn commands_do_not_change_with_the_language() {
        for lang in LANGS {
            for command in [
                "!bots",
                "!use",
                "!stop",
                "!status",
                "!lang",
                "!allow",
                "!disallow",
            ] {
                assert!(
                    Msg::Help.render(lang).contains(command),
                    "{command} in {lang:?}"
                );
            }
            let usage = Msg::LangUsage.render(lang);
            for code in ["!lang en", "!lang tr", "!lang de"] {
                assert_eq!(usage.matches(code).count(), 1, "{code} in {lang:?}");
            }
            assert_eq!(Msg::UseUsage.render(lang).matches("!use").count(), 1);
            assert_eq!(Msg::UseUnknown.render(lang).matches("!bots").count(), 1);
            assert_eq!(Msg::BotsHeading.render(lang).matches("!use").count(), 1);
            assert_eq!(Msg::Paired.render(lang).matches("!help").count(), 1);
            assert_eq!(Msg::AllowInServer.render(lang).matches("!allow").count(), 1);
        }
    }

    #[test]
    fn approval_fallbacks_send_the_owner_to_the_app_in_every_language() {
        // What the card hides or cannot show must say where to decide, in every
        // language, so no translation reads as if a button could settle it here.
        for lang in LANGS {
            let app = match lang {
                Lang::En => "app on your computer",
                Lang::Tr => "bilgisayarınızdaki uygulama",
                Lang::De => "app auf ihrem computer",
            };
            for msg in [
                Msg::ApprovalDesktopOnly("t"),
                Msg::ApprovalMasked("t"),
                Msg::ApprovalDetailsUnavailable("t"),
                Msg::ApprovalTooLarge("t"),
                Msg::ApprovalIncomplete,
            ] {
                let text = msg.render(lang);
                assert!(
                    text.to_lowercase().contains(app),
                    "{msg:?} ({lang:?}) names the app: {text}"
                );
            }
        }
        assert_eq!(Msg::ButtonApprove.render(Lang::En), "Approve once");
        assert_eq!(Msg::ButtonApprove.render(Lang::Tr), "Bir kez onayla");
        assert_eq!(Msg::ButtonApprove.render(Lang::De), "Einmal erlauben");
    }

    #[test]
    fn language_codes_round_trip_and_unknown_ones_are_refused() {
        for lang in LANGS {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::from_code(" TR_tr "), Some(Lang::Tr));
        assert_eq!(Lang::from_code("de-AT"), Some(Lang::De));
        assert_eq!(Lang::from_code("en-US"), Some(Lang::En));
        for other in ["fr", "", "turkish", "t"] {
            assert_eq!(Lang::from_code(other), None, "{other:?}");
        }
        assert_eq!(Lang::default(), Lang::En);
    }

    #[test]
    fn english_wording_of_the_common_replies_is_pinned() {
        assert_eq!(Msg::RunStopped.render(Lang::En), "Stopped.");
        assert_eq!(Msg::RunFinished.render(Lang::En), "Done.");
        assert_eq!(Msg::Approved.render(Lang::En), "✅ Approved once.");
        assert_eq!(Msg::Denied.render(Lang::En), "⛔ Denied.");
        assert_eq!(
            Msg::Status {
                running: 1,
                pending: 2
            }
            .render(Lang::En),
            "Connected to your computer. Running now: 1. Waiting for your OK: 2."
        );
    }
}
