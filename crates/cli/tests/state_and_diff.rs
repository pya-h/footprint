use serde_json::Value;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

struct Case {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    home: std::path::PathBuf,
    db: std::path::PathBuf,
}

fn setup() -> Case {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let home = root.join("home/testuser");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(root.join("etc/cron.d")).unwrap();
    fs::create_dir_all(root.join("etc/systemd/system")).unwrap();
    fs::create_dir_all(root.join("etc/sudoers.d")).unwrap();

    let db = dir.path().join("case.db");
    Case {
        _dir: dir,
        root,
        home,
        db,
    }
}

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

fn collect(case: &Case, sources: &str) -> String {
    footprint(
        &case.db,
        &[
            "collect",
            "--home",
            case.home.to_str().unwrap(),
            "--root",
            case.root.to_str().unwrap(),
            "--sources",
            sources,
        ],
    )
}

#[test]
fn filesystem_statx_and_group_by() {
    let case = setup();

    let dl_dir = case.home.join("Downloads");
    fs::create_dir_all(&dl_dir).unwrap();
    fs::write(dl_dir.join("pkg1.deb"), "data1").unwrap();
    fs::write(dl_dir.join("pkg2.deb"), "data2").unwrap();

    let doc_dir = case.home.join("Documents");
    fs::create_dir_all(&doc_dir).unwrap();
    fs::write(doc_dir.join("file.txt"), "text").unwrap();

    // Noise dir should be ignored
    let cache_dir = case.home.join(".cache/foo");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("cached.bin"), "noise").unwrap();

    collect(&case, "fs.statx");

    // Check list table
    let list_out = footprint(&case.db, &["list", "--kind", "file.*"]);
    assert!(list_out.contains("pkg1.deb"));
    assert!(list_out.contains("pkg2.deb"));
    assert!(list_out.contains("file.txt"));
    assert!(!list_out.contains("cached.bin"));

    // Check list with group-by dir
    let group_out = footprint(&case.db, &["list", "--kind", "file.*", "--group-by", "dir"]);
    assert!(group_out.contains("COUNT  GROUP"));
    assert!(group_out.contains("Downloads"));
    assert!(group_out.contains("Documents"));

    // Check jsonl output
    let jsonl_out = footprint(
        &case.db,
        &[
            "list",
            "--kind",
            "file.*",
            "--group-by",
            "dir",
            "--format",
            "jsonl",
        ],
    );
    let mut found_dl = false;
    for line in jsonl_out.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        if v["group"].as_str().unwrap().contains("Downloads") {
            assert_eq!(v["count"].as_u64().unwrap(), 2);
            found_dl = true;
        }
    }
    assert!(found_dl);
}

#[test]
fn persistence_snapshot_attention_flags() {
    let case = setup();

    // 1. Backdoor UID 0 user in /etc/passwd
    let passwd_content = "root:x:0:0:root:/root:/bin/bash\n\
                          toor:x:0:0:backdoor:/root:/bin/bash\n\
                          testuser:x:1000:1000::/home/testuser:/bin/bash\n";
    fs::write(case.root.join("etc/passwd"), passwd_content).unwrap();

    // 2. Shell rc with world-writable directory in PATH
    let bashrc_content = "export BUN_INSTALL=\"/tmp/bun\"\n\
                          export PATH=\"$BUN_INSTALL/bin:$PATH\"\n";
    fs::write(case.home.join(".bashrc"), bashrc_content).unwrap();

    // 3. Unowned systemd service
    let service_path = case.root.join("etc/systemd/system/malicious.service");
    fs::write(service_path, "[Service]\nExecStart=/tmp/bad\n").unwrap();

    collect(&case, "persist.snapshot");

    // Inspect state records via jsonl
    let out = footprint(&case.db, &["state", "--format", "jsonl"]);
    let mut saw_uid0 = false;
    let mut saw_ww_path = false;
    let mut saw_unowned_svc = false;

    for line in out.lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let kind = rec["kind"].as_str().unwrap();
        let val: Value = serde_json::from_str(rec["value_json"].as_str().unwrap()).unwrap();
        let flags: Vec<&str> = val["flags"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s.as_str())
            .collect();

        if kind == "persist.account"
            && rec["key"].as_str().unwrap().ends_with("/etc/passwd")
            && flags.contains(&"uid0_non_root")
        {
            saw_uid0 = true;
        } else if kind == "persist.shell_rc"
            && rec["key"].as_str().unwrap().ends_with(".bashrc")
            && flags.contains(&"world_writable_path")
        {
            saw_ww_path = true;
        } else if kind == "persist.systemd"
            && rec["key"].as_str().unwrap().contains("malicious.service")
            && flags.contains(&"unowned")
        {
            saw_unowned_svc = true;
        }
    }

    assert!(saw_uid0, "should flag uid0_non_root");
    assert!(saw_ww_path, "should flag world_writable_path");
    assert!(saw_unowned_svc, "should flag unowned systemd service");
}

#[test]
fn state_filtering_and_diff() {
    let case = setup();

    let cron_file = case.root.join("etc/cron.d/job1");
    fs::write(&cron_file, "* * * * * root /bin/true\n").unwrap();

    // Run 1
    collect(&case, "persist.snapshot");

    let state_run1 = footprint(&case.db, &["state", "--kind", "persist.cron"]);
    assert!(state_run1.contains("job1"));

    // Filter by match
    let match_out = footprint(&case.db, &["state", "--match", "job1"]);
    assert!(match_out.contains("job1"));
    let no_match = footprint(&case.db, &["state", "--match", "nonexistent_pattern"]);
    assert!(no_match.contains("no state records found"));

    // Modify job1 and add job2 for Run 2
    let mut f = File::create(&cron_file).unwrap();
    writeln!(f, "* * * * * root /bin/echo modified").unwrap();
    let cron_file2 = case.root.join("etc/cron.d/job2");
    fs::write(&cron_file2, "* * * * * root /bin/new\n").unwrap();

    // Run 2
    collect(&case, "persist.snapshot");

    // Test diff 1 2
    let diff_out = footprint(&case.db, &["diff", "1", "2"]);
    assert!(diff_out.contains("+    persist.cron"));
    assert!(diff_out.contains("job2"));
    assert!(diff_out.contains("~    persist.cron"));
    assert!(diff_out.contains("job1"));

    // Test diff prev last
    let diff_prev_last = footprint(&case.db, &["diff", "prev", "last"]);
    assert!(diff_prev_last.contains("+    persist.cron"));

    // Test diff jsonl format
    let diff_jsonl = footprint(&case.db, &["diff", "1", "2", "--format", "jsonl"]);
    let mut saw_add = false;
    let mut saw_change = false;
    for line in diff_jsonl.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        if v["diff"] == "+" && v["key"].as_str().unwrap().contains("job2") {
            saw_add = true;
        }
        if v["diff"] == "~" && v["key"].as_str().unwrap().contains("job1") {
            saw_change = true;
        }
    }
    assert!(saw_add);
    assert!(saw_change);
}
