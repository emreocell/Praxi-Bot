//! Who may receive keyboard input: process identity, the window the input would
//! reach, and the pure rules that decide whether a run-scoped control grant may
//! cover that window.
//!
//! Nothing here talks to the OS. A backend fills an [`InputTarget`] from its own
//! window and process APIs; these functions only judge it. Every check fails
//! closed: a missing fact is a refusal, never a pass.
//!
//! **This is a heuristic, not a program-class boundary.** The deny lists match
//! the executable's file name, and the dialog test looks at the window class and
//! at whether the window is owned. A renamed or copied binary of a listed
//! program is not recognised, and a program that is not listed but has its own
//! command line, macro host or launcher can still receive a grant. The grant is
//! therefore described to the user as "Praxi may type into this program", with
//! its full path, never as a promise that dangerous programs are excluded.

use serde::{Deserialize, Serialize};

/// One running program: image path, process id and process start time.
///
/// The start time is part of the identity so a recycled pid is a different
/// program. `pid == 0`, an empty path or `start_time == 0` is "unknown" and
/// never equal to anything.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    /// Full path of the executable image.
    pub image_path: String,
    /// Process id.
    pub pid: u32,
    /// Process creation time in OS ticks (Windows: FILETIME, 100 ns).
    pub start_time: u64,
}

impl ProcessIdentity {
    /// All three facts are known.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.pid != 0 && self.start_time != 0 && !self.image_path.trim().is_empty()
    }

    /// Same running program as `other`. Incomplete identities never match.
    /// The image path is compared without regard to case (Windows paths).
    #[must_use]
    pub fn same_program(&self, other: &Self) -> bool {
        self.is_complete()
            && other.is_complete()
            && self.pid == other.pid
            && self.start_time == other.start_time
            && self.image_path.eq_ignore_ascii_case(&other.image_path)
    }

    /// File name of the image without directory and extension, lower case.
    #[must_use]
    pub fn program_stem(&self) -> String {
        program_stem(&self.image_path)
    }
}

/// File name stem of an image path (either separator), lower case.
#[must_use]
pub fn program_stem(image_path: &str) -> String {
    let name = image_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image_path)
        .trim();
    let stem = name
        .rsplit_once('.')
        .map_or(name, |(stem, ext)| if ext.len() <= 4 { stem } else { name });
    stem.to_lowercase()
}

/// The window and program keyboard input would reach right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTarget {
    /// Platform window id.
    pub window_id: u64,
    /// Window title.
    pub title: String,
    /// Window class (Windows) or equivalent; empty when unknown.
    pub class: String,
    /// The program that owns the window.
    pub identity: ProcessIdentity,
    /// Whether the program runs elevated. `None` = could not be read, which
    /// counts as elevated.
    pub elevated: Option<bool>,
    /// The focused element is a password field.
    pub password_focus: bool,
    /// The top-level window has an owner window (an owned popup: custom-class
    /// dialogs and modal boxes included).
    #[serde(default)]
    pub owned: bool,
}

/// Condition the OS backend must verify immediately before it sends input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActGuard {
    /// The only program the input may reach.
    pub expect: ProcessIdentity,
}

/// Why a grant does not cover a window, or why the guard refused to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The backend could not name the program (or the pid is reused/unknown).
    NoIdentity,
    /// Elevated, or the elevation could not be read.
    Elevated,
    /// Terminal, console or shell.
    Terminal,
    /// An editor or tool with a built-in terminal or command line.
    CommandApp,
    /// A browser driven as a desktop app.
    Browser,
    /// A password manager.
    PasswordManager,
    /// A system shell, launcher or credential surface.
    SystemSurface,
    /// A packaged-app frame host that cannot be bound to one program.
    UwpFrame,
    /// A dialog (file dialogs, the Run dialog, message boxes).
    Dialog,
    /// The focused element is a password field.
    PasswordField,
    /// Keys that open a launcher or switch windows (Win/Super, Ctrl+Esc, ...).
    Chord,
    /// The window that would receive input is not the granted program.
    ForegroundChanged,
    /// A modifier key is held down, so the next key would not be what was approved.
    ModifierHeld,
    /// A remote-desktop or virtual-machine viewer: keystrokes operate another computer.
    RemoteControl,
    /// `desktop_drag` is never covered by a task-wide permission.
    Drag,
    /// A key could not be confirmed released after a send.
    ReleaseUnconfirmed,
    /// A window of Praxi itself (the app, its approval cards, the daemon).
    PraxiItself,
}

impl Refusal {
    /// Stable code for cards and audit.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoIdentity => "no_identity",
            Self::Elevated => "elevated",
            Self::Terminal => "terminal",
            Self::CommandApp => "command_app",
            Self::Browser => "browser",
            Self::PasswordManager => "password_manager",
            Self::SystemSurface => "system_surface",
            Self::UwpFrame => "uwp_frame",
            Self::Dialog => "dialog",
            Self::PasswordField => "password_field",
            Self::Chord => "chord",
            Self::ForegroundChanged => "foreground_changed",
            Self::ModifierHeld => "modifier_held",
            Self::RemoteControl => "remote_control",
            Self::Drag => "drag",
            Self::ReleaseUnconfirmed => "release_unconfirmed",
            Self::PraxiItself => "praxi_itself",
        }
    }

    /// One plain sentence for an error message.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::NoIdentity => "the program or window could not be identified",
            Self::Elevated => "the program runs elevated or its elevation could not be read",
            Self::Terminal => "the window is a terminal or shell",
            Self::CommandApp => {
                "the program has a built-in command line, macro host or command palette"
            }
            Self::Browser => "the window is a browser",
            Self::PasswordManager => "the program is a password manager",
            Self::SystemSurface => "the window is a system launcher or sign-in surface",
            Self::UwpFrame => {
                "the window is a packaged-app frame that cannot be bound to one program"
            }
            Self::Dialog => "the window is a dialog or an owned popup",
            Self::PasswordField => "the focused element is a password field",
            Self::Chord => {
                "this key combination is not on the short list of keys a task-wide permission may send"
            }
            Self::ForegroundChanged => {
                "the element that would receive the input is not the program that was approved"
            }
            Self::ModifierHeld => "a modifier key is held down",
            Self::RemoteControl => "the program is a remote-desktop or virtual-machine viewer",
            Self::Drag => "dragging is never covered by a task-wide permission",
            Self::ReleaseUnconfirmed => "a key could not be confirmed released",
            Self::PraxiItself => "the window belongs to Praxi itself",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// Hotkey launchers and command surfaces. `powertoys*` is matched by prefix.
const LAUNCHERS: &[&str] = &[
    "everything",
    "everything64",
    "flow.launcher",
    "wox",
    "keypirinha",
    "ueli",
    "launchy",
    "listary",
    "raycast",
    "alfred",
    "albert",
    "autohotkey",
    "autohotkey64",
    "autohotkeyu64",
];

/// Remote-desktop clients and virtual-machine viewers: local process, remote effect.
const REMOTE: &[&str] = &[
    "mstsc",
    "msrdc",
    "rdcman",
    "vmconnect",
    "vmware",
    "vmplayer",
    "vmware-view",
    "virtualbox",
    "virtualboxvm",
    "vboxmanage",
    "anydesk",
    "teamviewer",
    "rustdesk",
    "parsecd",
    "vncviewer",
    "tvnviewer",
    "wfica32",
    "cdviewer",
    "mremoteng",
    "qemu-system-x86_64",
    "utm",
    "parallels",
];

const TERMINALS: &[&str] = &[
    "cmd",
    "powershell",
    "powershell_ise",
    "pwsh",
    "windowsterminal",
    "wt",
    "conhost",
    "openconsole",
    "wsl",
    "wslhost",
    "bash",
    "sh",
    "zsh",
    "fish",
    "mintty",
    "alacritty",
    "wezterm",
    "wezterm-gui",
    "kitty",
    "hyper",
    "tabby",
    "putty",
    "kitty",
    "gnome-terminal",
    "konsole",
    "xterm",
    "terminal",
    "iterm2",
    "ConEmu64",
    "conemu",
];

const COMMAND_APPS: &[&str] = &[
    "code",
    "code - insiders",
    "codium",
    "vscodium",
    "cursor",
    "windsurf",
    "devenv",
    "idea",
    "idea64",
    "pycharm",
    "pycharm64",
    "webstorm",
    "webstorm64",
    "clion",
    "clion64",
    "rider",
    "rider64",
    "goland",
    "goland64",
    "rubymine",
    "rubymine64",
    "phpstorm",
    "phpstorm64",
    "datagrip",
    "datagrip64",
    "studio",
    "studio64",
    "notepad++",
    "sublime_text",
    "atom",
    "emacs",
    "gvim",
    "nvim-qt",
    "winword",
    "excel",
    "powerpnt",
    "outlook",
    "msaccess",
    "visio",
    "mspub",
    "onenote",
    "soffice",
    "cscript",
    "wscript",
    "mshta",
    "windbg",
    "explorer",
    "taskmgr",
    "mmc",
    "regedit",
    "taskschd",
    "msconfig",
    "control",
];

const BROWSERS: &[&str] = &[
    "chrome",
    "msedge",
    "firefox",
    "brave",
    "opera",
    "vivaldi",
    "iexplore",
    "chromium",
    "arc",
    "safari",
    "tor",
    "waterfox",
    "librewolf",
];

const PASSWORD_MANAGERS: &[&str] = &[
    "1password",
    "keepass",
    "keepassxc",
    "bitwarden",
    "lastpass",
    "dashlane",
    "enpass",
    "nordpass",
    "roboform",
    "keeper",
];

const SYSTEM_SURFACES: &[&str] = &[
    "searchhost",
    "searchapp",
    "searchui",
    "startmenuexperiencehost",
    "shellexperiencehost",
    "shellhost",
    "lockapp",
    "logonui",
    "consent",
    "credentialuibroker",
    "securityhealthsystray",
    "winlogon",
    "dwm",
    "runtimebroker",
];

/// Refusal for a program by its image name, if it is on a deny list.
#[must_use]
pub fn program_refusal(image_path: &str) -> Option<Refusal> {
    let stem = program_stem(image_path);
    let listed = |list: &[&str]| list.iter().any(|n| n.eq_ignore_ascii_case(&stem));
    if listed(TERMINALS) {
        Some(Refusal::Terminal)
    } else if listed(REMOTE) {
        Some(Refusal::RemoteControl)
    } else if listed(LAUNCHERS) || stem.starts_with("powertoys") {
        Some(Refusal::SystemSurface)
    } else if stem == "applicationframehost" {
        Some(Refusal::UwpFrame)
    } else if listed(SYSTEM_SURFACES) {
        Some(Refusal::SystemSurface)
    } else if listed(COMMAND_APPS) {
        Some(Refusal::CommandApp)
    } else if listed(BROWSERS) {
        Some(Refusal::Browser)
    } else if listed(PASSWORD_MANAGERS) {
        Some(Refusal::PasswordManager)
    } else {
        None
    }
}

/// Refusal for a window class: console windows are terminals, and the
/// standard dialog class (`#32770`, used by file dialogs, the Run dialog and
/// message boxes) is a dialog in any program.
#[must_use]
pub fn class_refusal(class: &str) -> Option<Refusal> {
    let c = class.trim().to_ascii_lowercase();
    match c.as_str() {
        "consolewindowclass"
        | "cascadia_hosting_window_class"
        | "pseudoconsolewindow"
        | "virtualconsoleclass" => Some(Refusal::Terminal),
        "#32770" => Some(Refusal::Dialog),
        _ => None,
    }
}

/// Whether the window itself may be covered by a control grant at all,
/// independent of which program was granted. `None` = it may.
#[must_use]
pub fn judge(target: &InputTarget) -> Option<Refusal> {
    if !target.identity.is_complete() {
        return Some(Refusal::NoIdentity);
    }
    // A failed class lookup is not a class: refuse, never accept an empty one.
    if target.class.trim().is_empty() {
        return Some(Refusal::NoIdentity);
    }
    if target.elevated != Some(false) {
        return Some(Refusal::Elevated);
    }
    if target.owned {
        return Some(Refusal::Dialog);
    }
    if let Some(r) = program_refusal(&target.identity.image_path) {
        return Some(r);
    }
    if let Some(r) = class_refusal(&target.class) {
        return Some(r);
    }
    if target.password_focus {
        return Some(Refusal::PasswordField);
    }
    None
}

/// Window classes of credential and consent surfaces (the Windows credential
/// prompt, the UAC consent window, the secure-desktop host). Lower case.
const CREDENTIAL_CLASSES: &[&str] = &[
    "credential dialog xaml host",
    "credentialuibroker",
    "$$$secure ui desktop window",
    "consentuiwindow",
];

/// Whether the window class is a credential or UAC consent surface.
#[must_use]
pub fn credential_class(class: &str) -> bool {
    let c = class.trim().to_ascii_lowercase();
    CREDENTIAL_CLASSES.contains(&c.as_str())
}

/// Program stems that are Praxi's own: the daemon (`bot`) and the desktop app
/// (`bot-desktop`). Anything with `praxi` in its file name counts too.
const PRAXI_STEMS: &[&str] = &["bot", "bot-desktop"];

/// Whether this program or window is Praxi itself: this very process, a program
/// named like it, or a window whose title names Praxi. The title is a heuristic
/// that fails toward refusing more.
#[must_use]
pub fn is_praxi_itself(identity: &ProcessIdentity, title: &str) -> bool {
    let stem = identity.program_stem();
    (identity.pid != 0 && identity.pid == std::process::id())
        || stem.contains("praxi")
        || PRAXI_STEMS.contains(&stem.as_str())
        || title.to_lowercase().contains("praxi")
}

/// Why a window operation (minimise, move, close, ...) on this window is refused
/// whatever the owner approved. No input is sent, so the input rules of [`judge`]
/// that are about where keys may go (terminals, browsers, command apps, remote
/// viewers, password fields) do not apply: those windows can be minimised or
/// moved. What stays off limits:
///
/// * a window whose program or elevation cannot be established (the owner's own
///   account only: an elevated or unreadable program is refused);
/// * Praxi itself;
/// * a system shell, a credential or UAC surface, a packaged-app frame host and
///   a password manager;
/// * closing a dialog or an owned popup (`#32770`, a modal box): it asks the
///   owner something, so the answer is theirs.
///
/// Whether the window is a payment surface is decided by the caller, which knows
/// the owner's money rules.
#[must_use]
pub fn window_op_refusal(target: &InputTarget, op: crate::window::WindowOp) -> Option<Refusal> {
    if !target.identity.is_complete() {
        return Some(Refusal::NoIdentity);
    }
    if target.elevated != Some(false) {
        return Some(Refusal::Elevated);
    }
    if is_praxi_itself(&target.identity, &target.title) {
        return Some(Refusal::PraxiItself);
    }
    if credential_class(&target.class) {
        return Some(Refusal::SystemSurface);
    }
    if let r @ Some(Refusal::SystemSurface | Refusal::PasswordManager) =
        program_refusal(&target.identity.image_path)
    {
        return r;
    }
    if op == crate::window::WindowOp::Close {
        // A failed class lookup is not an ordinary class: closing is refused.
        if target.class.trim().is_empty() {
            return Some(Refusal::NoIdentity);
        }
        if target.owned || class_refusal(&target.class) == Some(Refusal::Dialog) {
            return Some(Refusal::Dialog);
        }
    }
    None
}

/// The recheck a backend runs right before it asks the window system for a window
/// operation: `now` is the window's facts read at this moment. With a `guard`, the
/// window must still belong to the program the approval was shown for; whatever the
/// guard, the window must still pass [`window_op_refusal`]. `now == None` (the
/// facts could not be read) is a refusal when there is a guard and otherwise leaves
/// the decision to the caller, which has already judged the window.
///
/// # Errors
/// The [`Refusal`] to report; the operation must not be sent.
pub fn check_window_op(
    guard: Option<&ActGuard>,
    now: Option<&InputTarget>,
    op: crate::window::WindowOp,
) -> Result<(), Refusal> {
    if let Some(guard) = guard {
        let Some(now) = now else {
            return Err(Refusal::NoIdentity);
        };
        if !guard.expect.same_program(&now.identity) {
            return Err(if now.identity.is_complete() {
                Refusal::ForegroundChanged
            } else {
                Refusal::NoIdentity
            });
        }
    }
    now.and_then(|t| window_op_refusal(t, op))
        .map_or(Ok(()), Err)
}

/// The recheck a backend runs right before it sends input: the window that
/// would receive it must be the granted program and must still pass [`judge`].
///
/// # Errors
/// The [`Refusal`] to report; the input must not be sent.
pub fn check_guard(guard: &ActGuard, now: &InputTarget) -> Result<(), Refusal> {
    if !guard.expect.same_program(&now.identity) {
        return Err(if now.identity.is_complete() {
            Refusal::ForegroundChanged
        } else {
            Refusal::NoIdentity
        });
    }
    judge(now).map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(path: &str, pid: u32, start: u64) -> ProcessIdentity {
        ProcessIdentity {
            image_path: path.into(),
            pid,
            start_time: start,
        }
    }

    fn target(path: &str, class: &str) -> InputTarget {
        InputTarget {
            window_id: 1,
            title: "t".into(),
            class: class.into(),
            identity: ident(path, 10, 99),
            elevated: Some(false),
            password_focus: false,
            owned: false,
        }
    }

    #[test]
    fn window_operations_skip_input_rules_but_keep_system_and_credential_programs_off_limits() {
        use crate::window::WindowOp;
        // A terminal, a browser and an editor may be minimised, moved or closed: no input is sent.
        for (path, class) in [
            ("C:\\Windows\\System32\\cmd.exe", "ConsoleWindowClass"),
            (
                "C:\\Program Files\\Mozilla Firefox\\firefox.exe",
                "MozillaWindowClass",
            ),
            ("C:\\Windows\\notepad.exe", "Notepad"),
        ] {
            for op in [
                WindowOp::Minimize,
                WindowOp::Move { x: 1, y: 1 },
                WindowOp::Close,
            ] {
                assert_eq!(
                    window_op_refusal(&target(path, class), op),
                    None,
                    "{path} {op:?}"
                );
            }
        }
        let mut elevated = target("C:\\Windows\\notepad.exe", "Notepad");
        elevated.elevated = None;
        assert_eq!(
            window_op_refusal(&elevated, WindowOp::Minimize),
            Some(Refusal::Elevated)
        );
        elevated.elevated = Some(true);
        assert_eq!(
            window_op_refusal(&elevated, WindowOp::Minimize),
            Some(Refusal::Elevated)
        );
        let mut unnamed = target("C:\\Windows\\notepad.exe", "Notepad");
        unnamed.identity.start_time = 0;
        assert_eq!(
            window_op_refusal(&unnamed, WindowOp::Close),
            Some(Refusal::NoIdentity)
        );
        assert_eq!(
            window_op_refusal(
                &target("C:\\Program Files\\KeePass\\KeePass.exe", "WindowsForms10"),
                WindowOp::Move { x: 0, y: 0 }
            ),
            Some(Refusal::PasswordManager)
        );
        assert_eq!(
            window_op_refusal(
                &target("C:\\Windows\\SystemApps\\SearchHost.exe", "Class"),
                WindowOp::Close
            ),
            Some(Refusal::SystemSurface)
        );
    }

    #[test]
    fn a_dialog_is_never_closed_but_may_be_moved() {
        use crate::window::WindowOp;
        let save = target("C:\\Windows\\notepad.exe", "#32770");
        assert_eq!(
            window_op_refusal(&save, WindowOp::Close),
            Some(Refusal::Dialog)
        );
        assert_eq!(window_op_refusal(&save, WindowOp::Minimize), None);
        let mut popup = target("C:\\x\\app.exe", "MyCustomDialog");
        popup.owned = true;
        assert_eq!(
            window_op_refusal(&popup, WindowOp::Close),
            Some(Refusal::Dialog)
        );
        assert_eq!(
            window_op_refusal(&popup, WindowOp::Move { x: 5, y: 5 }),
            None
        );
        // A failed class lookup is not an ordinary class: closing is refused.
        let blank = target("C:\\x\\app.exe", "");
        assert_eq!(
            window_op_refusal(&blank, WindowOp::Close),
            Some(Refusal::NoIdentity)
        );
    }

    #[test]
    fn credential_and_uac_surfaces_take_no_window_operation_at_all() {
        use crate::window::WindowOp;
        for class in [
            "Credential Dialog Xaml Host",
            "CredentialUIBroker",
            "$$$Secure UI Desktop Window",
            "ConsentUIWindow",
        ] {
            for op in [
                WindowOp::Close,
                WindowOp::Minimize,
                WindowOp::Move { x: 0, y: 0 },
            ] {
                assert_eq!(
                    window_op_refusal(&target("C:\\x\\app.exe", class), op),
                    Some(Refusal::SystemSurface),
                    "{class} {op:?}"
                );
            }
        }
        // The UAC consent program is on the system-surface list by name.
        assert_eq!(
            window_op_refusal(
                &target("C:\\Windows\\System32\\consent.exe", "Class"),
                WindowOp::Close
            ),
            Some(Refusal::SystemSurface)
        );
    }

    #[test]
    fn praxi_itself_is_never_managed() {
        use crate::window::WindowOp;
        let own = target("C:\\x\\anything.exe", "Class");
        let mut own_pid = own.clone();
        own_pid.identity.pid = std::process::id();
        for t in [
            own_pid,
            target("C:\\Program Files\\Praxi-Bot\\Praxi-Bot.exe", "Class"),
            target("C:\\Program Files\\Praxi-Bot\\bot-desktop.exe", "Class"),
            target("/usr/local/bin/bot", "Class"),
            {
                let mut t = own.clone();
                t.title = "Praxi-Bot - approvals".into();
                t
            },
        ] {
            for op in [WindowOp::Minimize, WindowOp::Close] {
                assert_eq!(
                    window_op_refusal(&t, op),
                    Some(Refusal::PraxiItself),
                    "{t:?} {op:?}"
                );
            }
        }
        assert_eq!(window_op_refusal(&own, WindowOp::Minimize), None);
    }

    #[test]
    fn the_window_is_rechecked_right_before_the_operation() {
        use crate::window::WindowOp;
        let t = target("C:\\Windows\\notepad.exe", "Notepad");
        let g = ActGuard {
            expect: t.identity.clone(),
        };
        assert_eq!(
            check_window_op(Some(&g), Some(&t), WindowOp::Minimize),
            Ok(())
        );
        // The window now belongs to another program (same pid, new start time).
        let mut other = t.clone();
        other.identity.start_time += 1;
        assert_eq!(
            check_window_op(Some(&g), Some(&other), WindowOp::Minimize),
            Err(Refusal::ForegroundChanged)
        );
        // Facts that cannot be read are a refusal under a guard.
        assert_eq!(
            check_window_op(Some(&g), None, WindowOp::Close),
            Err(Refusal::NoIdentity)
        );
        // The same program, but it turned into a dialog since the approval.
        let mut dialog = t.clone();
        dialog.class = "#32770".into();
        assert_eq!(
            check_window_op(Some(&g), Some(&dialog), WindowOp::Close),
            Err(Refusal::Dialog)
        );
        assert_eq!(
            check_window_op(Some(&g), Some(&dialog), WindowOp::Minimize),
            Ok(())
        );
        // No guard: the window is still judged.
        let mut keepass = t;
        keepass.identity.image_path = "C:\\KeePass\\KeePass.exe".into();
        assert_eq!(
            check_window_op(None, Some(&keepass), WindowOp::Minimize),
            Err(Refusal::PasswordManager)
        );
    }

    #[test]
    fn an_empty_class_or_an_owned_window_is_refused() {
        let mut t = target("C:\\Windows\\notepad.exe", "");
        assert_eq!(judge(&t), Some(Refusal::NoIdentity), "class lookup failed");
        t.class = "MyCustomDialog".into();
        t.owned = true;
        assert_eq!(judge(&t), Some(Refusal::Dialog), "owned custom-class popup");
    }

    #[test]
    fn launchers_remote_viewers_and_office_hosts_are_refused_by_name() {
        for (path, want) in [
            ("C:\\x\\mstsc.exe", Refusal::RemoteControl),
            ("C:\\x\\vmconnect.exe", Refusal::RemoteControl),
            ("C:\\x\\AnyDesk.exe", Refusal::RemoteControl),
            ("C:\\x\\Everything.exe", Refusal::SystemSurface),
            ("C:\\x\\PowerToys.PowerLauncher.exe", Refusal::SystemSurface),
            ("C:\\x\\WINWORD.EXE", Refusal::CommandApp),
            ("C:\\x\\EXCEL.EXE", Refusal::CommandApp),
            ("C:\\x\\notepad++.exe", Refusal::CommandApp),
            ("C:\\x\\wscript.exe", Refusal::CommandApp),
        ] {
            assert_eq!(program_refusal(path), Some(want), "{path}");
        }
    }

    #[test]
    fn a_renamed_copy_of_a_listed_program_is_not_recognised() {
        // Documented limit of the heuristic: only the file name is looked at.
        assert_eq!(program_refusal("C:\\tmp\\totally-innocent.exe"), None);
    }

    #[test]
    fn a_normal_program_passes() {
        let t = target("C:\\Windows\\System32\\notepad.exe", "Notepad");
        assert_eq!(judge(&t), None);
        let g = ActGuard {
            expect: t.identity.clone(),
        };
        assert_eq!(check_guard(&g, &t), Ok(()));
    }

    #[test]
    fn same_pid_with_another_start_time_is_another_program() {
        let t = target("C:\\Windows\\notepad.exe", "Notepad");
        let g = ActGuard {
            expect: ident("C:\\Windows\\notepad.exe", 10, 98),
        };
        assert_eq!(check_guard(&g, &t), Err(Refusal::ForegroundChanged));
    }

    #[test]
    fn another_pid_with_the_same_path_and_start_time_is_another_program() {
        let t = target("C:\\Windows\\notepad.exe", "Notepad");
        let g = ActGuard {
            expect: ident("C:\\Windows\\notepad.exe", 11, 99),
        };
        assert_eq!(check_guard(&g, &t), Err(Refusal::ForegroundChanged));
    }

    #[test]
    fn an_unknown_identity_never_matches() {
        let mut t = target("C:\\Windows\\notepad.exe", "Notepad");
        t.identity.start_time = 0;
        let g = ActGuard {
            expect: t.identity.clone(),
        };
        assert_eq!(check_guard(&g, &t), Err(Refusal::NoIdentity));
        assert_eq!(judge(&t), Some(Refusal::NoIdentity));
    }

    #[test]
    fn elevated_or_unreadable_elevation_is_refused() {
        let mut t = target("C:\\Windows\\notepad.exe", "Notepad");
        t.elevated = Some(true);
        assert_eq!(judge(&t), Some(Refusal::Elevated));
        t.elevated = None;
        assert_eq!(judge(&t), Some(Refusal::Elevated));
    }

    #[test]
    fn listed_programs_are_refused_by_kind() {
        for (path, want) in [
            ("C:\\Windows\\System32\\cmd.exe", Refusal::Terminal),
            (
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                Refusal::Terminal,
            ),
            ("C:\\Windows\\explorer.exe", Refusal::CommandApp),
            ("C:\\x\\Code.exe", Refusal::CommandApp),
            ("C:\\x\\chrome.exe", Refusal::Browser),
            ("C:\\x\\KeePassXC.exe", Refusal::PasswordManager),
            (
                "C:\\Windows\\System32\\ApplicationFrameHost.exe",
                Refusal::UwpFrame,
            ),
            (
                "C:\\Windows\\SystemApps\\SearchHost.exe",
                Refusal::SystemSurface,
            ),
        ] {
            assert_eq!(program_refusal(path), Some(want), "{path}");
        }
        assert_eq!(program_refusal("C:\\Windows\\notepad.exe"), None);
        assert_eq!(program_refusal("/usr/bin/gedit"), None);
    }

    #[test]
    fn console_and_dialog_classes_are_refused_in_any_program() {
        let t = target("C:\\Windows\\notepad.exe", "ConsoleWindowClass");
        assert_eq!(judge(&t), Some(Refusal::Terminal));
        let t = target("C:\\Windows\\notepad.exe", "#32770");
        assert_eq!(judge(&t), Some(Refusal::Dialog));
    }

    #[test]
    fn a_password_field_is_refused() {
        let mut t = target("C:\\Windows\\notepad.exe", "Notepad");
        t.password_focus = true;
        assert_eq!(judge(&t), Some(Refusal::PasswordField));
    }

    #[test]
    fn the_stem_ignores_directories_and_extension() {
        assert_eq!(program_stem("C:\\A B\\Foo.EXE"), "foo");
        assert_eq!(program_stem("/usr/bin/bash"), "bash");
        assert_eq!(program_stem("Code - Insiders.exe"), "code - insiders");
    }
}
