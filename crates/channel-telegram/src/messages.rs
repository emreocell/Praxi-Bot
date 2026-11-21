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
    ApprovalHeading { tool: &'a str, subagent: bool },
    ApprovalMasked { tool: &'a str },
    ApprovalDetailsUnavailable { tool: &'a str },
    ApprovalTooLarge { tool: &'a str },
    ApprovalIncomplete,
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
            (Self::ApprovalIncomplete, false) => "Approval could not be shown completely here. Approve or deny it in the desktop app.".into(),
            (Self::ApprovalIncomplete, true) => "Onay isteğinin tamamı burada gösterilemedi. Masaüstü uygulamasında onayla veya reddet.".into(),
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
