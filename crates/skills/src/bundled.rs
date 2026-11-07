//! Skills embedded from the repo `skills/` folder.

/// `(directory name, SKILL.md text)` for every bundled skill.
pub(crate) const BUNDLED: &[(&str, &str)] = &[
    (
        "windows-file-upload-dialog",
        include_str!("../../../skills/windows-file-upload-dialog/SKILL.md"),
    ),
    (
        "shadow-dom-and-iframes",
        include_str!("../../../skills/shadow-dom-and-iframes/SKILL.md"),
    ),
    (
        "custom-dropdowns",
        include_str!("../../../skills/custom-dropdowns/SKILL.md"),
    ),
    (
        "date-pickers",
        include_str!("../../../skills/date-pickers/SKILL.md"),
    ),
    (
        "cookie-consent-banners",
        include_str!("../../../skills/cookie-consent-banners/SKILL.md"),
    ),
    (
        "github-basics",
        include_str!("../../../skills/github-basics/SKILL.md"),
    ),
    (
        "windows-settings-navigation",
        include_str!("../../../skills/windows-settings-navigation/SKILL.md"),
    ),
];
