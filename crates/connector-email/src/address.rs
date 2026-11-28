//! What counts as a plain, honest e-mail address, whatever a parser crate lets through.
//!
//! `email_address` accepts any non-ASCII character in an address, and IDNA drops zero-width
//! characters from a domain, so `ann@exam<ZWSP>ple.com` would read as `ann@example.com` while
//! being a different mailbox. The address is checked here, not left to a crate's behaviour.

/// Whether `c` takes no space on screen or only changes how the text around it is drawn, so
/// that two different addresses can look the same: control and format characters (zero-width,
/// bidirectional controls, the soft hyphen, the byte-order mark, tag characters), separators
/// other than the plain space, variation selectors, fillers, private-use characters and
/// non-characters.
pub(crate) fn is_hidden_char(c: char) -> bool {
    c.is_control()
        || c.is_whitespace()
        || matches!(
            u32::from(c),
            0x00AD
                | 0x034F
                | 0x0600..=0x0605
                | 0x061C
                | 0x06DD
                | 0x070F
                | 0x0890..=0x0891
                | 0x08E2
                | 0x115F..=0x1160
                | 0x17B4..=0x17B5
                | 0x180B..=0x180F
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x206F
                | 0x2800
                | 0x3164
                | 0xE000..=0xF8FF
                | 0xFDD0..=0xFDEF
                | 0xFE00..=0xFE0F
                | 0xFEFF
                | 0xFFA0
                | 0xFFF0..=0xFFFB
                | 0x110BD
                | 0x110CD
                | 0x13430..=0x1343F
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0000..=0xE0FFF
                | 0xF0000..=0x0010_FFFF
        )
        // The last two code points of every plane are non-characters.
        || (u32::from(c) & 0xFFFE) == 0xFFFE
}

/// Whether `s` has a character that is invisible or looks like nothing (see [`is_hidden_char`]).
pub(crate) fn has_hidden_char(s: &str) -> bool {
    s.chars().any(is_hidden_char)
}

/// Whether `s` is one valid address (`local@domain`, no display name) with nothing invisible in it.
pub(crate) fn is_valid_address(s: &str) -> bool {
    !has_hidden_char(s) && s.parse::<email_address::EmailAddress>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invisible_characters_are_hidden_and_ordinary_text_is_not() {
        for c in [
            '\u{0}',
            '\u{7F}',
            '\u{85}',
            '\u{A0}',
            '\u{AD}',
            '\u{34F}',
            '\u{61C}',
            '\u{180E}',
            '\u{2003}',
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{200E}',
            '\u{200F}',
            '\u{2028}',
            '\u{2029}',
            '\u{202A}',
            '\u{202E}',
            '\u{2060}',
            '\u{2066}',
            '\u{2069}',
            '\u{206F}',
            '\u{3000}',
            '\u{3164}',
            '\u{FE0F}',
            '\u{FEFF}',
            '\u{FFA0}',
            '\u{FFF9}',
            '\u{FFFE}',
            '\u{E000}',
            '\u{1D173}',
            '\u{1FFFF}',
            '\u{E0001}',
            '\u{E0041}',
            '\u{F0000}',
        ] {
            assert!(is_hidden_char(c), "{:X}", u32::from(c));
        }
        for c in [
            'a', 'Z', '0', '@', '.', '+', '-', '_', 'é', 'ğ', 'ş', 'ı', '中', 'я', '😀',
        ] {
            assert!(!is_hidden_char(c), "{c}");
        }
    }

    #[test]
    fn valid_address_refuses_hidden_characters_in_either_part() {
        assert!(is_valid_address("ann@example.com"));
        assert!(is_valid_address("ağa@exämple.com"));
        for bad in [
            "ann@exam\u{200B}ple.com",
            "an\u{200B}n@example.com",
            "ann@example.com\u{200B}",
            "\u{FEFF}ann@example.com",
            "ann@example\u{202E}moc.com",
            "ann\u{2060}@example.com",
            "ann@example\u{AD}.com",
            "ann@exam\u{E0041}ple.com",
        ] {
            assert!(!is_valid_address(bad), "{bad:?}");
        }
    }
}
