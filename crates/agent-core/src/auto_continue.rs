//! Auto-continue: a turn that ends with work left is continued by the runtime.
//!
//! A model often finishes one item and stops with "Shall I do the next one?" or
//! "I can also research other companies, want me to?". The owner then types
//! "go on" after every step. This module decides, from the model's closing
//! text and its plan, whether the turn really ended or merely asked for
//! permission to continue. It is a pure text and counting rule; the loop applies it only
//! to the owner's own top-level turn, inside the step, token and time budgets, and never for
//! money.
//!
//! * A reply that **asks permission to continue** ("shall I continue / do the next one?") is
//!   answered "yes" at the Full level (the owner chose to work alone), unless the run has read
//!   untrusted content: a page or a mail can plant such a question, and an automatic "yes" is
//!   the owner's voice.
//! * A reply that ends **without a question while the plan still has open
//!   steps** is continued (a progress report is not the end).
//! * A **real question** ("which e-mail address?"), a generic consent question ("shall I go
//!   ahead?"), an offer of more work ("want me to also ...?"), a **finished task**
//!   (`Done:`) and anything that mentions **money, a purchase or deleting** anywhere in the
//!   reply end the turn as before.
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

/// Phrases of "may I continue / do the next one?" (folded). Each names *continuing*; a generic
/// consent question ("should I go ahead?", "want me to sign up?", "shall I?") is not in the list
/// on purpose: only the owner answers those, and so does an offer to do *more* ("want me to
/// also ...?"), which would widen the task. "Is there anything else?" is a courtesy, not a
/// request to continue.
const ASK_PHRASES: &[&str] = &[
    // English
    "should i continue",
    "shall i continue",
    "should i proceed",
    "shall i proceed",
    "should i go on",
    "shall i go on",
    "should i keep going",
    "shall i keep going",
    "should i move on",
    "shall i move on",
    "should i carry on",
    "shall i carry on",
    "should i start",
    "shall i start",
    "should i do the next",
    "shall i do the next",
    "should i do the rest",
    "shall i do the rest",
    "want me to continue",
    "want me to proceed",
    "want me to go on",
    "want me to keep going",
    "want me to move on",
    "want me to carry on",
    "want me to do the next",
    "want me to do the rest",
    "want me to start",
    "like me to continue",
    "like me to proceed",
    "like me to go on",
    "like me to keep going",
    "like me to move on",
    "like me to carry on",
    "like me to do the next",
    "like me to do the rest",
    "like me to start",
    "do you want me to continue",
    "do you want me to proceed",
    "do you want me to go on",
    "do you want me to keep going",
    "do you want me to move on",
    "do you want me to carry on",
    "do you want me to start",
    "do you want to continue",
    "do you want to proceed",
    "ok to continue",
    "ok to proceed",
    "okay to continue",
    "okay to proceed",
    "ready to continue",
    "ready for the next",
    "continue with the next",
    "proceed with the next",
    "go ahead with the next",
    "move on to the next",
    "move to the next",
    // Turkish
    "devam edeyim",
    "devam edelim",
    "devam etmemi ister",
    "devam etmeme izin",
    "devam etmek ister",
    "devam mi",
    "devam edebilirim",
    "sonrakine gecmemi ister",
    "sonrakine gecelim mi",
    "sonrakine geceyim mi",
    "sonraki siteye gecmemi ister",
    "sonraki siteye gecelim mi",
    "sonraki siteye geceyim mi",
    "sonraki adima gecmemi ister",
    "sonraki adima gecelim mi",
    "sonraki adima geceyim mi",
    "kalanlara devam",
    "baslayayim mi",
    "baslamami ister",
    "baslayalim mi",
    "ilerleyeyim mi",
    "ilerlememi ister",
    // German
    "soll ich weitermachen",
    "soll ich fortfahren",
    "soll ich weiter",
    "soll ich mit dem nachsten",
    "soll ich als nachstes",
    "soll ich den rest",
    "soll ich anfangen",
    "soll ich starten",
    "mochten sie, dass ich weitermache",
    "mochtest du, dass ich weitermache",
    "willst du, dass ich weitermache",
    "wollen sie, dass ich weitermache",
    "darf ich weitermachen",
    "darf ich fortfahren",
    // Spanish
    "quieres que continue",
    "quieres que siga",
    "quieres que proceda",
    "deseas que continue",
    "desea que continue",
    "deseas que siga",
    "debo continuar",
    "debo seguir",
    "debo proceder",
    "sigo con el siguiente",
    "sigo con la siguiente",
    "continuo con el siguiente",
    "continuo con la siguiente",
    "paso al siguiente",
    "paso a la siguiente",
];

/// A whole last question that is nothing but a short offer to carry on ("Ready for the next?",
/// "¿Sigo?"). Compared with the folded sentence, trimmed.
const ASK_BARE: &[&str] = &[
    "continue",
    "go on",
    "proceed",
    "keep going",
    "continue then",
    "devam",
    "weiter",
    "sigo",
    "continuo",
    "procedo",
];

/// First words that make a question a real question about the task ("which address should I
/// continue with?"), never a request for leave to carry on.
const WH_WORDS: &[&str] = &[
    "which", "what", "where", "who", "whom", "whose", "when", "why", "how", "hangi", "ne",
    "nerede", "nasil", "kim", "nereye", "welche", "welcher", "welches", "welchen", "was", "wo",
    "wie", "wann", "warum", "wer", "cual", "que", "donde", "como", "quien", "cuando",
];

/// Whole words (folded) that mean money, a payment or a purchase may be in play. A reply that
/// has one anywhere is never answered automatically: spending always stops for the owner.
const MONEY_WORDS: &[&str] = &[
    // English
    "pay",
    "paid",
    "paying",
    "payment",
    "payments",
    "purchase",
    "purchases",
    "purchased",
    "buy",
    "buying",
    "bought",
    "checkout",
    "subscribe",
    "subscribed",
    "subscription",
    "subscriptions",
    "billing",
    "bill",
    "invoice",
    "invoices",
    "credit",
    "debit",
    "transfer",
    "transfers",
    "transferred",
    "wire",
    "wired",
    "bank",
    "banking",
    "iban",
    "fee",
    "fees",
    "charge",
    "charged",
    "charges",
    "charging",
    "price",
    "prices",
    "priced",
    "pricing",
    "order",
    "orders",
    "ordered",
    "ordering",
    "crypto",
    "cryptocurrency",
    "bitcoin",
    "btc",
    "usdt",
    "wallet",
    "donate",
    "donation",
    "donations",
    "refund",
    "usd",
    "eur",
    "gbp",
    "tl",
    "dollar",
    "dollars",
    "euro",
    "euros",
    "pound",
    "pounds",
    "lira",
    // Turkish
    "odeme",
    "odemeyi",
    "odemek",
    "odemesi",
    "odeyim",
    "satin",
    "abonelik",
    "aboneligi",
    "kredi",
    "fatura",
    "faturayi",
    "havale",
    "eft",
    "ucret",
    "ucreti",
    "fiyat",
    "fiyati",
    "para",
    "parayi",
    "paranin",
    "banka",
    "komisyon",
    "bagis",
    "dolar",
    "siparis",
    // German
    "bezahlen",
    "bezahle",
    "bezahlung",
    "zahlung",
    "zahlungen",
    "kaufen",
    "kaufe",
    "kauf",
    "rechnung",
    "abo",
    "abonnement",
    "gebuhr",
    "gebuhren",
    "preis",
    "preise",
    "uberweisung",
    "uberweisen",
    "bestellung",
    "bestellen",
    "spende",
    // Spanish
    "pago",
    "pagos",
    "pagar",
    "comprar",
    "compra",
    "compras",
    "suscripcion",
    "factura",
    "transferencia",
    "banco",
    "precio",
    "precios",
    "tarifa",
    "pedido",
    "donacion",
    "reembolso",
    "cripto",
    "tarjeta",
];

/// Whole words (folded) for deleting or destroying something, or handing out a credential.
const DESTRUCTIVE_WORDS: &[&str] = &[
    // English
    "delete",
    "deleted",
    "deleting",
    "remove",
    "removing",
    "erase",
    "erasing",
    "wipe",
    "wiping",
    "drop",
    "destroy",
    "purge",
    "overwrite",
    "uninstall",
    "revoke",
    "terminate",
    // Turkish
    "sil",
    "silmek",
    "silmeyi",
    "silmem",
    "silmemi",
    "sileyim",
    "silelim",
    "silinsin",
    "silinecek",
    "kaldir",
    "kaldirmak",
    "kaldirayim",
    "temizle",
    // German
    "loschen",
    "loeschen",
    "entfernen",
    "entferne",
    // Spanish
    "eliminar",
    "elimine",
    "borrar",
    "borre",
    "quitar",
];

/// Words that, in the closing QUESTION itself, say the model wants to do something that leaves
/// the owner's hands (send, publish, hand out a credential). Not scanned in the rest of the
/// reply: a report may say "I sent the form".
const HARD_STOP_WORDS: &[&str] = &[
    // English
    "send",
    "sending",
    "email",
    "mail",
    "message",
    "publish",
    "post",
    "posting",
    "tweet",
    "password",
    "credentials",
    "credential",
    "token",
    "secret",
    // Turkish
    "gonder",
    "gonderelim",
    "gonderivereyim",
    "yayinla",
    "mesaj",
    "sifre",
    "parola",
    // German
    "senden",
    "schicken",
    "veroffentlichen",
    "passwort",
    // Spanish
    "enviar",
    "envio",
    "publicar",
    "mensaje",
    "contrasena",
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

/// The folded words of `text` (everything that is not a letter or digit separates words).
fn words(folded: &str) -> Vec<&str> {
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

/// One of `list` is a whole word of `folded`. "in order (to)" and "in charge" are not about money.
fn has_word(folded: &str, list: &[&str]) -> bool {
    let ws = words(folded);
    ws.iter().enumerate().any(|(i, w)| {
        list.contains(w) && !(matches!(*w, "order" | "charge") && i > 0 && ws[i - 1] == "in")
    })
}

/// A currency sign next to a number (`$20`, `20 €`, `₺5`): an amount, not a shell variable.
fn has_amount(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, c)| {
        matches!(c, '$' | '€' | '£' | '₺' | '¥')
            && (chars[i + 1..]
                .iter()
                .find(|c| !c.is_whitespace())
                .is_some_and(char::is_ascii_digit)
                || chars[..i]
                    .iter()
                    .rev()
                    .find(|c| !c.is_whitespace())
                    .is_some_and(char::is_ascii_digit))
    })
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

/// Anywhere in the WHOLE reply: money or a purchase (amounts, payment, banking, prices, fees,
/// orders, crypto, in English, Turkish, German and Spanish) or deleting something. The reply is
/// never answered automatically then; the owner decides.
#[must_use]
pub(crate) fn needs_owner(text: &str) -> bool {
    let folded = fold(text);
    has_amount(text) || has_word(&folded, MONEY_WORDS) || has_word(&folded, DESTRUCTIVE_WORDS)
}

/// The last sentence of `head` (what comes before the final `?`): from the last sentence
/// boundary (`.`, `!`, `?`, `:` followed by white space, or a line break) on.
fn last_sentence(head: &str) -> &str {
    let mut start = 0;
    let mut chars = head.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = c == '\n'
            || (matches!(c, '.' | '!' | '?' | ':')
                && chars.peek().is_none_or(|(_, n)| n.is_whitespace()));
        if boundary {
            start = i + c.len_utf8();
        }
    }
    head[start..].trim()
}

/// The reply ends in a question that asks leave to carry on with the work it was given. A
/// question about the task itself ("which address?"), a generic consent question ("shall I go
/// ahead?"), an offer of more work and anything about sending, publishing or credentials do
/// not match.
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
    let sentence = last_sentence(&folded[..q]);
    let sentence = tail_chars(sentence, 200);
    let ws = words(sentence);
    let Some(first) = ws.first() else {
        return false;
    };
    if WH_WORDS.contains(first) || has_word(sentence, HARD_STOP_WORDS) {
        return false;
    }
    let window = format!(" {sentence}");
    ASK_BARE.contains(&sentence) || ASK_PHRASES.iter().any(|p| window.contains(p))
}

/// The reply ends in any question (real or not).
#[must_use]
pub(crate) fn ends_with_question(text: &str) -> bool {
    let folded = fold(text.trim());
    folded
        .rfind('?')
        .is_some_and(|q| folded[q..].chars().count() <= 80)
}

/// What to do with a turn that just ended. `open_steps` counts plan steps that are not
/// completed; `full` is the Full level; `untrusted` says the run has read untrusted content
/// (a page, a mail), after which a question in the reply might be one the content planted.
#[must_use]
pub(crate) fn verdict(
    text: &str,
    open_steps: usize,
    full: bool,
    untrusted: bool,
) -> Option<Reason> {
    if needs_owner(text) {
        return None;
    }
    let done = declares_done(text);
    // Answering "yes, continue" to a question is the owner's voice: never after untrusted
    // content, which can pose that question. A plan the run itself keeps open carries on.
    if full && !untrusted && !done && asks_permission_to_continue(text) {
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

    fn full(text: &str, open: usize) -> Option<Reason> {
        verdict(text, open, true, false)
    }

    #[test]
    fn permission_asks_in_four_languages_are_detected() {
        for text in [
            "I registered on the first site. Should I continue with the next one?",
            "Done with site 1 of 15. Shall I proceed?",
            "I did one, shall I do the next?",
            "Would you like me to continue with the remaining sites?",
            "Do you want me to go on?",
            "Let me know if you'd like me to continue.\n\nReady for the next?",
            "Site 3 is registered: should I keep going?",
            "İlk siteye kayıt oldum. Devam edeyim mi?",
            "Birini yaptım, sonrakine geçeyim mi?",
            "Başlayayım mı?",
            "Soll ich weitermachen?",
            "Möchten Sie, dass ich weitermache?",
            "He terminado la primera. ¿Quieres que continúe?",
            "¿Sigo?",
        ] {
            assert!(asks_permission_to_continue(text), "{text}");
            assert_eq!(full(text, 0), Some(Reason::AskedPermission), "{text}");
        }
    }

    #[test]
    fn real_questions_courtesies_and_generic_consent_are_not_permission_asks() {
        for text in [
            "Which e-mail address should I use for the sign-ups?",
            "Which address should I continue with?",
            "Hangi e-posta adresini kullanmamı istersin?",
            "I need the password of the account. Can you give it to me?",
            "Is there anything else I can help you with?",
            "Başka bir isteğin var mı?",
            "Welche E-Mail-Adresse soll ich verwenden?",
            "Done. All 12 accounts are created.",
            "I will continue with the next site.",
            "",
            // generic consent questions are the owner's to answer
            "The form is filled in. Should I go ahead?",
            "The form is filled in. Shall I?",
            "I found the sign-up page. Want me to sign up?",
            "Ready to submit. Want me to register?",
            "Formu doldurdum. Yapayım mı?",
            "Formu doldurdum. Edeyim mi?",
            "Başvuruyu göndereyim mi?",
            "Ich habe das Formular ausgefüllt. Soll ich es abschicken?",
            // an offer of MORE work widens the task
            "I can also research other companies, want me to?",
            "Want me to also contact their support?",
            "Möchten Sie, dass ich auch die anderen Seiten erledige?",
            "Diğer şirketleri de araştırmamı ister misin?",
            "¿Quieres que busque más empresas?",
            // sending or credentials are never "continue"
            "Shall I continue and send the report to the client?",
            "Should I continue and use your password?",
        ] {
            assert!(!asks_permission_to_continue(text), "{text}");
            assert_eq!(full(text, 0), None, "{text}");
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
    fn money_and_deleting_never_continue_wherever_they_appear() {
        for text in [
            "The plan costs $20 a month. Shall I continue and pay?",
            "Ödeme gerekiyor. Devam edeyim mi?",
            "I need your credit card to continue. Shall I proceed?",
            "Soll ich die Zahlung fortsetzen? Soll ich weitermachen?",
            "It needs an IBAN. Shall I continue?",
            "The registration fee is due. Shall I continue?",
            "They would charge my account. Should I proceed?",
            "Prices are listed on page 2. Shall I continue?",
            "I can wire the amount. Shall I continue?",
            "Havale yapılması gerekiyor. Devam edeyim mi?",
            "EFT ile ödenir. Devam edeyim mi?",
            "Ücret alınıyor. Devam edeyim mi?",
            "Überweisung nötig. Soll ich weitermachen?",
            "Hace falta una transferencia. ¿Quieres que continúe?",
            "Needs 5 € upfront. Shall I continue?",
            "The order form is next. Shall I continue?",
            "This accepts crypto only. Shall I continue?",
            "I would delete the old account first. Shall I continue?",
            "Eski hesabı silmem gerekiyor. Devam edeyim mi?",
        ] {
            assert!(needs_owner(text), "{text}");
            assert_eq!(full(text, 3), None, "{text}");
        }
        // The word is early in a long reply, far from the closing 500 characters.
        let long = format!(
            "The checkout page asked for payment.\n{}\nShall I continue?",
            "Everything else went fine. ".repeat(60)
        );
        assert!(needs_owner(&long));
        assert_eq!(full(&long, 0), None);
    }

    #[test]
    fn plain_words_and_shell_variables_are_not_money() {
        for text in [
            "Registered on example.com. Shall I continue?",
            "In order to continue I used $HOME for the config. Shall I continue?",
            "The team lead is in charge of the next steps. Shall I continue?",
            "Wireless support was checked. Shall I continue?",
        ] {
            assert!(!needs_owner(text), "{text}");
            assert_eq!(full(text, 0), Some(Reason::AskedPermission), "{text}");
        }
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
            assert_eq!(full(text, 2), None, "{text}");
        }
        assert!(!declares_done("Not done yet, still working on it."));
    }

    #[test]
    fn open_plan_without_a_question_continues_at_every_level() {
        let text = "Site 1 is registered. Site 2 uses a CAPTCHA, so I stopped there.";
        assert_eq!(verdict(text, 3, false, false), Some(Reason::OpenPlan));
        assert_eq!(verdict(text, 3, true, false), Some(Reason::OpenPlan));
        assert_eq!(
            verdict(text, 0, true, false),
            None,
            "no open plan, nothing to continue"
        );
    }

    #[test]
    fn open_plan_with_a_real_question_stops() {
        assert_eq!(full("Which e-mail address should I use?", 4), None);
        assert_eq!(
            verdict("Hangi e-posta adresini kullanayım?", 4, false, false),
            None
        );
    }

    #[test]
    fn permission_asks_only_continue_at_the_full_level() {
        let text = "I registered on the first site. Shall I continue?";
        assert_eq!(verdict(text, 2, false, false), None);
        assert_eq!(verdict(text, 2, true, false), Some(Reason::AskedPermission));
    }

    #[test]
    fn untrusted_content_only_allows_a_plan_driven_continuation() {
        // A page can plant "shall I continue?": the automatic yes would be the owner's voice.
        let ask = "I read the page. Shall I continue?";
        assert_eq!(verdict(ask, 0, true, true), None);
        assert_eq!(
            verdict(ask, 3, true, true),
            None,
            "a question is not plan-driven"
        );
        // A report without a question and an open plan of the run's own is plan-driven.
        let report = "Site 2 is registered. Site 3 is next.";
        assert_eq!(verdict(report, 2, true, true), Some(Reason::OpenPlan));
        assert_eq!(verdict(report, 0, true, true), None);
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
