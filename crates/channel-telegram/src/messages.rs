//! Telegram-owned messages. The bridge uses English until per-chat language
//! selection is added. Keep model and daemon text outside this catalog.

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Language(bool);

impl Language {
    const EN: Self = Self(false);
    #[cfg(test)]
    const TR: Self = Self(true);
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message<'a> {
    NoBots,
    AvailableBots,
    BotNotFound { query: &'a str },
    InputTooLong,
    NoActiveRun,
    RunStopped,
    UnknownOrExpiredRequest,
    OnceOnly,
    ApproveOnce,
    ApproveForTask { program: &'a str },
    Always,
    Deny,
    Approved,
    Denied,
    Answered,
}

impl<'a> Message<'a> {
    pub(crate) fn english(self) -> Cow<'a, str> {
        self.render(Language::EN)
    }

    fn render(self, language: Language) -> Cow<'a, str> {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Language, Message};
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
}
