//! Conservative control-plane boundary, independent of catalog display classes.

pub(crate) fn toolkit_allowed(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && ![
            "composio",
            "code",
            "bash",
            "shell",
            "workbench",
            "tool_router",
            "auth",
            "control",
            "meta",
            "connect",
            "router",
            "browser",
            "automation",
            "connect_link",
            "session",
            "remote",
            "e2b",
            "python",
            "terminal",
            "proxy",
            "pipedream",
            "zapier",
            "n8n",
        ]
        .iter()
        .any(|p| slug.contains(p))
}

/// Read-only actions whose slug contains `AUTH` only as "the account that is
/// signed in" (not an authentication control surface). Exact `(toolkit, slug)`
/// pairs, vetted one by one: GitHub has many `..._THE_AUTHENTICATED_USER`
/// actions that change SSH/GPG keys, emails or the account itself, so no suffix
/// or pattern may be set aside from the `AUTH` check.
const READ_ONLY_SIGNED_IN: &[(&str, &str)] = &[
    ("github", "GITHUB_GET_THE_AUTHENTICATED_USER"),
    (
        "github",
        "GITHUB_LIST_ORGANIZATIONS_FOR_THE_AUTHENTICATED_USER",
    ),
];

pub(crate) fn action_allowed(toolkit: &str, slug: &str) -> bool {
    let vetted = READ_ONLY_SIGNED_IN.contains(&(toolkit, slug));
    toolkit_allowed(toolkit)
        && slug.starts_with(&format!("{}_", toolkit.to_ascii_uppercase()))
        && slug
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && ![
            "COMPOSIO",
            "EXECUTE",
            "EXECUTION",
            "BASH",
            "SHELL",
            "WORKBENCH",
            "CODE",
            "AUTH",
            "CONTROL",
            "META",
            "ROUTER",
            "CONNECT_LINK",
            "CONNECTION",
            "TOOL_ROUTER",
            "SESSION",
            "PROXY",
            "TERMINAL",
            "SCRIPT",
            "COMMAND",
            "EVAL",
            "WORKFLOW",
            "RUN_",
        ]
        .iter()
        .any(|p| slug.contains(p) && !(vetted && *p == "AUTH"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_and_execution_families_are_denied_independent_of_display_class() {
        for toolkit in [
            "composio",
            "composio_search",
            "codeinterpreter",
            "remote_workbench",
            "auth0",
            "tool_router",
            "control_plane",
            "metatools",
            "browserbase",
        ] {
            assert!(!toolkit_allowed(toolkit), "{toolkit}");
        }
        for action in [
            "COMPOSIO_EXECUTE_TOOL",
            "GITHUB_REMOTE_BASH",
            "GITHUB_REMOTE_WORKBENCH",
            "GITHUB_CREATE_CONNECT_LINK",
            "GITHUB_CREATE_AUTH_CONFIG",
            "GITHUB_GET_SESSION",
            "GITHUB_TOOL_ROUTER",
            "GITHUB_EVAL_SCRIPT",
        ] {
            assert!(!action_allowed("github", action), "{action}");
        }
        assert!(action_allowed("github", "GITHUB_LIST_REPOSITORY_ISSUES"));
    }

    #[test]
    fn only_vetted_read_only_signed_in_actions_are_set_aside_from_the_auth_check() {
        for ok in [
            "GITHUB_GET_THE_AUTHENTICATED_USER",
            "GITHUB_LIST_ORGANIZATIONS_FOR_THE_AUTHENTICATED_USER",
        ] {
            assert!(action_allowed("github", ok), "{ok}");
        }
        for denied in [
            "GITHUB_CREATE_A_PUBLIC_SSH_KEY_FOR_THE_AUTHENTICATED_USER",
            "GITHUB_DELETE_A_PUBLIC_SSH_KEY_FOR_THE_AUTHENTICATED_USER",
            "GITHUB_CREATE_A_GPG_KEY_FOR_THE_AUTHENTICATED_USER",
            "GITHUB_CREATE_A_SSH_SIGNING_KEY_FOR_THE_AUTHENTICATED_USER",
            "GITHUB_ADD_AN_EMAIL_ADDRESS_FOR_THE_AUTHENTICATED_USER",
            "GITHUB_DELETE_AN_EMAIL_ADDRESS_FOR_THE_AUTHENTICATED_USER",
            "GITHUB_UPDATE_THE_AUTHENTICATED_USER",
            "GITHUB_CREATE_A_REPOSITORY_FOR_THE_AUTHENTICATED_USER",
            "GITHUB_AUTHENTICATE_USER",
            "GITHUB_AUTHENTICATED_USER_TOKEN",
            "GITHUB_AUTH_THE_AUTHENTICATED_USER",
            "GITHUB_CREATE_AUTH_CONFIG",
            "GITHUB_RUN_THE_AUTHENTICATED_USER",
        ] {
            assert!(!action_allowed("github", denied), "{denied}");
        }
        // The pair is exact: another toolkit with the same slug shape stays denied.
        assert!(!action_allowed(
            "gitlab",
            "GITLAB_GET_THE_AUTHENTICATED_USER"
        ));
    }
}
