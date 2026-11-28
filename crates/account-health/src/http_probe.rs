//! Cheap GET of a ledger `evidence_url`: looks for sign-in redirects.

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(12);
const LOGIN_MARKERS: &[&str] = &[
    "login", "signin", "sign-in", "sign_in", "log-in", "session", "auth",
];

/// `true` when the page still looks like a signed-in view (best effort, offline-friendly).
#[must_use]
pub fn evidence_still_valid(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return true;
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return true;
    }
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(8))
        .user_agent("praxi-account-health")
        .build()
    else {
        return true;
    };
    let Ok(resp) = client.get(parsed.clone()).send() else {
        return true;
    };
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return false;
    }
    let final_url = resp.url().clone();
    let path = final_url.path().to_ascii_lowercase();
    if LOGIN_MARKERS.iter().any(|m| path.contains(m)) {
        return false;
    }
    if final_url.host_str() != parsed.host_str()
        && LOGIN_MARKERS
            .iter()
            .any(|m| final_url.path().to_ascii_lowercase().contains(m))
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn login_redirect_counts_as_expired() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/login"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let url = format!("{}/me", server.uri());
        let expired = tokio::task::spawn_blocking(move || evidence_still_valid(&url))
            .await
            .unwrap();
        assert!(!expired);
    }
}
