//! Hermetic tests: temp git repos, no network.

use std::path::Path;
use std::process::Command;

use dev_tools::{DevToolsConfig, GIT_TOOL_NAMES, PROCESS_LIST_TOOL_NAME, register, tools};
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, ToolContext, ToolError, ToolRegistry,
};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Temp dir (the allowed workspace) containing one repo `repo/` with a commit.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    repo: std::path::PathBuf,
    reg: ToolRegistry,
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join("b.txt"), "bee\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    let reg = ToolRegistry::new();
    let n = register(&reg, &DevToolsConfig::new(vec![root.clone()]));
    assert_eq!(n, 7);
    Fixture {
        _tmp: tmp,
        root,
        repo,
        reg,
    }
}

async fn call(f: &Fixture, name: &str, input: Value) -> Result<Value, ToolError> {
    let tool = f.reg.get(name).unwrap_or_else(|| panic!("tool {name}"));
    let out = tool.call(&ToolContext::default(), input).await?;
    Ok(match out.content.into_iter().next().unwrap() {
        ContentPart::Text { text } => Value::String(text),
        ContentPart::Json { data } => data,
        ContentPart::Binary { .. } => panic!("binary"),
    })
}

fn text(v: &Value) -> &str {
    v.as_str().expect("text output")
}

fn repo_arg(f: &Fixture) -> String {
    f.repo.display().to_string()
}

#[test]
fn specs_are_valid_and_classified() {
    let all = tools(&DevToolsConfig::new(vec![]));
    assert_eq!(all.len(), 7);
    let mut names: Vec<_> = all.iter().map(|t| t.spec().name.clone()).collect();
    for n in GIT_TOOL_NAMES.iter().chain([&PROCESS_LIST_TOOL_NAME]) {
        assert!(names.iter().any(|x| x == n), "missing {n}");
    }
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 7, "names unique");
    for t in &all {
        let s = t.spec();
        assert!(s.is_valid(), "{}", s.name);
        let expected = match s.name.as_str() {
            "git_commit" | "git_branch" => Effect::Write,
            _ => Effect::Read,
        };
        assert_eq!(s.effect, expected, "{}", s.name);
        let cat = if s.name == "process_list" {
            PermissionCategory::System
        } else {
            PermissionCategory::Fs
        };
        assert_eq!(s.category, cat, "{}", s.name);
    }
}

#[tokio::test]
async fn status_reports_modified_and_untracked() {
    let f = fixture();
    let r = repo_arg(&f);
    let clean = call(&f, "git_status", json!({"repo": r})).await.unwrap();
    assert!(text(&clean).starts_with("## main"), "{clean}");
    assert_eq!(text(&clean).lines().count(), 1);

    std::fs::write(f.repo.join("a.txt"), "changed\n").unwrap();
    std::fs::write(f.repo.join("new.txt"), "x\n").unwrap();
    let dirty = call(&f, "git_status", json!({"repo": r})).await.unwrap();
    assert!(text(&dirty).contains(" M a.txt"), "{dirty}");
    assert!(text(&dirty).contains("?? new.txt"));
    let no_untracked = call(
        &f,
        "git_status",
        json!({"repo": r, "include_untracked": false}),
    )
    .await
    .unwrap();
    assert!(!text(&no_untracked).contains("new.txt"));
}

#[tokio::test]
async fn default_repo_is_first_root_and_relative_repo_works() {
    let f = fixture();
    // Root itself is not a repo.
    let err = call(&f, "git_status", json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)), "{err}");
    let ok = call(&f, "git_status", json!({"repo": "repo"}))
        .await
        .unwrap();
    assert!(text(&ok).starts_with("## main"));
}

#[tokio::test]
async fn diff_unstaged_staged_path_filter_and_cap() {
    let f = fixture();
    let r = repo_arg(&f);
    std::fs::write(f.repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(f.repo.join("b.txt"), "bee\nbuzz\n").unwrap();

    let all = call(&f, "git_diff", json!({"repo": r})).await.unwrap();
    assert!(text(&all).contains("+two") && text(&all).contains("+buzz"));
    let only_a = call(&f, "git_diff", json!({"repo": r, "paths": ["a.txt"]}))
        .await
        .unwrap();
    assert!(text(&only_a).contains("+two") && !text(&only_a).contains("+buzz"));
    let stat = call(&f, "git_diff", json!({"repo": r, "stat": true}))
        .await
        .unwrap();
    assert!(text(&stat).contains("2 files changed"), "{stat}");

    git(&f.repo, &["add", "a.txt"]);
    let staged = call(&f, "git_diff", json!({"repo": r, "staged": true}))
        .await
        .unwrap();
    assert!(text(&staged).contains("+two") && !text(&staged).contains("+buzz"));
    let vs_head = call(
        &f,
        "git_diff",
        json!({"repo": r, "rev": "HEAD", "paths": ["b.txt"]}),
    )
    .await
    .unwrap();
    assert!(text(&vs_head).contains("+buzz"));

    let big = "x".repeat(50_000);
    std::fs::write(f.repo.join("big.txt"), &big).unwrap();
    git(&f.repo, &["add", "-N", "big.txt"]);
    let capped = call(
        &f,
        "git_diff",
        json!({"repo": r, "paths": ["big.txt"], "max_bytes": 1000}),
    )
    .await
    .unwrap();
    assert!(
        text(&capped).contains("truncated"),
        "{}",
        text(&capped).len()
    );
    assert!(text(&capped).len() < 1300);

    git(&f.repo, &["reset", "-q"]);
    git(&f.repo, &["checkout", "-q", "--", "."]);
    let none = call(&f, "git_diff", json!({"repo": r, "paths": ["a.txt"]}))
        .await
        .unwrap();
    assert_eq!(text(&none), "(no differences)");
}

#[tokio::test]
async fn log_and_show() {
    let f = fixture();
    let r = repo_arg(&f);
    std::fs::write(f.repo.join("a.txt"), "one\ntwo\n").unwrap();
    git(&f.repo, &["commit", "-q", "-am", "second commit"]);

    let log = call(&f, "git_log", json!({"repo": r})).await.unwrap();
    let commits = log["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0]["subject"], "second commit");
    assert_eq!(commits[0]["author"], "Test");
    assert_eq!(commits[0]["hash"].as_str().unwrap().len(), 40);

    let one = call(&f, "git_log", json!({"repo": r, "max_count": 1}))
        .await
        .unwrap();
    assert_eq!(one["commits"].as_array().unwrap().len(), 1);
    let by_path = call(&f, "git_log", json!({"repo": r, "paths": ["b.txt"]}))
        .await
        .unwrap();
    assert_eq!(by_path["commits"].as_array().unwrap().len(), 1);

    let show = call(&f, "git_show", json!({"repo": r})).await.unwrap();
    assert!(text(&show).contains("second commit") && text(&show).contains("+two"));
    let show_stat = call(
        &f,
        "git_show",
        json!({"repo": r, "rev": "HEAD~1", "stat": true}),
    )
    .await
    .unwrap();
    assert!(text(&show_stat).contains("initial") && text(&show_stat).contains("a.txt"));
}

#[tokio::test]
async fn log_on_empty_repo_is_empty() {
    let f = fixture();
    init_repo(&f.root.join("empty"));
    let log = call(&f, "git_log", json!({"repo": "empty"})).await.unwrap();
    assert_eq!(log["commits"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn branch_list_create_switch() {
    let f = fixture();
    let r = repo_arg(&f);
    let list = call(&f, "git_branch", json!({"repo": r, "action": "list"}))
        .await
        .unwrap();
    assert_eq!(list["current"], "main");
    assert_eq!(list["branches"].as_array().unwrap().len(), 1);

    let created = call(
        &f,
        "git_branch",
        json!({"repo": r, "action": "create", "name": "feat/x"}),
    )
    .await
    .unwrap();
    assert_eq!(created["switched"], false);
    let list = call(&f, "git_branch", json!({"repo": r, "action": "list"}))
        .await
        .unwrap();
    assert_eq!(list["branches"].as_array().unwrap().len(), 2);
    assert_eq!(list["current"], "main");

    call(
        &f,
        "git_branch",
        json!({"repo": r, "action": "switch", "name": "feat/x"}),
    )
    .await
    .unwrap();
    let list = call(&f, "git_branch", json!({"repo": r, "action": "list"}))
        .await
        .unwrap();
    assert_eq!(list["current"], "feat/x");

    let both = call(
        &f,
        "git_branch",
        json!({"repo": r, "action": "create", "name": "topic", "start_point": "main", "switch": true}),
    )
    .await
    .unwrap();
    assert_eq!(both["switched"], true);
    let list = call(&f, "git_branch", json!({"repo": r, "action": "list"}))
        .await
        .unwrap();
    assert_eq!(list["current"], "topic");

    // Switching to a missing branch is an execution error, creating a duplicate too.
    assert!(
        call(
            &f,
            "git_branch",
            json!({"repo": r, "action": "switch", "name": "nope"})
        )
        .await
        .is_err()
    );
    assert!(
        call(
            &f,
            "git_branch",
            json!({"repo": r, "action": "create", "name": "topic"})
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn branch_rejects_bad_names() {
    let f = fixture();
    let r = repo_arg(&f);
    for bad in [
        "-d",
        "--orphan",
        "a b",
        "x..y",
        "a~1",
        "feat/.hidden",
        "end.lock",
        "@",
        "a//b",
    ] {
        let e = call(
            &f,
            "git_branch",
            json!({"repo": r, "action": "create", "name": bad}),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, ToolError::InvalidInput(_)), "{bad}: {e}");
    }
    let e = call(&f, "git_branch", json!({"repo": r, "action": "create"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::InvalidInput(_)));
    let e = call(
        &f,
        "git_branch",
        json!({"repo": r, "action": "create", "name": "ok", "start_point": "--foo"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn commit_stages_and_commits_only_given_paths() {
    let f = fixture();
    let r = repo_arg(&f);
    std::fs::write(f.repo.join("a.txt"), "one\nmore\n").unwrap();
    std::fs::write(f.repo.join("b.txt"), "bee\nother\n").unwrap();
    std::fs::write(f.repo.join("c.txt"), "brand new\n").unwrap();
    git(&f.repo, &["add", "b.txt"]); // pre-staged, must not be swept into the commit

    let out = call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["a.txt", "c.txt"], "message": "  Add things\n\nbody line  "}),
    )
    .await
    .unwrap();
    assert_eq!(out["subject"], "Add things");
    assert_eq!(out["commit"].as_str().unwrap().len(), 40);

    let show = call(&f, "git_show", json!({"repo": r, "stat": true}))
        .await
        .unwrap();
    assert!(text(&show).contains("a.txt") && text(&show).contains("c.txt"));
    assert!(!text(&show).contains("b.txt"), "{}", text(&show));
    assert!(text(&show).contains("body line"));
    let status = call(&f, "git_status", json!({"repo": r})).await.unwrap();
    assert!(text(&status).contains("M  b.txt"), "{}", text(&status));
}

#[tokio::test]
async fn commit_in_fresh_repo_and_of_deletion() {
    let f = fixture();
    let fresh = f.root.join("fresh");
    init_repo(&fresh);
    std::fs::write(fresh.join("x.txt"), "x\n").unwrap();
    let r = fresh.display().to_string();
    call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["x.txt"], "message": "first"}),
    )
    .await
    .unwrap();
    let log = call(&f, "git_log", json!({"repo": r})).await.unwrap();
    assert_eq!(log["commits"].as_array().unwrap().len(), 1);

    std::fs::remove_file(fresh.join("x.txt")).unwrap();
    call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["x.txt"], "message": "remove"}),
    )
    .await
    .unwrap();
    assert!(!fresh.join("x.txt").exists());
    let log = call(&f, "git_log", json!({"repo": r})).await.unwrap();
    assert_eq!(log["commits"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn commit_validates_input_and_reports_nothing_to_commit() {
    let f = fixture();
    let r = repo_arg(&f);
    for input in [
        json!({"repo": r, "paths": [], "message": "m"}),
        json!({"repo": r, "paths": ["a.txt"], "message": "   "}),
        json!({"repo": r, "paths": ["-a"], "message": "m"}),
        json!({"repo": r, "paths": ["../x"], "message": "m"}),
        json!({"repo": r, "paths": ["a.txt"]}),
        json!({"repo": r, "paths": ["a.txt"], "message": "m", "amend": true}),
    ] {
        let e = call(&f, "git_commit", input.clone()).await.unwrap_err();
        assert!(matches!(e, ToolError::InvalidInput(_)), "{input}: {e}");
    }
    let e = call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["a.txt"], "message": "m"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ToolError::Execution(_)), "{e}");
    assert!(e.to_string().to_lowercase().contains("nothing"), "{e}");
}

#[tokio::test]
async fn hooks_are_not_run() {
    let f = fixture();
    let r = repo_arg(&f);
    let marker = f.root.join("hook-ran");
    let hooks = f.repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in ["pre-commit", "commit-msg", "post-commit", "post-checkout"] {
        let p = hooks.join(hook);
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\necho ran > \"{}\"\n",
                marker.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    std::fs::write(f.repo.join("a.txt"), "hooked\n").unwrap();
    call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["a.txt"], "message": "m"}),
    )
    .await
    .unwrap();
    call(
        &f,
        "git_branch",
        json!({"repo": r, "action": "create", "name": "h", "switch": true}),
    )
    .await
    .unwrap();
    assert!(!marker.exists(), "a git hook ran");
}

#[tokio::test]
async fn option_injection_is_rejected() {
    let f = fixture();
    let r = repo_arg(&f);
    let victim = f.root.join("pwned.txt");
    let out_opt = format!("--output={}", victim.display());
    for (tool, input) in [
        ("git_diff", json!({"repo": r, "paths": [out_opt.clone()]})),
        ("git_diff", json!({"repo": r, "rev": out_opt.clone()})),
        ("git_diff", json!({"repo": r, "paths": ["--no-index"]})),
        ("git_log", json!({"repo": r, "rev": "--all"})),
        ("git_log", json!({"repo": r, "paths": ["-p"]})),
        ("git_show", json!({"repo": r, "rev": out_opt.clone()})),
        ("git_show", json!({"repo": r, "paths": ["--help"]})),
        ("git_show", json!({"repo": r, "rev": "HEAD; echo hi"})),
        ("git_diff", json!({"repo": r, "paths": [":(top)a.txt"]})),
        ("git_diff", json!({"repo": r, "paths": ["/etc/passwd"]})),
        ("git_diff", json!({"repo": r, "paths": ["sub/../../x"]})),
        ("git_diff", json!({"repo": r, "context_lines": 1000})),
    ] {
        let e = call(&f, tool, input.clone()).await.unwrap_err();
        assert!(
            matches!(e, ToolError::InvalidInput(_)),
            "{tool} {input}: {e}"
        );
    }
    assert!(!victim.exists());
}

#[tokio::test]
async fn unknown_fields_are_rejected() {
    let f = fixture();
    let e = call(&f, "git_status", json!({"repo": repo_arg(&f), "extra": 1}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn pathspec_magic_is_literal() {
    let f = fixture();
    let r = repo_arg(&f);
    std::fs::write(f.repo.join("a.txt"), "one\nglob\n").unwrap();
    // `*.txt` must not expand: it names no file, so nothing matches.
    let none = call(&f, "git_diff", json!({"repo": r, "paths": ["*.txt"]}))
        .await
        .unwrap();
    assert_eq!(text(&none), "(no differences)");
}

#[tokio::test]
async fn repo_outside_workspace_is_denied() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    init_repo(outside.path());
    for tool in GIT_TOOL_NAMES {
        let input = match tool {
            "git_branch" => json!({"repo": outside.path().display().to_string(), "action": "list"}),
            "git_commit" => {
                json!({"repo": outside.path().display().to_string(), "paths": ["a"], "message": "m"})
            }
            _ => json!({"repo": outside.path().display().to_string()}),
        };
        let e = call(&f, tool, input).await.unwrap_err();
        assert!(matches!(e, ToolError::PermissionDenied(_)), "{tool}: {e}");
    }
    // `..` escape out of the root is also caught after canonicalisation.
    let escape = format!(
        "{}/../../{}",
        f.repo.display(),
        outside.path().file_name().unwrap().to_string_lossy()
    );
    let e = call(&f, "git_status", json!({"repo": escape})).await;
    assert!(e.is_err());
}

#[tokio::test]
async fn directory_inside_workspace_but_repo_root_outside_is_denied() {
    // Workspace root is a plain subdirectory of an outer repository.
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let inner = tmp.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let reg = ToolRegistry::new();
    let _ = register(&reg, &DevToolsConfig::new(vec![inner.clone()]));
    let tool = reg.get("git_status").unwrap();
    let e = tool
        .call(
            &ToolContext::default(),
            json!({"repo": inner.display().to_string()}),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::PermissionDenied(_)), "{e}");
}

#[tokio::test]
async fn no_roots_means_denied() {
    let reg = ToolRegistry::new();
    let _ = register(&reg, &DevToolsConfig::new(vec![]));
    let e = reg
        .get("git_status")
        .unwrap()
        .call(&ToolContext::default(), json!({}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::PermissionDenied(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_escape_is_denied() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    init_repo(outside.path());
    std::os::unix::fs::symlink(outside.path(), f.root.join("link")).unwrap();
    let e = call(&f, "git_status", json!({"repo": "link"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::PermissionDenied(_)), "{e}");
}

#[tokio::test]
async fn missing_git_binary_is_an_execution_error() {
    let f = fixture();
    let mut cfg = DevToolsConfig::new(vec![f.root.clone()]);
    cfg.git_binary = "definitely-not-a-git-binary".into();
    let reg = ToolRegistry::new();
    let _ = register(&reg, &cfg);
    let e = reg
        .get("git_status")
        .unwrap()
        .call(&ToolContext::default(), json!({"repo": repo_arg(&f)}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::Execution(_)), "{e}");
}

#[tokio::test]
async fn process_list_finds_this_process() {
    let f = fixture();
    let all = call(&f, PROCESS_LIST_TOOL_NAME, json!({"limit": 500}))
        .await
        .unwrap();
    let me = u64::from(std::process::id());
    let procs = all["processes"].as_array().unwrap();
    assert!(procs.len() > 1);
    assert!(all["total_matching"].as_u64().unwrap() >= procs.len() as u64);
    // 500 may not include us on very busy hosts, so look us up by name too.
    let exe = std::env::current_exe().unwrap();
    // Linux truncates process names (comm) to 15 characters.
    let stem: String = exe
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .chars()
        .take(15)
        .collect();
    let by_name = call(
        &f,
        PROCESS_LIST_TOOL_NAME,
        json!({"name_contains": stem.to_uppercase()}),
    )
    .await
    .unwrap();
    assert!(
        by_name["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["pid"] == me),
        "{by_name}"
    );
    let p = &by_name["processes"][0];
    for k in ["pid", "name", "cpu_percent", "memory_bytes", "status"] {
        assert!(p.get(k).is_some(), "{k}");
    }
}

#[tokio::test]
async fn process_list_limit_sort_and_validation() {
    let f = fixture();
    let two = call(
        &f,
        PROCESS_LIST_TOOL_NAME,
        json!({"limit": 2, "sort_by": "pid"}),
    )
    .await
    .unwrap();
    let p = two["processes"].as_array().unwrap();
    assert!(p.len() <= 2);
    if p.len() == 2 {
        assert!(p[0]["pid"].as_u64() <= p[1]["pid"].as_u64());
    }
    let none = call(
        &f,
        PROCESS_LIST_TOOL_NAME,
        json!({"name_contains": "zz-no-such-process-zz"}),
    )
    .await
    .unwrap();
    assert_eq!(none["returned"], 0);
    let e = call(&f, PROCESS_LIST_TOOL_NAME, json!({"sort_by": "bogus"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::InvalidInput(_)));
}

fn sh_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

#[tokio::test]
async fn filter_drivers_are_not_run() {
    let f = fixture();
    let r = repo_arg(&f);
    let marker = f.root.join("filter-ran");
    let cmd = format!("touch '{}'; cat", sh_path(&marker));
    git(&f.repo, &["config", "filter.evil.clean", &cmd]);
    git(&f.repo, &["config", "filter.evil.smudge", &cmd]);
    git(&f.repo, &["config", "filter.evil.process", &cmd]);
    std::fs::write(f.repo.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
    std::fs::write(f.repo.join("a.txt"), "changed\n").unwrap();
    call(&f, "git_status", json!({"repo": r})).await.unwrap();
    call(&f, "git_diff", json!({"repo": r})).await.unwrap();
    call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["a.txt", ".gitattributes"], "message": "m"}),
    )
    .await
    .unwrap();
    std::fs::write(f.repo.join("a.txt"), "again\n").unwrap();
    call(&f, "git_diff", json!({"repo": r})).await.unwrap();
    call(
        &f,
        "git_branch",
        json!({"repo": r, "action": "create", "name": "x", "switch": true}),
    )
    .await
    .unwrap();
    assert!(!marker.exists(), "a filter driver ran");
}

#[tokio::test]
async fn signing_programs_are_not_run() {
    let f = fixture();
    let r = repo_arg(&f);
    let marker = f.root.join("gpg-ran");
    let prog = f.root.join("fake-gpg.sh");
    std::fs::write(
        &prog,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", sh_path(&marker)),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let p = sh_path(&prog);
    for (k, v) in [
        ("commit.gpgSign", "true"),
        ("tag.gpgSign", "true"),
        ("log.showSignature", "true"),
        ("gpg.program", p.as_str()),
        ("gpg.ssh.program", p.as_str()),
        ("gpg.x509.program", p.as_str()),
    ] {
        git(&f.repo, &["config", k, v]);
    }
    std::fs::write(f.repo.join("a.txt"), "signed?\n").unwrap();
    call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["a.txt"], "message": "m"}),
    )
    .await
    .unwrap();
    call(&f, "git_log", json!({"repo": r})).await.unwrap();
    call(&f, "git_show", json!({"repo": r})).await.unwrap();
    assert!(!marker.exists(), "a signing program ran");
}

#[tokio::test]
async fn gitdir_file_pointing_outside_is_denied() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    let outside_repo = outside.path().join("o");
    init_repo(&outside_repo);
    let sneaky = f.root.join("sneaky");
    std::fs::create_dir_all(&sneaky).unwrap();
    std::fs::write(
        sneaky.join(".git"),
        format!("gitdir: {}\n", sh_path(&outside_repo.join(".git"))),
    )
    .unwrap();
    std::fs::write(sneaky.join("x.txt"), "x\n").unwrap();
    let r = sneaky.display().to_string();
    let e = call(
        &f,
        "git_commit",
        json!({"repo": r, "paths": ["x.txt"], "message": "m"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ToolError::PermissionDenied(_)), "{e:?}");
    let e = call(
        &f,
        "git_branch",
        json!({"repo": r, "action": "create", "name": "b"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ToolError::PermissionDenied(_)), "{e:?}");
}

#[tokio::test]
async fn branch_list_reports_truncated_flag() {
    let f = fixture();
    let v = call(
        &f,
        "git_branch",
        json!({"repo": repo_arg(&f), "action": "list"}),
    )
    .await
    .unwrap();
    assert_eq!(v["truncated"], json!(false));
}
