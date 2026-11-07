//! Turkish-aware text normalisation shared by the cache key, rules and BM25.
//!
//! Plain `to_lowercase` is wrong for Turkish: `İ` lowercases to `i` + a
//! combining dot (U+0307) and `I` / `ı` never meet. For *matching* we fold
//! both languages onto one ASCII-ish alphabet instead: `İ ı I i` all become
//! `i`, and `ş ğ ü ö ç` lose their diacritics, so `"İSTANBUL"`, `"istanbul"`
//! and `"Istanbul"` are the same key.

use crate::bm25::tokenize;

/// Fold diacritics / dotted-dotless i, keeping letter case (so camelCase
/// boundaries stay visible to the tokenizer).
fn prefold(text: &str) -> String {
    text.chars()
        .filter_map(|c| {
            Some(match c {
                // combining marks (decomposed diacritics, the dot of `İ`)
                '\u{0300}'..='\u{036F}' => return None,
                'İ' | 'Î' => 'I',
                'ı' | 'î' => 'i',
                'ş' | 'ș' => 's',
                'Ş' | 'Ș' => 'S',
                'ğ' => 'g',
                'Ğ' => 'G',
                'ü' | 'û' => 'u',
                'Ü' | 'Û' => 'U',
                'ö' => 'o',
                'Ö' => 'O',
                'ç' => 'c',
                'Ç' => 'C',
                'â' => 'a',
                'Â' => 'A',
                other => other,
            })
        })
        .collect()
}

/// Canonical form of a user utterance: folded, lowercase, punctuation-free
/// words separated by single spaces.
#[must_use]
pub fn normalize(text: &str) -> String {
    terms(text).join(" ")
}

/// Folded lowercase word terms (camelCase and separators split).
#[must_use]
pub fn terms(text: &str) -> Vec<String> {
    tokenize(&prefold(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_casing_collapses() {
        let a = normalize("İSTANBUL'a Işık GÖNDER");
        let b = normalize("istanbul'a ısık gonder");
        assert_eq!(a, b);
        assert_eq!(a, "istanbul a isik gonder");
    }

    #[test]
    fn decomposed_dotted_i_is_handled() {
        // "İ" as I + U+0307
        assert_eq!(normalize("I\u{307}stanbul"), "istanbul");
        assert_eq!(normalize("  Hello,   WORLD! "), "hello world");
    }

    #[test]
    fn diacritics_fold() {
        assert_eq!(normalize("Şu çağrı üç öğe"), "su cagri uc oge");
    }
}
