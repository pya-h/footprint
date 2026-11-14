use fp_fixtures::{SCENARIO_TZ, fake_zoom, local};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

struct Case {
    _dir: tempfile::TempDir,
    db: std::path::PathBuf,
}

fn setup() -> Case {
    let dir = tempfile::tempdir().unwrap();
    let scenario = fake_zoom(dir.path()).unwrap();
    let db = dir.path().join("case.db");
    footprint(
        &db,
        &[
            "collect",
            "--home",
            scenario.home.to_str().unwrap(),
            "--root",
            scenario.root.to_str().unwrap(),
            "--assume-tz",
            SCENARIO_TZ,
            "--since",
            &format!("@{}", local(0, 0, 0)),
            "--until",
            &format!("@{}", local(23, 59, 59)),
        ],
    );
    Case { _dir: dir, db }
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

fn events(db: &Path, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["list", "--format", "jsonl"];
    all.extend_from_slice(args);
    footprint(db, &all)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn ids(rows: &[Value]) -> Vec<u64> {
    let mut ids: Vec<u64> = rows.iter().map(|r| r["id"].as_u64().unwrap()).collect();
    ids.sort_unstable();
    ids
}

#[test]
fn include_and_exclude_flags_partition_the_timeline() {
    let case = setup();
    let all = events(&case.db, &[]);
    assert!(!all.is_empty());

    // --exclude-kind is the exact complement of --kind
    let web = events(&case.db, &["--kind", "web.*"]);
    let no_web = events(&case.db, &["--exclude-kind", "web.*"]);
    let mut complement = ids(&web);
    complement.extend(ids(&no_web));
    complement.sort_unstable();
    assert_eq!(complement, ids(&all));

    // exclusions match subdomains like inclusions do
    let no_zoom = events(&case.db, &["--exclude-host", "zoom.us"]);
    assert!(
        no_zoom
            .iter()
            .all(|r| r["subject_host"].as_str() != Some("zoom.us")
                && r["subject_host"].as_str() != Some("cdn.zoom.us"))
    );
    assert_eq!(
        no_zoom.len() + events(&case.db, &["--host", "zoom.us"]).len(),
        all.len()
    );

    // --exclude-source accepts the same source names as --source
    let xdm = events(&case.db, &["--source", "xdm"]);
    assert!(!xdm.is_empty());
    let no_xdm = events(&case.db, &["--exclude-source", "xdm"]);
    assert!(
        no_xdm
            .iter()
            .all(|r| r["source_kind"].as_str() != Some("xdm.downloads"))
    );
    assert_eq!(no_xdm.len() + xdm.len(), all.len());
}

#[test]
fn run_confidence_and_timing_flags_narrow_the_scope() {
    let case = setup();
    let all = events(&case.db, &[]);

    // a single-run case: --run 1 is everything, --run 2 is nothing
    assert_eq!(ids(&events(&case.db, &["--run", "1"])), ids(&all));
    assert!(events(&case.db, &["--run", "2"]).is_empty());

    // confidence levels partition the timeline
    let direct = events(&case.db, &["--confidence", "direct"]);
    let derived = events(&case.db, &["--confidence", "derived"]);
    let heuristic = events(&case.db, &["--confidence", "heuristic"]);
    assert!(direct.iter().all(|r| r["confidence"] == "direct"));
    assert!(derived.iter().all(|r| r["confidence"] == "derived"));
    assert!(heuristic.iter().all(|r| r["confidence"] == "heuristic"));
    assert_eq!(direct.len() + derived.len() + heuristic.len(), all.len());
    let both = events(&case.db, &["--confidence", "direct,derived"]);
    assert_eq!(ids(&both), {
        let mut d = ids(&direct);
        d.extend(ids(&derived));
        d.sort_unstable();
        d
    });

    // timed and untimed partition the timeline
    let timed = events(&case.db, &["--timed"]);
    let untimed = events(&case.db, &["--untimed"]);
    assert!(timed.iter().all(|r| r["ts_precision"] != "order_only"));
    assert!(untimed.iter().all(|r| r["ts_precision"] == "order_only"));
    assert!(
        !untimed.is_empty(),
        "the fixture keeps untimed shell history"
    );
    assert_eq!(timed.len() + untimed.len(), all.len());
}

#[test]
fn ioc_only_lists_the_linked_events() {
    let case = setup();
    footprint(&case.db, &["ioc", "add", "domain", "us09-web-zoom.com"]);

    let hits = events(&case.db, &["--ioc-only"]);
    assert!(!hits.is_empty());
    assert!(
        hits.iter()
            .all(|r| !r["ioc_hits"].as_array().unwrap().is_empty())
    );
    // the scope composes: IOC hits outside the window are still excluded by --since/--until
    let scoped = events(
        &case.db,
        &["--ioc-only", "--since", &format!("@{}", local(21, 0, 0))],
    );
    assert!(
        scoped
            .iter()
            .all(|r| r["ts_utc"].as_i64().unwrap() >= local(21, 0, 0) * 1_000_000)
    );
    assert!(scoped.len() < hits.len());
}

#[test]
fn export_accepts_the_same_scope_flags() {
    let case = setup();
    let out = case._dir.path().join("export.jsonl");
    footprint(
        &case.db,
        &[
            "export",
            "--format",
            "jsonl",
            "--out",
            out.to_str().unwrap(),
            "--exclude-kind",
            "web.*",
            "--timed",
        ],
    );
    let exported: Vec<Value> = std::fs::read_to_string(&out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let listed = events(&case.db, &["--exclude-kind", "web.*", "--timed"]);
    assert_eq!(ids(&exported), ids(&listed));
}
