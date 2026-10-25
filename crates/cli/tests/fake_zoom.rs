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
    assert!(journald.contains("not installed"), "{journald}");
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

#[test]
fn coverage_report_prints_categories() {
    let case = setup();
    collect(&case);
    let cov = footprint(&case.db, &["sources", "--coverage"]);
    assert!(cov.contains("Browser history"));
    assert!(cov.contains("Downloads"));
    assert!(cov.contains("Packages"));
    assert!(cov.contains("Covered"));
}

#[test]
fn fake_zoom_trace_matches_scenario() {
    let case = setup();
    collect(&case);

    let deb_path = case.scenario.home.join("Downloads/Programs/zoom_amd64.deb");
    let trace_out = footprint(&case.db, &["trace", deb_path.to_str().unwrap()]);
    assert!(trace_out.contains("artifact"), "{trace_out}");
    assert!(trace_out.contains("downloaded_as"), "{trace_out}");
    assert!(trace_out.contains("dlmgr.xdm"), "{trace_out}");
    assert!(
        trace_out.contains("https://cdn.zoom.us/prod/7.2.1.5760/zoom_amd64.deb"),
        "{trace_out}"
    );
    assert!(
        trace_out.contains("no browser download row: XDM intercepted it"),
        "{trace_out}"
    );
    assert!(trace_out.contains("navigated_from"), "{trace_out}");
    assert!(
        trace_out.contains("https://zoom.us/download"),
        "{trace_out}"
    );
    assert!(trace_out.contains("installed_from"), "{trace_out}");
    assert!(
        trace_out.contains("apt install ./zoom_amd64.deb"),
        "{trace_out}"
    );
    assert!(trace_out.contains("pkg.install"), "{trace_out}");
    assert!(trace_out.contains("zoom:amd64 7.2.1.5760"), "{trace_out}");
}

#[test]
fn bash_anchoring_anchors_untimed_command() {
    let case = setup();
    collect(&case);

    let events_list = events(&case.db, &["--kind", "shell.command"]);
    let bash_apt = events_list
        .iter()
        .find(|e| e["subject"] == "sudo apt install ./zoom_amd64.deb")
        .unwrap();

    assert_eq!(bash_apt["ts_precision"], "second");
    assert_eq!(bash_apt["confidence"], "derived");
    assert_eq!(bash_apt["ts_utc"], local(21, 7, 58) * 1_000_000);
}

#[test]
fn show_displays_provenance_links() {
    let case = setup();
    collect(&case);

    let xdm = events(&case.db, &["--kind", "dl.*"]);
    let id = xdm[0]["id"].as_u64().unwrap();
    let show_out = footprint(&case.db, &["show", &id.to_string()]);
    assert!(show_out.contains("links:"), "{show_out}");
    assert!(show_out.contains("downloaded_as"), "{show_out}");
}

#[test]
fn tz_applies_to_every_read_command() {
    let case = setup();
    collect(&case);

    let dpkg = events(&case.db, &["--kind", "pkg.install", "--source", "pkg.dpkg"]);
    let id = dpkg[0]["id"].as_u64().unwrap().to_string();
    // the dpkg line was written at 21:08:00 +03:30
    let utc = footprint(&case.db, &["show", &id, "--tz", "UTC"]);
    assert!(utc.contains("17:38:00.000000 +00:00"), "{utc}");
    let local = footprint(&case.db, &["--tz", "+03:30", "show", &id]);
    assert!(local.contains("21:08:00.000000 +03:30"), "{local}");

    let listed = footprint(&case.db, &["list", "--kind", "pkg.install", "--tz", "UTC"]);
    assert!(listed.contains("2026-10-16 17:38:00"), "{listed}");
    let searched = footprint(&case.db, &["search", "zoom", "--tz", "+03:30"]);
    assert!(searched.contains("2026-10-16 21:08:00"), "{searched}");

    let deb_path = case.scenario.home.join("Downloads/Programs/zoom_amd64.deb");
    let traced = footprint(
        &case.db,
        &["trace", deb_path.to_str().unwrap(), "--tz", "UTC"],
    );
    assert!(traced.contains("17:38:00"), "{traced}");
}
