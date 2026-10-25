//! multi-user: every `--home` is read by the per-home collectors, and events
//! found in a home carry the uid and name of the user it belongs to.

use std::os::unix::fs::MetadataExt;
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
fn every_home_is_collected_and_owns_its_events() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let alice = dir.path().join("home/alice");
    let bob = dir.path().join("home/bob");
    for (home, cmd) in [(&alice, "echo from-alice"), (&bob, "echo from-bob")] {
        std::fs::create_dir_all(home).unwrap();
        std::fs::write(home.join(".bash_history"), format!("{cmd}\n")).unwrap();
    }
    // the homes belong to whoever runs the test; passwd in the root names that uid
    let uid = std::fs::metadata(&alice).unwrap().uid();
    std::fs::create_dir_all(root.join("etc")).unwrap();
    std::fs::write(
        root.join("etc/passwd"),
        format!("tester:x:{uid}:{uid}::/home/tester:/bin/bash\n"),
    )
    .unwrap();
    let db = dir.path().join("case.db");

    footprint(
        &db,
        &[
            "collect",
            "--sources",
            "shell.history",
            "--home",
            alice.to_str().unwrap(),
            "--home",
            bob.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ],
    );

    let out = footprint(
        &db,
        &["list", "--kind", "shell.command", "--format", "jsonl"],
    );
    let subjects: Vec<String> = out
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .map(|v| v["subject"].as_str().unwrap().to_string())
        .collect();
    assert!(subjects.contains(&"echo from-alice".to_string()), "{out}");
    assert!(subjects.contains(&"echo from-bob".to_string()), "{out}");

    let id = out
        .lines()
        .next()
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .and_then(|v| v["id"].as_u64())
        .unwrap();
    let shown = footprint(&db, &["show", &id.to_string()]);
    assert!(shown.contains(&format!("\"uid\":{uid}")), "{shown}");
    assert!(shown.contains("\"user\":\"tester\""), "{shown}");
}
