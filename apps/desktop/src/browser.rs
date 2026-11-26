//! Narrow system-browser opener, available only to the main window.

fn allowed(href: &str, dashboard: bool) -> bool {
    let Some(rest) = href.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !href.chars().any(|c| c.is_control() || c == '\\')
        && if dashboard {
            matches!(host, "platform.composio.dev" | "dashboard.composio.dev")
        } else {
            matches!(host, "connect.composio.dev" | "backend.composio.dev")
                || rest
                    .strip_prefix("app.composio.dev/link/")
                    .and_then(|path| path.split(['?', '#']).next())
                    .is_some_and(|token| {
                        !token.is_empty()
                            && token.len() <= 128
                            && token
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    })
        }
}

/// Validate and hand a URL to the operating system browser.
///
/// # Errors
/// Unapproved URL or OS opener failure. Success means OS accepted the request.
pub fn open_composio(href: &str, dashboard: bool) -> Result<(), String> {
    if !allowed(href, dashboard) {
        return Err("unapproved Composio URL".into());
    }
    open(href)
}

/// Longest sign-in address handed to the browser (authorization URLs carry a
/// PKCE challenge, a state and scopes).
const MAX_SIGN_IN_URL: usize = 4096;

fn sign_in_allowed(href: &str) -> bool {
    let Some(rest) = href.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    href.len() <= MAX_SIGN_IN_URL
        && !authority.is_empty()
        && !authority.contains(['@', '%'])
        && !href
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
}

/// Validate and hand a sign-in page to the operating system browser: any
/// `https` address without credentials in it. The address comes from the
/// daemon (the authorization page of a sign-in the owner started, which the
/// sign-in panel shows by origin and confirms when it is not the app's own
/// site) or from the app's own setup guide (Google Cloud console pages); a
/// plain `http` or `file` address, or one with a user name in front of the
/// host, is refused. Callers must never pass an address taken from a tool
/// result, a catalog `homepage` or other text a server supplies: this check
/// cannot tell whose site it is.
///
/// # Errors
/// Unapproved URL or OS opener failure.
pub fn open_sign_in(href: &str) -> Result<(), String> {
    if !sign_in_allowed(href) {
        return Err("unapproved sign-in URL".into());
    }
    open(href)
}

fn open(href: &str) -> Result<(), String> {
    #[cfg(windows)]
    let result = open::that_detached(href);
    #[cfg(not(windows))]
    let result = open::that(href);
    result.map_err(|_| "system browser could not be opened".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_sign_in_page_is_https_without_credentials_or_control_characters() {
        assert!(sign_in_allowed(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=x&state=y"
        ));
        assert!(sign_in_allowed(
            "https://console.cloud.google.com/auth/clients/create"
        ));
        for bad in [
            "http://accounts.google.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user@accounts.google.com/",
            "https://accounts.google.com\\@evil/",
            "https://%61ccounts.google.com/",
            "https:///path",
            "https://a.example/ b",
            "https://a.example/\n",
        ] {
            assert!(!sign_in_allowed(bad), "{bad}");
        }
        assert!(!sign_in_allowed(&format!(
            "https://a.example/{}",
            "a".repeat(MAX_SIGN_IN_URL)
        )));
    }

    #[test]
    fn exact_browser_hosts_and_separate_dashboard() {
        assert!(allowed("https://connect.composio.dev/link/x", false));
        assert!(allowed("https://app.composio.dev/link/lt_example", false));
        assert!(!allowed("https://app.composio.dev/link/lt_example", true));
        assert!(allowed("https://platform.composio.dev/", true));
        assert!(allowed("https://dashboard.composio.dev/?cliKey=abc", true));
        assert!(!allowed("https://dashboard.composio.dev/", false));
        assert!(!allowed("https://platform.composio.dev/", false));
        for bad in [
            "https://app.composio.dev/",
            "https://app.composio.dev/link/../settings",
            "https://app.composio.dev/link/lt_example/other",
            "https://logos.composio.dev/x",
            "https://unexpected.composio.dev/x",
            "https://user@connect.composio.dev/x",
            "https://connect.composio.dev:443/x",
            "https://%63onnect.composio.dev/x",
            "https://connect.composio.dev.evil/x",
            "https://connect.composio.dev\\@evil/x",
        ] {
            assert!(!allowed(bad, false), "{bad}");
        }
    }
}
