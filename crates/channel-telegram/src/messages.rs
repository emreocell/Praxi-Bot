//! Telegram-owned messages in English and Turkish. Each paired chat renders
//! them in its own language (see [`crate::pairing::PairingStore::chat_language`]).
//! Keep model and daemon text outside this catalog.

use std::borrow::Cow;

/// Language of a paired Telegram chat. English is the default and the fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Language(bool);

impl Language {
    pub(crate) const EN: Self = Self(false);
    pub(crate) const TR: Self = Self(true);

    /// Stored and `/lang` code.
    pub(crate) const fn code(self) -> &'static str {
        if self.0 { "tr" } else { "en" }
    }

    /// Parse an exact `en` / `tr` code (stored values and `/lang` arguments).
    pub(crate) fn from_code(code: &str) -> Option<Self> {
        match code.trim().to_ascii_lowercase().as_str() {
            "en" => Some(Self::EN),
            "tr" => Some(Self::TR),
            _ => None,
        }
    }

    /// Parse a language tag such as Telegram's `language_code` (`tr`, `tr-TR`,
    /// `en_US`) by its primary subtag. Unsupported languages return `None`.
    pub(crate) fn from_tag(tag: &str) -> Option<Self> {
        let primary = tag.trim().split(['-', '_']).next().unwrap_or_default();
        Self::from_code(primary)
    }

    /// Language for a chat that is paired for the first time: the Telegram
    /// language code, then the desktop preference, then English. An explicit
    /// `/lang` choice always replaces it later.
    pub(crate) fn for_new_pairing(telegram_code: Option<&str>, desktop: Option<Self>) -> Self {
        telegram_code
            .and_then(Self::from_tag)
            .or(desktop)
            .unwrap_or(Self::EN)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message<'a> {
    NoBots,
    AvailableBots,
    BotNotFound {
        query: &'a str,
    },
    InputTooLong,
    /// A secret was cut out of the owner's message (`whole`: nothing was left to pass on).
    SecretRemoved {
        whole: bool,
    },
    NoActiveRun,
    RunStopped,
    UnknownOrExpiredRequest,
    OnceOnly,
    ApproveOnce,
    ApproveForTask {
        program: &'a str,
    },
    Always,
    Deny,
    Approved,
    Denied,
    Answered,
    ApprovalHeading {
        tool: &'a str,
        subagent: bool,
    },
    ApprovalMasked {
        tool: &'a str,
    },
    ApprovalDetailsUnavailable {
        tool: &'a str,
    },
    ApprovalTooLarge {
        tool: &'a str,
    },
    ApprovalIncomplete,
    ApprovalDesktopOnly {
        tool: &'a str,
    },
    SecretRequestNotice,
    LanguageSet,
    LanguageUsage {
        current: Language,
    },
    VoiceUnavailable,
    VoiceTooLong,
    VoiceFailed,
    VoiceEmpty,
    PairUsage,
    Paired,
    AlreadyPaired,
    PairExpired,
    PairInvalid,
    UseUsage,
    BotSwitched {
        name: &'a str,
        id: &'a str,
    },
    RunStoppedId {
        run: &'a str,
    },
    Status {
        status: &'a str,
        version: &'a str,
        uptime_secs: u64,
        pending: usize,
        bot: Option<&'a str>,
        run: Option<&'a str>,
    },
    NoBotsOnDaemon,
    Unsupported {
        label: &'a str,
    },
    Queued,
    TooManyRuns,
    StreamLost,
    SubagentFailed {
        error: &'a str,
    },
    RunFailed {
        error: &'a str,
    },
    RunFinished,
    CouldNotProcess {
        id: i64,
    },
    RunError {
        message: &'a str,
    },
}

/// Turkish object form of an attachment label from `TgMedia::label` ("photo" ->
/// "Fotoğrafları"). An unknown label stays as it is.
fn turkish_attachment(label: &str) -> Cow<'_, str> {
    match label {
        "voice message" => "Sesli mesajları".into(),
        "audio file" => "Ses dosyalarını".into(),
        "video message" => "Görüntülü mesajları".into(),
        "video" => "Videoları".into(),
        "photo" => "Fotoğrafları".into(),
        "file" => "Dosyaları".into(),
        "sticker" => "Çıkartmaları".into(),
        other => format!("{other} türündeki ekleri").into(),
    }
}

impl<'a> Message<'a> {
    #[cfg(test)]
    pub(crate) fn english(self) -> Cow<'a, str> {
        self.render(Language::EN)
    }

    #[allow(clippy::too_many_lines)] // the catalog: one arm per message and language
    pub(crate) fn render(self, language: Language) -> Cow<'a, str> {
        match (self, language.0) {
            (Self::NoBots, false) => "No bots configured.".into(),
            (Self::NoBots, true) => "Henüz bir bot ayarlanmamış.".into(),
            (Self::AvailableBots, false) => "Available bots:".into(),
            (Self::AvailableBots, true) => "Kullanılabilir botlar:".into(),
            (Self::BotNotFound { query }, false) => {
                format!("Bot {query:?} not found. Use /bots to list available bots.").into()
            }
            (Self::BotNotFound { query }, true) => format!(
                "{query:?} adlı bot bulunamadı. Botları listelemek için /bots komutunu kullan."
            )
            .into(),
            (Self::InputTooLong, false) => "Message too long; please shorten it.".into(),
            (Self::InputTooLong, true) => "Mesaj çok uzun. Lütfen kısalt.".into(),
            (Self::SecretRemoved { whole }, false) => format!(
                "🔒 That message looks like a secret (a key, token, password or card number), so I {}. \
                 Telegram keeps the original in this chat: please delete it. To give me a secret \
                 safely, store it in the vault in the desktop app.",
                if whole {
                    "did not pass it on"
                } else {
                    "cut that part out and passed on the rest"
                }
            )
            .into(),
            (Self::SecretRemoved { whole }, true) => format!(
                "🔒 Bu mesaj gizli bilgiye benziyor (anahtar, jeton, parola ya da kart numarası), \
                 o yüzden {}. Telegram asıl mesajı bu sohbette tutuyor: lütfen sil. Gizli bir \
                 bilgiyi bana güvenle vermek için masaüstü uygulamasındaki kasaya kaydet.",
                if whole {
                    "iletmedim"
                } else {
                    "o kısmı çıkarıp mesajın geri kalanını ilettim"
                }
            )
            .into(),
            (Self::NoActiveRun, false) => "No active run to stop.".into(),
            (Self::NoActiveRun, true) => "Durdurulacak etkin çalışma yok.".into(),
            (Self::RunStopped, false) => "🛑 Run stopped.".into(),
            (Self::RunStopped, true) => "🛑 Çalışma durduruldu.".into(),
            (Self::UnknownOrExpiredRequest, false) => "Unknown or expired request".into(),
            (Self::UnknownOrExpiredRequest, true) => "İstek tanınmıyor veya süresi dolmuş".into(),
            (Self::OnceOnly, false) => "This can only be approved once".into(),
            (Self::OnceOnly, true) => "Bu istek yalnızca bir kez onaylanabilir".into(),
            (Self::ApproveOnce, false) => "Approve once".into(),
            (Self::ApproveOnce, true) => "Bir kez onayla".into(),
            (Self::ApproveForTask { program }, false) => format!("This task: {program}").into(),
            (Self::ApproveForTask { program }, true) => format!("Bu görev için: {program}").into(),
            (Self::Always, false) => "Always".into(),
            (Self::Always, true) => "Her zaman onayla".into(),
            (Self::Deny, false) => "Deny".into(),
            (Self::Deny, true) => "Reddet".into(),
            (Self::Approved, false) => "Approved".into(),
            (Self::Approved, true) => "Onaylandı".into(),
            (Self::Denied, false) => "Denied".into(),
            (Self::Denied, true) => "Reddedildi".into(),
            (Self::Answered, false) => "Answered".into(),
            (Self::Answered, true) => "Yanıtlandı".into(),
            (Self::ApprovalHeading { tool, subagent }, false) => {
                let prefix = if subagent { "subagent: " } else { "" };
                format!("⚠️ Approval needed ({prefix}{tool})").into()
            }
            (Self::ApprovalHeading { tool, subagent }, true) => {
                let prefix = if subagent { "alt ajan: " } else { "" };
                format!("⚠️ Onay gerekiyor ({prefix}{tool})").into()
            }
            (Self::ApprovalMasked { tool }, false) => format!(
                "⚠️ {tool} needs approval, but part of it would be masked here, so this chat cannot show it fully. Approve or deny it in the desktop app."
            ).into(),
            (Self::ApprovalMasked { tool }, true) => format!(
                "⚠️ {tool} için onay gerekiyor. Bazı ayrıntılar burada gizleneceği için isteğin tamamı bu sohbette gösterilemiyor. Masaüstü uygulamasında onayla veya reddet."
            ).into(),
            (Self::ApprovalDetailsUnavailable { tool }, false) => format!(
                "⚠️ {tool} needs approval, but this chat cannot show what it would do. Approve or deny it in the desktop app."
            ).into(),
            (Self::ApprovalDetailsUnavailable { tool }, true) => format!(
                "⚠️ {tool} için onay gerekiyor ancak aracın ne yapacağı bu sohbette gösterilemiyor. Masaüstü uygulamasında onayla veya reddet."
            ).into(),
            (Self::ApprovalTooLarge { tool }, false) => format!(
                "⚠️ {tool} needs approval, but it is too large to review in Telegram. Review it in the desktop app."
            ).into(),
            (Self::ApprovalTooLarge { tool }, true) => format!(
                "⚠️ {tool} için onay gerekiyor. İstek Telegram’da incelenemeyecek kadar uzun. Masaüstü uygulamasında incele."
            ).into(),
            (Self::ApprovalDesktopOnly { tool }, false) => format!(
                "🔒 {tool} needs your approval, and this kind of action can only be approved in the desktop app (not from Telegram). Open the desktop app to allow or deny it."
            ).into(),
            (Self::ApprovalDesktopOnly { tool }, true) => format!(
                "🔒 {tool} için onayın gerekiyor ve bu tür bir işlem yalnızca masaüstü uygulamasında onaylanabilir (Telegram’dan değil). İzin vermek veya reddetmek için masaüstü uygulamasını aç."
            ).into(),
            (Self::ApprovalIncomplete, false) => "Approval could not be shown completely here. Approve or deny it in the desktop app.".into(),
            (Self::ApprovalIncomplete, true) => "Onay isteğinin tamamı burada gösterilemedi. Masaüstü uygulamasında onayla veya reddet.".into(),
            (Self::VoiceUnavailable, false) => "I can't read voice notes yet. Turn on offline voice in the desktop app (Settings > Dictation), or send your request as text.".into(),
            (Self::VoiceUnavailable, true) => "Sesli mesajları henüz okuyamıyorum. Masaüstü uygulamasında çevrimdışı sesi aç (Ayarlar > Dikte) ya da isteğini yazıyla gönder.".into(),
            (Self::VoiceTooLong, false) => "That voice note is too long (limit: 2 minutes). Please send a shorter one or write it.".into(),
            (Self::VoiceTooLong, true) => "Sesli mesaj çok uzun (sınır: 2 dakika). Lütfen daha kısa gönder ya da yaz.".into(),
            (Self::VoiceFailed, false) => "I could not turn that voice note into text. Please try again or write it.".into(),
            (Self::VoiceFailed, true) => "Sesli mesajı metne çeviremedim. Lütfen tekrar dene ya da yaz.".into(),
            (Self::VoiceEmpty, false) => "I did not hear any speech in that voice note.".into(),
            (Self::VoiceEmpty, true) => "Sesli mesajda konuşma duyamadım.".into(),
            (Self::PairUsage, false) => "Usage: /pair <6-digit-code>".into(),
            (Self::PairUsage, true) => "Kullanım: /pair <6 haneli kod>".into(),
            (Self::Paired, false) => "✅ Paired successfully! Send a message or use /bots to choose a bot.".into(),
            (Self::Paired, true) => "✅ Eşleşme tamamlandı! Bir mesaj gönder ya da bot seçmek için /bots komutunu kullan.".into(),
            (Self::AlreadyPaired, false) => "✅ Your Telegram account is already paired.".into(),
            (Self::AlreadyPaired, true) => "✅ Telegram hesabın zaten eşleşmiş.".into(),
            (Self::PairExpired, false) => "❌ Pairing code has expired. Generate a new code and try again.".into(),
            (Self::PairExpired, true) => "❌ Eşleşme kodunun süresi dolmuş. Yeni bir kod oluşturup tekrar dene.".into(),
            (Self::PairInvalid, false) => "❌ Invalid pairing code.".into(),
            (Self::PairInvalid, true) => "❌ Eşleşme kodu geçersiz.".into(),
            (Self::UseUsage, false) => "Usage: /use <bot-name-or-id>".into(),
            (Self::UseUsage, true) => "Kullanım: /use <bot adı veya kimliği>".into(),
            (Self::BotSwitched { name, id }, false) => format!("Switched active bot to {name} ({id}).").into(),
            (Self::BotSwitched { name, id }, true) => format!("Etkin bot {name} ({id}) olarak değiştirildi.").into(),
            (Self::RunStoppedId { run }, false) => format!("🛑 Stopped run {run}.").into(),
            (Self::RunStoppedId { run }, true) => format!("🛑 {run} çalışması durduruldu.").into(),
            (Self::Status { status, version, uptime_secs, pending, bot, run }, false) => format!(
                "Daemon: {status} (v{version}, up {uptime_secs}s)\nPending approvals: {pending}\nBot: {}\nRun: {}",
                bot.unwrap_or("default"),
                run.unwrap_or("idle"),
            ).into(),
            (Self::Status { status, version, uptime_secs, pending, bot, run }, true) => format!(
                "Daemon: {status} (v{version}, {uptime_secs} sn'dir açık)\nBekleyen onaylar: {pending}\nBot: {}\nÇalışma: {}",
                bot.unwrap_or("varsayılan"),
                run.unwrap_or("boşta"),
            ).into(),
            (Self::NoBotsOnDaemon, false) => "No bots configured on the daemon. Add a bot first.".into(),
            (Self::NoBotsOnDaemon, true) => "Daemon'da ayarlanmış bot yok. Önce bir bot ekle.".into(),
            (Self::Unsupported { label }, false) => format!("I can't read a {label} yet. Please send your request as text.").into(),
            (Self::Unsupported { label }, true) => format!("{} henüz okuyamıyorum. Lütfen isteğini yazıyla gönder.", turkish_attachment(label)).into(),
            (Self::Queued, false) => "⏳ Message queued behind the active run.".into(),
            (Self::Queued, true) => "⏳ Mesaj, etkin çalışmanın arkasında sıraya alındı.".into(),
            (Self::TooManyRuns, false) => "⚠️ Too many runs are being followed right now; this run's output will not be streamed here. Try again in a moment.".into(),
            (Self::TooManyRuns, true) => "⚠️ Şu anda çok fazla çalışma izleniyor; bu çalışmanın çıktısı buraya aktarılmayacak. Birazdan tekrar dene.".into(),
            (Self::StreamLost, false) => "⚠️ Lost the connection to the run's event stream; it may still be running.".into(),
            (Self::StreamLost, true) => "⚠️ Çalışmanın olay akışıyla bağlantı koptu; çalışma hâlâ sürüyor olabilir.".into(),
            (Self::SubagentFailed { error }, false) => format!("❌ Subagent failed: {error}").into(),
            (Self::SubagentFailed { error }, true) => format!("❌ Alt ajan başarısız oldu: {error}").into(),
            (Self::RunFailed { error }, false) => format!("❌ Run failed: {error}").into(),
            (Self::RunFailed { error }, true) => format!("❌ Çalışma başarısız oldu: {error}").into(),
            (Self::RunFinished, false) => "✅ Run finished.".into(),
            (Self::RunFinished, true) => "✅ Çalışma bitti.".into(),
            (Self::CouldNotProcess { id }, false) => format!("Could not process message {id}.").into(),
            (Self::CouldNotProcess { id }, true) => format!("{id} numaralı mesaj işlenemedi.").into(),
            (Self::RunError { message }, false) => format!("⚠️ Error: {message}").into(),
            (Self::RunError { message }, true) => format!("⚠️ Hata: {message}").into(),
            (Self::SecretRequestNotice, false) => "A credential request is waiting. Open Praxi on your computer to answer. Keep credentials out of this chat.".into(),
            (Self::SecretRequestNotice, true) => "Yanıtınızı bekleyen bir gizli bilgi isteği var. Yanıtlamak için bilgisayarınızda Praxi’yi açın. Gizli bilgilerinizi bu sohbette paylaşmayın.".into(),
            (Self::LanguageSet, false) => "This chat now uses English.".into(),
            (Self::LanguageSet, true) => "Bu sohbet artık Türkçe.".into(),
            (Self::LanguageUsage { current }, false) => {
                let name = if current.0 { "Turkish" } else { "English" };
                format!("This chat uses {name}. To change it, send /lang en or /lang tr.").into()
            }
            (Self::LanguageUsage { current }, true) => {
                let name = if current.0 { "Türkçe" } else { "İngilizce" };
                format!("Bu sohbetin dili: {name}. Değiştirmek için /lang en veya /lang tr gönder.").into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Language, Message};

    #[test]
    fn language_codes_and_new_pairing_priority() {
        assert_eq!(Language::default(), Language::EN);
        for (tag, expected) in [
            ("tr", Some(Language::TR)),
            ("TR-tr", Some(Language::TR)),
            ("tr_TR", Some(Language::TR)),
            ("en-GB", Some(Language::EN)),
            ("de", None),
            ("", None),
            ("turkish", None),
        ] {
            assert_eq!(Language::from_tag(tag), expected, "{tag}");
        }
        assert_eq!(Language::from_code("tr-TR"), None);
        assert_eq!(Language::from_code(Language::TR.code()), Some(Language::TR));
        assert_eq!(
            Language::for_new_pairing(Some("tr-TR"), Some(Language::EN)),
            Language::TR
        );
        assert_eq!(
            Language::for_new_pairing(Some("en"), Some(Language::TR)),
            Language::EN
        );
        assert_eq!(
            Language::for_new_pairing(Some("de"), Some(Language::TR)),
            Language::TR
        );
        assert_eq!(
            Language::for_new_pairing(None, Some(Language::TR)),
            Language::TR
        );
        assert_eq!(Language::for_new_pairing(Some("de"), None), Language::EN);
        assert_eq!(Language::for_new_pairing(None, None), Language::EN);
    }

    /// The text each message must show exactly once in every language. The match has
    /// no wildcard, so a new message does not compile until it is listed here.
    fn arguments(message: Message<'_>) -> Vec<String> {
        match message {
            Message::BotNotFound { query } => vec![query.into()],
            Message::ApproveForTask { program } => vec![program.into()],
            Message::ApprovalHeading { tool, .. }
            | Message::ApprovalMasked { tool }
            | Message::ApprovalDetailsUnavailable { tool }
            | Message::ApprovalTooLarge { tool }
            | Message::ApprovalDesktopOnly { tool } => vec![tool.into()],
            Message::BotSwitched { name, id } => vec![name.into(), id.into()],
            Message::RunStoppedId { run } => vec![run.into()],
            Message::Status {
                status,
                version,
                uptime_secs,
                pending,
                bot,
                run,
            } => {
                let mut parts = vec![
                    status.into(),
                    version.into(),
                    uptime_secs.to_string(),
                    pending.to_string(),
                ];
                parts.extend(bot.map(Into::into));
                parts.extend(run.map(Into::into));
                parts
            }
            Message::Unsupported { label } => vec![label.into()],
            Message::SubagentFailed { error } | Message::RunFailed { error } => {
                vec![error.into()]
            }
            Message::CouldNotProcess { id } => vec![id.to_string()],
            Message::RunError { message } => vec![message.into()],
            Message::NoBots
            | Message::AvailableBots
            | Message::InputTooLong
            | Message::SecretRemoved { .. }
            | Message::NoActiveRun
            | Message::RunStopped
            | Message::UnknownOrExpiredRequest
            | Message::OnceOnly
            | Message::ApproveOnce
            | Message::Always
            | Message::Deny
            | Message::Approved
            | Message::Denied
            | Message::Answered
            | Message::ApprovalIncomplete
            | Message::SecretRequestNotice
            | Message::LanguageSet
            | Message::LanguageUsage { .. }
            | Message::VoiceUnavailable
            | Message::VoiceTooLong
            | Message::VoiceFailed
            | Message::VoiceEmpty
            | Message::PairUsage
            | Message::Paired
            | Message::AlreadyPaired
            | Message::PairExpired
            | Message::PairInvalid
            | Message::UseUsage
            | Message::NoBotsOnDaemon
            | Message::Queued
            | Message::TooManyRuns
            | Message::StreamLost
            | Message::RunFinished => Vec::new(),
        }
    }

    /// One sample of every message, with distinctive argument values.
    fn every_message() -> Vec<Message<'static>> {
        let tool = "TOOL_x7";
        let status = |bot, run| Message::Status {
            status: "STATUS_x7",
            version: "VERSION_x7",
            uptime_secs: 918_273,
            pending: 646_464,
            bot,
            run,
        };
        vec![
            Message::NoBots,
            Message::AvailableBots,
            Message::BotNotFound { query: "QUERY_x7" },
            Message::InputTooLong,
            Message::SecretRemoved { whole: false },
            Message::SecretRemoved { whole: true },
            Message::NoActiveRun,
            Message::RunStopped,
            Message::UnknownOrExpiredRequest,
            Message::OnceOnly,
            Message::ApproveOnce,
            Message::ApproveForTask {
                program: "PROGRAM_x7",
            },
            Message::Always,
            Message::Deny,
            Message::Approved,
            Message::Denied,
            Message::Answered,
            Message::ApprovalHeading {
                tool,
                subagent: false,
            },
            Message::ApprovalHeading {
                tool,
                subagent: true,
            },
            Message::ApprovalMasked { tool },
            Message::ApprovalDetailsUnavailable { tool },
            Message::ApprovalTooLarge { tool },
            Message::ApprovalIncomplete,
            Message::ApprovalDesktopOnly { tool },
            Message::SecretRequestNotice,
            Message::LanguageSet,
            Message::LanguageUsage {
                current: Language::EN,
            },
            Message::LanguageUsage {
                current: Language::TR,
            },
            Message::VoiceUnavailable,
            Message::VoiceTooLong,
            Message::VoiceFailed,
            Message::VoiceEmpty,
            Message::PairUsage,
            Message::Paired,
            Message::AlreadyPaired,
            Message::PairExpired,
            Message::PairInvalid,
            Message::UseUsage,
            Message::BotSwitched {
                name: "NAME_x7",
                id: "ID_x7",
            },
            Message::RunStoppedId { run: "RUN_x7" },
            status(Some("BOT_x7"), Some("RUNID_x7")),
            status(None, None),
            Message::NoBotsOnDaemon,
            Message::Unsupported { label: "LABEL_x7" },
            Message::Queued,
            Message::TooManyRuns,
            Message::StreamLost,
            Message::SubagentFailed { error: "ERROR_x7" },
            Message::RunFailed { error: "ERROR_x7" },
            Message::RunFinished,
            Message::CouldNotProcess { id: 737_373 },
            Message::RunError {
                message: "MESSAGE_x7",
            },
        ]
    }

    // Every message has both languages (the render match has no wildcard, so a missing
    // arm does not compile), each language shows every argument exactly once, and the
    // Turkish text is not a copy of the English one.
    #[test]
    fn every_message_is_translated_with_the_same_arguments() {
        for message in every_message() {
            let english = message.render(Language::EN);
            let turkish = message.render(Language::TR);
            assert!(!english.trim().is_empty(), "{message:?}");
            assert_ne!(english, turkish, "{message:?} is not translated");
            for argument in arguments(message) {
                for text in [&english, &turkish] {
                    assert_eq!(
                        text.matches(argument.as_str()).count(),
                        1,
                        "{message:?}: {argument:?} in {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn remaining_english_messages_are_unchanged() {
        let status = Message::Status {
            status: "ok",
            version: "0.1.0",
            uptime_secs: 120,
            pending: 1,
            bot: None,
            run: None,
        };
        for (message, english) in [
            (Message::PairUsage, "Usage: /pair <6-digit-code>"),
            (
                Message::Paired,
                "✅ Paired successfully! Send a message or use /bots to choose a bot.",
            ),
            (
                Message::AlreadyPaired,
                "✅ Your Telegram account is already paired.",
            ),
            (
                Message::PairExpired,
                "❌ Pairing code has expired. Generate a new code and try again.",
            ),
            (Message::PairInvalid, "❌ Invalid pairing code."),
            (Message::UseUsage, "Usage: /use <bot-name-or-id>"),
            (
                Message::BotSwitched {
                    name: "Mira",
                    id: "b1",
                },
                "Switched active bot to Mira (b1).",
            ),
            (Message::RunStoppedId { run: "r1" }, "🛑 Stopped run r1."),
            (
                status,
                "Daemon: ok (v0.1.0, up 120s)\nPending approvals: 1\nBot: default\nRun: idle",
            ),
            (
                Message::NoBotsOnDaemon,
                "No bots configured on the daemon. Add a bot first.",
            ),
            (
                Message::Unsupported { label: "photo" },
                "I can't read a photo yet. Please send your request as text.",
            ),
            (Message::Queued, "⏳ Message queued behind the active run."),
            (
                Message::TooManyRuns,
                "⚠️ Too many runs are being followed right now; this run's output will not be streamed here. Try again in a moment.",
            ),
            (
                Message::StreamLost,
                "⚠️ Lost the connection to the run's event stream; it may still be running.",
            ),
            (
                Message::SubagentFailed { error: "boom" },
                "❌ Subagent failed: boom",
            ),
            (Message::RunFailed { error: "boom" }, "❌ Run failed: boom"),
            (Message::RunFinished, "✅ Run finished."),
            (
                Message::CouldNotProcess { id: 42 },
                "Could not process message 42.",
            ),
            (Message::RunError { message: "boom" }, "⚠️ Error: boom"),
        ] {
            assert_eq!(message.english(), english);
        }
    }

    #[test]
    fn remaining_messages_keep_their_arguments_in_both_languages() {
        let (name, id, run, error) = ("Mira Bot", "bot-7f3a", "run-91c2", "disk full: /tmp");
        for language in [Language::EN, Language::TR] {
            let once = |text: &str, part: &str| {
                assert_eq!(text.matches(part).count(), 1, "{part:?} in {text:?}");
            };
            let switched = Message::BotSwitched { name, id }.render(language);
            once(&switched, name);
            once(&switched, id);
            once(&Message::RunStoppedId { run }.render(language), run);
            once(&Message::RunFailed { error }.render(language), error);
            once(&Message::SubagentFailed { error }.render(language), error);
            once(
                &Message::CouldNotProcess { id: 4242 }.render(language),
                "4242",
            );
            once(&Message::PairUsage.render(language), "/pair");
            once(&Message::UseUsage.render(language), "/use");
            once(&Message::Paired.render(language), "/bots");
            let status = Message::Status {
                status: "degraded",
                version: "9.8.7",
                uptime_secs: 3605,
                pending: 17,
                bot: Some(id),
                run: Some(run),
            }
            .render(language);
            for part in ["degraded", "v9.8.7", "3605", "17", id, run] {
                once(&status, part);
            }
        }
    }

    #[test]
    fn every_attachment_label_has_a_turkish_name() {
        // The labels `TgMedia::label` produces.
        for label in [
            "voice message",
            "audio file",
            "video message",
            "video",
            "photo",
            "file",
            "sticker",
        ] {
            let text = Message::Unsupported { label }.render(Language::TR);
            assert!(!text.contains(label), "{label} not translated: {text}");
            assert!(text.ends_with("Lütfen isteğini yazıyla gönder."));
        }
        let unknown = Message::Unsupported { label: "poll" }.render(Language::TR);
        assert!(unknown.starts_with("poll türündeki ekleri"));
    }

    #[test]
    fn language_replies_name_both_commands() {
        for language in [Language::EN, Language::TR] {
            for current in [Language::EN, Language::TR] {
                let usage = Message::LanguageUsage { current }.render(language);
                assert_eq!(usage.matches("/lang en").count(), 1);
                assert_eq!(usage.matches("/lang tr").count(), 1);
            }
        }
        assert_eq!(
            Message::LanguageSet.render(Language::TR),
            "Bu sohbet artık Türkçe."
        );
    }
    #[test]
    fn preserves_the_existing_english_labels() {
        assert_eq!(Message::ApproveOnce.render(Language::EN), "Approve once");
        assert_eq!(Message::Always.render(Language::EN), "Always");
        assert_eq!(Message::Deny.render(Language::EN), "Deny");
        assert_eq!(Message::NoBots.render(Language::EN), "No bots configured.");
    }

    #[test]
    fn turkish_distinguishes_the_two_approval_scopes() {
        assert_eq!(Message::ApproveOnce.render(Language::TR), "Bir kez onayla");
        assert_eq!(Message::Always.render(Language::TR), "Her zaman onayla");
        assert_eq!(
            Message::OnceOnly.render(Language::TR),
            "Bu istek yalnızca bir kez onaylanabilir"
        );
    }

    #[test]
    fn preserves_display_arguments_and_protocol_command_names() {
        let program = "örnek.exe";
        let query = "Türkçe Bot";
        for language in [Language::EN, Language::TR] {
            let task = Message::ApproveForTask { program }.render(language);
            assert!(task.ends_with(program));
            assert_eq!(task.matches(program).count(), 1);
            let missing = Message::BotNotFound { query }.render(language);
            assert_eq!(missing.matches("/bots").count(), 1);
            assert_eq!(missing.matches(query).count(), 1);
        }
    }
    #[test]
    fn approval_heading_preserves_tool_and_subagent_context() {
        let tool = "tool_name";
        let main = Message::ApprovalHeading {
            tool,
            subagent: false,
        };
        let child = Message::ApprovalHeading {
            tool,
            subagent: true,
        };
        assert_eq!(main.english(), "⚠️ Approval needed (tool_name)");
        assert_eq!(child.english(), "⚠️ Approval needed (subagent: tool_name)");
        assert_eq!(main.render(Language::TR), "⚠️ Onay gerekiyor (tool_name)");
        assert_eq!(
            child.render(Language::TR),
            "⚠️ Onay gerekiyor (alt ajan: tool_name)"
        );
    }

    #[test]
    fn approval_fallbacks_keep_the_same_desktop_guidance() {
        let tool = "tool_name";
        for message in [
            Message::ApprovalMasked { tool },
            Message::ApprovalDetailsUnavailable { tool },
        ] {
            assert_eq!(message.english().matches(tool).count(), 1);
            assert!(
                message
                    .english()
                    .ends_with("Approve or deny it in the desktop app.")
            );
            let translated = message.render(Language::TR);
            assert_eq!(translated.matches(tool).count(), 1);
            assert!(translated.ends_with("Masaüstü uygulamasında onayla veya reddet."));
        }
        let large = Message::ApprovalTooLarge { tool };
        assert_eq!(large.english().matches(tool).count(), 1);
        assert!(large.english().ends_with("Review it in the desktop app."));
        let translated = large.render(Language::TR);
        assert_eq!(translated.matches(tool).count(), 1);
        assert!(translated.ends_with("Masaüstü uygulamasında incele."));
        assert!(
            Message::ApprovalIncomplete
                .render(Language::TR)
                .ends_with("Masaüstü uygulamasında onayla veya reddet.")
        );
    }
}
