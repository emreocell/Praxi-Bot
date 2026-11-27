//! Where each app keeps its files.

use std::path::{Path, PathBuf};

use crate::mcp::Shape;
use crate::skills_src::MdKind;
use crate::{App, Ctx, Reason};

fn file(p: PathBuf) -> Option<PathBuf> {
    p.is_file().then_some(p)
}

impl Ctx<'_> {
    pub(crate) fn claude(&mut self) {
        let home = self.roots.home.clone();
        let mut desktop = vec![
            home.join("Library/Application Support/Claude/claude_desktop_config.json"),
            home.join(".config/Claude/claude_desktop_config.json"),
        ];
        if let Some(a) = &self.roots.appdata {
            desktop.insert(0, a.join("Claude/claude_desktop_config.json"));
        }
        for f in desktop.into_iter().filter_map(file) {
            self.json_servers(App::Claude, &f, Shape::McpServers);
        }
        if let Some(f) = file(home.join(".claude.json")) {
            self.json_servers(App::Claude, &f, Shape::McpServers);
        }
        self.skill_folders(App::Claude, &[home.join(".claude/skills")]);
        self.md_skills(App::Claude, &home.join(".claude/agents"), MdKind::Agent);
        self.instructions(App::Claude, &home.join(".claude/CLAUDE.md"), "CLAUDE.md");
        let commands = home.join(".claude/commands");
        if commands.is_dir() {
            let source = self.roots.show(&commands);
            self.other(
                App::Claude,
                "commands",
                &source,
                Reason::NotImported,
                "custom slash commands are not imported: copy the ones you want into a folder and import that folder",
            );
        }
    }

    pub(crate) fn cursor(&mut self) {
        if let Some(f) = file(self.roots.home.join(".cursor/mcp.json")) {
            self.json_servers(App::Cursor, &f, Shape::McpServers);
        }
    }

    pub(crate) fn vscode(&mut self) {
        let home = self.roots.home.clone();
        for edition in ["Code", "Code - Insiders"] {
            let mut candidates = vec![
                home.join(format!(
                    "Library/Application Support/{edition}/User/mcp.json"
                )),
                home.join(format!(".config/{edition}/User/mcp.json")),
            ];
            if let Some(a) = &self.roots.appdata {
                candidates.insert(0, a.join(format!("{edition}/User/mcp.json")));
            }
            for f in candidates.into_iter().filter_map(file) {
                self.json_servers(App::Vscode, &f, Shape::VsCode);
            }
        }
    }

    pub(crate) fn codex(&mut self) {
        let root = self.roots.codex();
        if !root.is_dir() {
            return;
        }
        if let Some(f) = file(root.join("config.toml")) {
            self.codex_servers(&f);
        }
        self.skill_folders(App::Codex, &[root.join("skills")]);
        self.md_skills(App::Codex, &root.join("prompts"), MdKind::Prompt);
        self.instructions(App::Codex, &root.join("AGENTS.md"), "AGENTS.md");
        if let Some(f) = file(root.join("auth.json")) {
            let source = self.roots.show(&f);
            self.other(
                App::Codex,
                "auth.json",
                &source,
                Reason::NeverCopied,
                "sign-ins and keys are never copied: sign in again in Praxi-Bot",
            );
        }
    }

    pub(crate) fn openclaw(&mut self) {
        let root = self
            .roots
            .openclaw_home
            .clone()
            .unwrap_or_else(|| self.roots.home.join(".openclaw"));
        if !root.is_dir() {
            return;
        }
        if let Some(f) = file(root.join("mcp-servers.json")) {
            // Same shape as Claude's: a map of name -> {command, args, env}.
            self.json_servers(App::Openclaw, &f, Shape::Bare);
        }
        let workspace = root.join("workspace");
        self.skill_folders(
            App::Openclaw,
            &[workspace.join("skills"), root.join("skills")],
        );
        if root.join("agents").is_dir() {
            let source = self.roots.show(&root.join("agents"));
            self.other(
                App::Openclaw,
                "chats",
                &source,
                Reason::NotImported,
                "past chats are not imported here: use `bot chats import` if you have an export",
            );
        }
        if let Some(f) = file(root.join("openclaw.json")) {
            let source = self.roots.show(&f);
            self.other(
                App::Openclaw,
                "channels and API keys",
                &source,
                Reason::NeverCopied,
                "sign-ins and keys are never copied: set them up again in Praxi-Bot",
            );
        }
    }

    pub(crate) fn hermes(&mut self) {
        let root = self
            .roots
            .hermes_home
            .clone()
            .unwrap_or_else(|| self.roots.home.join(".hermes"));
        if !root.is_dir() {
            return;
        }
        self.skill_folders(App::Hermes, &[root.join("skills")]);
        if let Some(f) = file(root.join(".env")) {
            let source = self.roots.show(&f);
            self.other(
                App::Hermes,
                ".env",
                &source,
                Reason::NeverCopied,
                "API keys are never copied: add them again in Praxi-Bot",
            );
        }
        if let Some(f) = file(root.join("config.yaml")) {
            let source = self.roots.show(&f);
            self.other(
                App::Hermes,
                "config.yaml",
                &source,
                Reason::NotImported,
                "settings and MCP servers in this file are not read: add servers with `bot mcp add`",
            );
        }
    }

    /// A folder of skills and prompt markdown the owner chose.
    pub(crate) fn folder(&mut self, folder: &Path) {
        let source = self.roots.show(folder);
        if !folder.is_dir() {
            self.other(
                App::Folder,
                &source,
                &source,
                Reason::Unreadable,
                "the folder cannot be read",
            );
            return;
        }
        self.skill_folders(App::Folder, &[folder.to_path_buf()]);
        self.md_skills(App::Folder, folder, MdKind::Loose);
        for name in ["AGENTS.md", "CLAUDE.md"] {
            let file = folder.join(name);
            // A link in a folder of someone else's could point at any file of this machine.
            if std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
                self.instructions(App::Folder, &file, name);
            }
        }
    }
}
