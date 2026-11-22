//! `WebView2` media permission, separate from Tauri's IPC capability permissions.
//! Only the main app's own origin may capture audio; camera and other permissions
//! keep their existing browser policy. OS microphone privacy settings still apply.

#[cfg(windows)]
use tauri::Manager;

#[cfg(any(windows, test))]
fn trusted_origin(uri: &str, dev: bool) -> bool {
    let Ok(url) = tauri::Url::parse(uri) else {
        return false;
    };
    let app =
        url.scheme() == "http" && url.host_str() == Some("tauri.localhost") && url.port().is_none();
    let development = dev
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
        && url.port() == Some(1420);
    url.username().is_empty() && url.password().is_none() && (app || development)
}

#[cfg(windows)]
#[allow(clippy::default_trait_access)] // PWSTR comes from webview2-com's own windows version
#[allow(unsafe_code)] // WebView2 COM calls; no safe wrapper exists in tauri
pub fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW, COREWEBVIEW2_PERMISSION_STATE_DENY,
    };
    use webview2_com::{PermissionRequestedEventHandler, take_pwstr};

    if let Some(main) = app.get_webview_window("main") {
        main.with_webview(|webview| {
            // SAFETY: Tauri calls this on the webview thread; the controller and
            // event arguments are valid COM interfaces for that thread. WebView2
            // retains the handler until its webview is destroyed.
            let result = (|| unsafe {
                let core = webview.controller().CoreWebView2()?;
                let mut token = 0;
                core.add_PermissionRequested(
                    &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                        args.PermissionKind(&raw mut kind)?;
                        if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE {
                            let mut uri = Default::default();
                            args.Uri(&raw mut uri)?;
                            let uri = take_pwstr(uri);
                            // getUserMedia is invoked explicitly by the owner's
                            // call/dictation controls. Do not grant remote pages,
                            // widget/overlay webviews, or any camera permission.
                            args.SetState(if trusted_origin(&uri, cfg!(debug_assertions)) {
                                COREWEBVIEW2_PERMISSION_STATE_ALLOW
                            } else {
                                COREWEBVIEW2_PERMISSION_STATE_DENY
                            })?;
                        }
                        Ok(())
                    })),
                    &raw mut token,
                )
            })();
            if result.is_err() {
                eprintln!("bot-desktop: microphone permission handler unavailable");
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::trusted_origin;

    #[test]
    fn microphone_is_scoped_to_the_app_origin_and_exact_dev_port() {
        assert!(trusted_origin("http://tauri.localhost/index.html", false));
        assert!(trusted_origin("http://localhost:1420/", true));
        for uri in [
            "https://example.com/",
            "http://tauri.localhost.evil/",
            "http://tauri.localhost:8080/",
            "http://user@tauri.localhost/",
            "http://localhost:1421/",
        ] {
            assert!(!trusted_origin(uri, true), "{uri}");
        }
        assert!(!trusted_origin("http://localhost:1420/", false));
    }
}
