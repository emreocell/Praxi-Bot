//! Lexical helpers for tool discovery: stemming-lite and a small EN + TR
//! synonym table. Used only by [`crate::discovery`]; independent of the
//! `ladder` module.
//!
//! Both indexed documents and queries are stemmed, so `typing`, `typed` and
//! `type` meet; queries are additionally expanded with the synonyms of every
//! term (`klavye` also looks for `keyboard`, `type`, `yaz`, ...).

/// Groups of interchangeable terms (lowercase; Turkish with and without
/// diacritics, since users often type ASCII).
const SYNONYM_GROUPS: &[&[&str]] = &[
    &[
        "keyboard", "klavye", "type", "yaz", "typing", "write", "key", "text",
    ],
    &[
        "open", "aç", "ac", "launch", "run", "başlat", "baslat", "start",
    ],
    &["click", "tıkla", "tikla", "tap"],
    &["file", "dosya"],
    &["folder", "klasör", "klasor", "directory"],
    &["window", "pencere"],
    &["process", "işlem", "islem"],
];

/// Turkish case/inflection suffixes tried on non-ASCII words, longest first.
/// A suffix is only removed when at least three characters of stem remain.
const TR_SUFFIXES: &[&str] = &[
    "lerini", "larını", "lerin", "ların", "leri", "ları", "ler", "lar", "nin", "nın", "nun", "nün",
    "dan", "den", "tan", "ten", "yı", "yi", "yu", "yü", "da", "de", "ta", "te", "ı", "i", "u", "ü",
];

/// The few Turkish suffixes that are rare enough in English to also strip from
/// ASCII words (with at least four characters of stem left) (users often type Turkish without diacritics).
const TR_ASCII_SUFFIXES: &[&str] = &["lerini", "larini", "leri", "lari", "ler", "lar", "yi", "yu"];

/// Reduce a lowercase term to a crude stem: common English (`ing`, `ed`, `es`,
/// `s`, trailing `e`, doubled final consonant) and Turkish suffixes are removed.
/// Stems under three characters are left alone.
#[must_use]
pub fn stem(term: &str) -> String {
    let chars: Vec<char> = term.chars().collect();
    if chars.len() <= 3 {
        return term.to_owned();
    }
    let mut s = term.to_owned();
    // Possibly Turkish: strip one inflectional suffix.
    let (tr, min_stem): (&[&str], usize) = if s.is_ascii() {
        (TR_ASCII_SUFFIXES, 4)
    } else {
        (TR_SUFFIXES, 3)
    };
    for suf in tr {
        if let Some(rest) = s.strip_suffix(suf)
            && rest.chars().count() >= min_stem
        {
            return finish(rest.to_owned());
        }
    }
    for suf in ["ing", "ed", "es"] {
        if let Some(rest) = s.strip_suffix(suf)
            && rest.chars().count() >= 3
        {
            let mut stem = rest.to_owned();
            collapse_double(&mut stem);
            return finish(stem);
        }
    }
    if s.ends_with('s') && !s.ends_with("ss") {
        s.pop();
    }
    finish(s)
}

fn finish(mut s: String) -> String {
    if s.chars().count() > 3 && s.ends_with('e') {
        s.pop();
    }
    s
}

fn collapse_double(s: &mut String) {
    let mut it = s.chars().rev();
    if let (Some(a), Some(b)) = (it.next(), it.next())
        && a == b
        && !matches!(a, 'l' | 's' | 'z')
    {
        s.pop();
    }
}

/// Stems of `terms`.
#[must_use]
pub fn stem_all(terms: &[String]) -> Vec<String> {
    terms.iter().map(|t| stem(t)).collect()
}

/// Stems of `terms` plus the stemmed synonyms of each term (duplicates removed,
/// original terms first).
#[must_use]
pub fn expand_query(terms: &[String]) -> Vec<String> {
    let mut out = stem_all(terms);
    let mut extra = Vec::new();
    for t in terms {
        let st = stem(t);
        for group in SYNONYM_GROUPS {
            if group.iter().any(|g| *g == t || stem(g) == st) {
                extra.extend(group.iter().map(|g| stem(g)));
            }
        }
    }
    for e in extra {
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Vec<String> {
        crate::bm25::tokenize(s)
    }

    #[test]
    fn stems_english_and_turkish_forms_together() {
        assert_eq!(stem("typing"), stem("type"));
        assert_eq!(stem("typed"), stem("type"));
        assert_eq!(stem("clicks"), stem("click"));
        assert_eq!(stem("running"), stem("run"));
        assert_eq!(stem("files"), stem("file"));
        assert_eq!(stem("processes"), stem("process"));
        assert_eq!(stem("dosyayı"), stem("dosya"));
        assert_eq!(stem("klavyeyi"), stem("klavye"));
        assert_eq!(stem("klasörü"), stem("klasör"));
        assert_eq!(stem("pencereler"), stem("pencere"));
        assert_eq!(stem("id"), "id");
        assert_eq!(stem("process"), "process");
    }

    #[test]
    fn synonyms_expand_across_languages() {
        let e = expand_query(&q("klavye"));
        for want in ["keyboard", "typ", "yaz"] {
            assert!(e.contains(&stem(want)), "{want} missing in {e:?}");
        }
        let e = expand_query(&q("dosyayı aç"));
        assert!(e.contains(&stem("file")) && e.contains(&stem("launch")));
        assert_eq!(expand_query(&q("zzz")).len(), 1);
    }
}
