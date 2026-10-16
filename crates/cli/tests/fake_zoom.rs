use fp_fixtures::{FAKE_ZOOM_DOMAINS, SCENARIO_TZ, Scenario, fake_zoom, local};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

struct Case {
    _dir: tempfile::TempDir,
    scenario: Scenario,
    db: std::path::PathBuf,
}

fn setup() -> Case {
    let dir = tempfile::tempdir().unwrap();
    let scenario = fake_zoom(dir.path()).unwrap();
    let db = dir.path().join("case.db");
    Case {
        _dir: dir,
        scenario,
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

fn collect(case: &Case) -> String {
    let since = format!("@{}", local(0, 0, 0));
    let until = format!("@{}", local(23, 59, 59));
    footprint(
        &case.db,
        &[
            "collect",
            "--home",
            case.scenario.home.to_str().unwrap(),
            "--root",
            case.scenario.root.to_str().unwrap(),
            "--assume-tz",
            SCENARIO_TZ,
            "--since",
            &since,
            "--until",
            &until,
        ],
    )
}

fn events(db: &Path, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["list", "--format", "jsonl"];
    all.extend_from_slice(args);
    footprint(db, &all)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn summaries(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|r| r["summary"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn reproduces_the_investigation_table() {
    let case = setup();
    let first = collect(&case);
    assert!(!first.contains("failed"), "{first}");

    let redirect = summaries(&events(&case.db, &["--kind", "web.redirect"]));
    assert!(
        redirect.contains(
            &"https://us09-web-zoom.com/api/meetings/download → https://zoom.us/download [3xx]"
                .to_string()
        ),
        "{redirect:?}"
    );
    assert!(redirect.contains(&"https://site-redirect.com/r/fegvyj18sy → https://us09-web-zoom.com/j/88461?pwd=x1 [client]".to_string()), "{redirect:?}");

    let xdm = events(&case.db, &["--kind", "dl.*"]);
    assert_eq!(xdm.len(), 1, "the older XDM row is outside the window");
    let xdm_summary = xdm[0]["summary"].as_str().unwrap();
    assert!(xdm_summary.ends_with("Downloads/Programs/zoom_amd64.deb ← https://cdn.zoom.us/prod/7.2.1.5760/zoom_amd64.deb (302.8 MiB)"), "{xdm_summary}");
    assert_eq!(xdm[0]["ts_utc"], local(21, 6, 20) * 1_000_000);

    let pkg = events(&case.db, &["--kind", "pkg.*"]);
    let pkg_summaries = summaries(&pkg);
    assert!(
        pkg_summaries.contains(&"zoom:amd64 7.2.1.5760".to_string()),
        "{pkg_summaries:?}"
    );
    assert!(
        pkg_summaries
            .contains(&"install: zoom:amd64 (+1 more) ← apt install ./zoom_amd64.deb".to_string()),
        "{pkg_summaries:?}"
    );
    assert!(
        pkg.iter()
            .any(|e| e["ts_utc"] == local(21, 8, 0) * 1_000_000)
    );
    assert!(
        !pkg_summaries.iter().any(|s| s.contains("curl")),
        "the rotated block is outside the window"
    );

    let used = summaries(&events(&case.db, &["--kind", "web.permission_used"]));
    assert_eq!(used.len(), 2, "{used:?}");
    assert!(
        used.iter()
            .all(|s| s.starts_with("https://us09-web-zoom.com:443"))
    );
    assert!(
        events(&case.db, &["--kind", "web.permission_change"]).is_empty(),
        "client_hints/engagement are not permissions; the notification grant is outside the window"
    );

    let ext = summaries(&events(&case.db, &["--kind", "ext.installed"]));
    assert_eq!(
        ext.len(),
        2,
        "component extensions are not install events: {ext:?}"
    );
    assert!(
        ext.iter().any(|s| s.starts_with("MetaMask")),
        "Secure Preferences is read"
    );

    let download = summaries(&events(&case.db, &["--kind", "web.download"]));
    assert_eq!(
        download,
        ["https://cdn.zoom.us/prod/7.2.1.5760/zoom_amd64.deb (0 B, cancelled, not saved)"]
    );

    let shell = summaries(&events(&case.db, &["--kind", "shell.command"]));
    for expected in [
        "sudo apt install ./zoom_amd64.deb",
        "dpkg -I zoom_amd64.deb",
        "echo café",
        "ALTER USER postgres WITH PASSWORD '***';",
    ] {
        assert!(
            shell.contains(&expected.to_string()),
            "{expected}: {shell:?}"
        );
    }
    assert!(!shell.iter().any(|s| s.contains("hunter2")));

    let opened = summaries(&events(&case.db, &["--kind", "file.opened"]));
    assert_eq!(opened.len(), 1);
    assert!(opened[0].ends_with("zoom_amd64.deb [Files]"));

    for domain in FAKE_ZOOM_DOMAINS {
        assert!(
            !events(&case.db, &["--host", domain]).is_empty(),
            "{domain}"
        );
    }
}

#[test]
fn profiles_never_share_side_files() {
    let case = setup();
    collect(&case);
    let profile1: Vec<Value> = events(&case.db, &["--kind", "web.*"])
        .into_iter()
        .filter(|e| e["profile"] == "Profile 1")
        .collect();
    assert_eq!(profile1.len(), 1, "{profile1:?}");
    assert_eq!(profile1[0]["subject_host"], "schedulio.io");
}

#[test]
fn rerun_adds_nothing() {
    let case = setup();
    let first = collect(&case);
    let second = collect(&case);
    assert!(!first.contains(": 0 new events"), "{first}");
    assert!(second.contains(": 0 new events"), "{second}");
    let total: usize = first
        .split(": ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(events(&case.db, &[]).len(), total);
}

#[test]
fn secrets_are_never_read() {
    let case = setup();
    collect(&case);
    let raw = std::fs::read(&case.db).unwrap();
    for secret in [
        "secret cookie jar",
        "xdm-site-password",
        "xdm-proxy-password",
        "hunter2",
    ] {
        assert!(
            !raw.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{secret} leaked into the case"
        );
    }
}

#[test]
fn missing_journal_is_unavailable_not_an_error() {
    let case = setup();
    collect(&case);
    let sources = footprint(&case.db, &["sources"]);
    let journald = sources
        .lines()
        .find(|l| l.contains("auth.journald"))
        .unwrap();
    assert!(journald.contains("unavailable"), "{journald}");
    assert!(!sources.contains("error"), "{sources}");
}

#[test]
fn read_commands_never_create_a_case() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("missing.db");
    let out = Command::new(env!("CARGO_BIN_EXE_footprint"))
        .arg("-c")
        .arg(&db)
        .arg("list")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!db.exists());
}
