//! Auto-continue: a turn that ends with work left is continued by the runtime.
//!
//! A model often finishes one item and stops with "Shall I do the next one?" or
//! "I can also research other companies, want me to?". The owner then types
//! "go on" after every step. This module decides, from the model's closing
//! text and its plan, whether the turn really ended or merely asked for
//! permission. It is a pure text and counting rule; the loop applies it only
//! inside the step, token and time budgets, and never for money.
//!
//! * A reply that **asks permission to continue** is answered "yes" at the Full
//!   level (the owner chose to work alone).
//! * A reply that ends **without a question while the plan still has open
//!   steps** is continued at every level (a progress report is not the end).
//! * A **real question** ("which e-mail address?"), a **finished task**
//!   (`Done:`) and anything about **money** end the turn as before.
//!
//! Detection is a list of short phrases per language (English, Turkish,
//! German, Spanish). A phrase that is missing means the turn simply ends: the
//! failure mode is the old behaviour, never a runaway loop.

/// Why the runtime continues the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// The reply asked for permission to continue (Full level).
    AskedPermission,
    /// The plan still has open steps and the reply asked nothing.
    OpenPlan,
}

impl Reason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AskedPermission => "asked_permission",
            Self::OpenPlan => "open_plan",
        }
    }
}

/// Runtime-written reply recorded as the nudge step. Honest about who wrote
/// it: it is not the owner typing.
#[must_use]
pub(crate) fn continue_text(reason: Reason, open_steps: usize) -> String {
    let lead = match reason {
        Reason::AskedPermission => {
            "Automatic reply from the Praxi runtime, not typed by the owner: the owner chose to let you work alone, so the answer is yes, continue."
        }
        Reason::OpenPlan => {
            "Automatic message from the Praxi runtime, not typed by the owner: your turn ended but the plan is not finished."
        }
    };
    let plan = if open_steps > 0 {
        format!(" Your plan still has {open_steps} open step(s).")
    } else {
        String::new()
    };
    format!(
        "{lead}{plan} Do the next step now and keep going until the whole task is done; do not ask for permission again. \
         Decide small choices yourself and mention them in your final report. \
         If one item needs the owner (a CAPTCHA, a 2FA code, a login only they have), mark that item as blocked, say so briefly and carry on with the rest. \
         Never move money. If everything is already done, reply with \"Done:\" and a short report instead."
    )
}

/// Lowercase and fold the letters that differ between the languages and their
/// ASCII spellings, so one phrase list matches both.
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|&c| c != '\u{307}')
        .map(|c| match c {
            'ı' | 'í' | 'ì' | 'î' => 'i',
            'ş' => 's',
            'ğ' => 'g',
            'ü' | 'ú' | 'ù' | 'û' => 'u',
            'ö' | 'ó' | 'ò' | 'ô' => 'o',
            'ç' => 'c',
            'ä' | 'á' | 'à' | 'â' => 'a',
            'é' | 'è' | 'ê' => 'e',
            'ñ' => 'n',
            '’' | '‘' => '\'',
            '¿' | '¡' => ' ',
            c => c,
        })
        .collect()
}

/// Phrases of "may I continue / do the next / do more?" (folded). Each is
/// specific to asking leave; "is there anything else?" is not in the list on
/// purpose (a closing courtesy is not a request to continue).
const ASK_PHRASES: &[&str] = &[
    // English
    "should i continue",
    "shall i continue",
    "should i proceed",
    "shall i proceed",
    "should i go on",
    "shall i go on",
    "should i go ahead",
    "shall i go ahead",
    "should i keep going",
    "shall i keep going",
    "should i move on",
    "shall i move on",
    "should i start",
    "shall i start",
    "should i do the next",
    "shall i do the next",
    "should i do the rest",
    "shall i do the rest",
    "should i carry on",
    "shall i carry on",
    "want me to continue",
    "want me to proceed",
    "want me to go on",
    "want me to keep going",
    "want me to move on",
    "want me to do the next",
    "want me to do the rest",
    "want me to do the same",
    "want me to start",
    "want me to also",
    "want me to try",
    "want me to look",
    "want me to check",
    "want me to research",
    "want me to register",
    "want me to sign",
    "want me to go ahead",
    "like me to continue",
    "like me to proceed",
    "like me to go on",
    "like me to keep going",
    "like me to move on",
    "like me to do the next",
    "like me to do the rest",
    "like me to start",
    "like me to also",
    "like me to try",
    "like me to look",
    "like me to check",
    "like me to research",
    "like me to go ahead",
    "ready to continue",
    "ready for the next",
    "let me know if you'd like me to",
    "let me know if you want me to",
    "let me know if i should",
    "do you want me to continue",
    "do you want me to proceed",
    "do you want me to go on",
    "do you want me to do",
    "do you want me to move",
    "do you want me to start",
    "do you want me to also",
    "do you want me to try",
    "do you want me to look",
    "do you want me to check",
    "do you want me to research",
    "do you want me to keep",
    "do you want to continue",
    "do you want to proceed",
    "ok to continue",
    "ok to proceed",
    "okay to continue",
    "okay to proceed",
    "go-ahead",
    "go ahead with the next",
    "continue with the next",
    "proceed with the next",
    "move on to the next",
    "move to the next",
    // Turkish
    "devam edeyim mi",
    "devam edelim mi",
    "devam edeyim",
    "devam etmemi ister",
    "devam etmeme izin",
    "devam etmek ister",
    "devam mi",
    "devam edebilirim",
    "sonrakine gecmemi ister",
    "sonrakine gecmek ister",
    "sonrakine gecelim mi",
    "sonrakine gecim mi",
    "geceyim mi",
    "gecelim mi",
    "gecmemi ister",
    "edeyim mi",
    "edelim mi",
    "sonrakine gecevim mi",
    "sonrakine gecebilirim",
    "sonraki siteye",
    "sonraki adima",
    "sonraki adimi",
    "digerlerine de",
    "digerine de",
    "digerine gecmemi",
    "digerlerine gecmemi",
    "digerlerini de",
    "baslayayim mi",
    "baslamami ister",
    "baslayalim mi",
    "yapayim mi",
    "yapmami ister",
    "yapmami istermisin",
    "arastirayim mi",
    "arastirmami ister",
    "bakayim mi",
    "bakmami ister",
    "kontrol edeyim mi",
    "kontrol etmemi ister",
    "kaydolayim mi",
    "kayit olayim mi",
    "devam etmemi",
    "ilerleyeyim mi",
    "ilerlememi ister",
    "ister misin, devam",
    "onay verirsen devam",
    "onayin ile devam",
    "onayiniz ile devam",
    "izin verirsen",
    "izin verirseniz",
    // German
    "soll ich weitermachen",
    "soll ich fortfahren",
    "soll ich weiter",
    "soll ich mit dem nachsten",
    "soll ich als nachstes",
    "soll ich auch",
    "soll ich es auch",
    "soll ich anfangen",
    "soll ich starten",
    "soll ich den rest",
    "mochten sie, dass ich",
    "mochtest du, dass ich",
    "willst du, dass ich",
    "wollen sie, dass ich",
    "darf ich weitermachen",
    "darf ich fortfahren",
    // Spanish
    "quieres que continue",
    "quieres que siga",
    "quieres que proceda",
    "quieres que haga",
    "quieres que busque",
    "quieres que empiece",
    "quieres que pase",
    "deseas que continue",
    "desea que continue",
    "deseas que siga",
    "debo continuar",
    "debo seguir",
    "debo proceder",
    " sigo",
    " continuo",
    "paso al siguiente",
    "paso a la siguiente",
    " procedo",
];

/// Endings of an offer that leaves out what it offers: "..., want me to?".
const ASK_ENDINGS: &[&str] = &[
    "want me to",
    "like me to",
    "want me to do that",
    "want me to do it",
    "want me to do so",
    "should i",
    "shall i",
    "ister misin",
    "ister misiniz",
];

/// Words that mean money would move. A reply about them is never answered
/// automatically: spending always stops for the owner.
const MONEY_WORDS: &[&str] = &[
    "$",
    "€",
    "£",
    "₺",
    " usd",
    " eur ",
    " tl ",
    "payment",
    "purchase",
    "checkout",
    "subscribe",
    "subscription",
    "credit card",
    "card number",
    "invoice",
    "billing",
    " pay ",
    " pay?",
    " buy ",
    "odeme",
    "satin al",
    "abonelik",
    "kredi kart",
    "fatura",
    "bezahl",
    "kaufen",
    "zahlung",
    "pago",
    "comprar",
    "suscrip",
];

/// Markers of a finished task: the Full prompt asks for "Done:" in the user's language.
const DONE_MARKERS: &[&str] = &[
    "done:",
    "tamamlandi:",
    "bitti:",
    "gorev tamamlandi",
    "fertig:",
    "erledigt:",
    "listo:",
    "hecho:",
    "completed:",
];

fn tail_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    let skip = count - n;
    s.char_indices().nth(skip).map_or(s, |(i, _)| &s[i..])
}

/// The reply declares the task finished (`Done:` and its translations).
#[must_use]
pub(crate) fn declares_done(text: &str) -> bool {
    let folded = fold(text);
    folded.lines().any(|line| {
        let l = line.trim_start_matches(|c: char| !c.is_alphanumeric());
        DONE_MARKERS.iter().any(|m| l.starts_with(m))
    })
}

/// The tail of the reply talks about money.
#[must_use]
pub(crate) fn mentions_money(text: &str) -> bool {
    let folded = fold(tail_chars(text, 500));
    let padded = format!(" {folded} ");
    MONEY_WORDS.iter().any(|w| padded.contains(w))
}

/// The reply ends in a question that asks leave to carry on (or to do more).
/// A question about the task itself ("which address?") does not match.
#[must_use]
pub(crate) fn asks_permission_to_continue(text: &str) -> bool {
    let folded = fold(text.trim());
    let Some(q) = folded.rfind('?') else {
        // "Devam edeyim." without a question mark is still an offer to ask leave;
        // only the explicit "?" form counts, which keeps this rule narrow.
        return false;
    };
    // The question is the last thing said: at most a short line follows it.
    if folded[q..].chars().count() > 80 {
        return false;
    }
    let head = &folded[..q];
    let window = format!(" {}", tail_chars(head, 240));
    // An offer that stops short ("..., want me to?") counts when nothing is asked about it;
    // Turkish "ister misin" only after a word that offers to carry on.
    let head = head.trim_end();
    let bare = ASK_ENDINGS.iter().any(|e| {
        head.ends_with(e)
            && (!e.starts_with("ister")
                || ["devam", "gec", "yap", "baslay", "sonra", "diger"]
                    .iter()
                    .any(|w| tail_chars(head, 60).contains(w)))
    });
    bare || ASK_PHRASES.iter().any(|p| window.contains(p))
}

/// The reply ends in any question (real or not).
#[must_use]
pub(crate) fn ends_with_question(text: &str) -> bool {
    let folded = fold(text.trim());
    folded
        .rfind('?')
        .is_some_and(|q| folded[q..].chars().count() <= 80)
}

/// What to do with a turn that just ended. `open_steps` counts plan steps
/// that are not completed; `full` is the Full level.
#[must_use]
pub(crate) fn verdict(text: &str, open_steps: usize, full: bool) -> Option<Reason> {
    if mentions_money(text) {
        return None;
    }
    let done = declares_done(text);
    if full && !done && asks_permission_to_continue(text) {
        return Some(Reason::AskedPermission);
    }
    if open_steps > 0 && !done && !ends_with_question(text) {
        return Some(Reason::OpenPlan);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_asks_in_four_languages_are_detected() {
        for text in [
            "I registered on the first site. Should I continue with the next one?",
            "Done with site 1 of 15. Shall I proceed?",
            "I did one, shall I do the next?",
            "I can also research other companies, want me to?",
            "Would you like me to continue with the remaining sites?",
            "Do you want me to go on?",
            "Let me know if you'd like me to continue.\n\nReady for the next?",
            "İlk siteye kayıt oldum. Devam edeyim mi?",
            "Birini yaptım, sonrakine geçeyim mi?",
            "Diğer şirketleri de araştırmamı ister misin? Sonraki siteye geçmemi istersen yaparım?",
            "Başlayayım mı?",
            "Soll ich weitermachen?",
            "Möchten Sie, dass ich auch die anderen Seiten erledige?",
            "He terminado la primera. ¿Quieres que continúe?",
            "¿Sigo?",
        ] {
            assert!(asks_permission_to_continue(text), "{text}");
            assert_eq!(
                verdict(text, 0, true),
                Some(Reason::AskedPermission),
                "{text}"
            );
        }
    }

    #[test]
    fn real_questions_and_courtesies_are_not_permission_asks() {
        for text in [
            "Which e-mail address should I use for the sign-ups?",
            "Hangi e-posta adresini kullanmamı istersin?",
            "I need the password of the account. Can you give it to me?",
            "Is there anything else I can help you with?",
            "Başka bir isteğin var mı?",
            "Welche E-Mail-Adresse soll ich verwenden?",
            "Done. All 12 accounts are created.",
            "I will continue with the next site.",
            "",
        ] {
            assert!(!asks_permission_to_continue(text), "{text}");
            assert_eq!(verdict(text, 0, true), None, "{text}");
        }
    }

    #[test]
    fn a_question_that_is_not_the_last_thing_said_does_not_count() {
        let text = format!(
            "Shall I continue? {}",
            "Anyway, here is a long report of everything that happened so far. ".repeat(4)
        );
        assert!(!asks_permission_to_continue(&text));
    }

    #[test]
    fn money_never_continues() {
        for text in [
            "The plan costs $20 a month. Shall I continue and pay?",
            "Ödeme gerekiyor. Devam edeyim mi?",
            "I need your credit card to continue. Shall I proceed?",
            "Soll ich die Zahlung fortsetzen? Soll ich weitermachen?",
        ] {
            assert!(mentions_money(text), "{text}");
            assert_eq!(verdict(text, 3, true), None, "{text}");
        }
        assert!(!mentions_money(
            "Registered on example.com. Shall I continue?"
        ));
    }

    #[test]
    fn done_marker_ends_the_turn_even_with_an_open_plan() {
        for text in [
            "Done: all 15 sites.",
            "**Done:** 12 accounts, 3 blocked.",
            "Tamamlandı: 12 hesap açıldı.",
            "Fertig: alles erledigt.",
        ] {
            assert!(declares_done(text), "{text}");
            assert_eq!(verdict(text, 2, true), None, "{text}");
        }
        assert!(!declares_done("Not done yet, still working on it."));
    }

    #[test]
    fn open_plan_without_a_question_continues_at_every_level() {
        let text = "Site 1 is registered. Site 2 uses a CAPTCHA, so I stopped there.";
        assert_eq!(verdict(text, 3, false), Some(Reason::OpenPlan));
        assert_eq!(verdict(text, 3, true), Some(Reason::OpenPlan));
        assert_eq!(
            verdict(text, 0, true),
            None,
            "no open plan, nothing to continue"
        );
    }

    #[test]
    fn open_plan_with_a_real_question_stops() {
        assert_eq!(verdict("Which e-mail address should I use?", 4, true), None);
        assert_eq!(
            verdict("Hangi e-posta adresini kullanayım?", 4, false),
            None
        );
    }

    #[test]
    fn permission_asks_only_continue_at_the_full_level() {
        let text = "I registered on the first site. Shall I continue?";
        assert_eq!(verdict(text, 2, false), None);
        assert_eq!(verdict(text, 2, true), Some(Reason::AskedPermission));
    }

    #[test]
    fn continue_text_is_honest_and_bounded() {
        let ask = continue_text(Reason::AskedPermission, 0);
        assert!(ask.contains("not typed by the owner") && ask.contains("yes, continue"));
        assert!(ask.contains("Never move money") && ask.contains("blocked"));
        let plan = continue_text(Reason::OpenPlan, 3);
        assert!(plan.contains("3 open step(s)"));
        assert!(plan.len() < 900, "{}", plan.len());
    }

    #[test]
    fn fold_handles_turkish_dotted_capital_i() {
        assert!(asks_permission_to_continue(
            "İLK SİTE TAMAM. DEVAM EDEYİM Mİ?"
        ));
    }
}
