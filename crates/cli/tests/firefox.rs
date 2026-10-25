use fp_fixtures::{firefox_profile, local};
use serde_json::Value;
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

fn events(db: &Path, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["list", "--format", "jsonl"];
    all.extend_from_slice(args);
    footprint(db, &all)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn firefox_profile_collects_visits_downloads_permissions_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let profile = firefox_profile(dir.path()).unwrap();
    let db = dir.path().join("case.db");
    let home = dir.path().join("home/u");

    let since = format!("@{}", local(0, 0, 0));
    let until = format!("@{}", local(23, 59, 59));
    let out = footprint(
        &db,
        &[
            "collect",
            "--sources",
            "firefox.history",
            "--home",
            home.to_str().unwrap(),
            "--since",
            &since,
            "--until",
            &until,
        ],
    );
    assert!(!out.contains("failed"), "{out}");

    // visits and the redirect chain, with from_visit for correlation
    let visits = events(&db, &["--kind", "web.visit"]);
    assert!(
        visits
            .iter()
            .any(|e| e["subject"].as_str().unwrap().contains("mail.google.com"))
    );
    let redirects = events(&db, &["--kind", "web.redirect"]);
    assert_eq!(redirects.len(), 2);
    assert!(redirects.iter().any(|e| {
        e["summary"]
            .as_str()
            .unwrap()
            .contains("[permanent_redirect]")
    }));
    assert!(redirects.iter().any(|e| {
        e["summary"]
            .as_str()
            .unwrap()
            .contains("[temporary_redirect]")
    }));
    // from_visit pointers are stored (correlation chains on them)
    let rid = redirects[0]["id"].as_u64().unwrap();
    let shown = footprint(&db, &["show", &rid.to_string()]);
    assert!(shown.contains("from_visit"), "{shown}");
    assert!(shown.contains("moz_historyvisits.id=2"), "{shown}");

    // download from moz_annos: target path artifact + summary
    let downloads = events(&db, &["--kind", "web.download"]);
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0]["ts_utc"], local(20, 44, 53) * 1_000_000);
    let dl_id = downloads[0]["id"].as_u64().unwrap();
    let dl_show = footprint(&db, &["show", &dl_id.to_string()]);
    assert!(
        dl_show.contains("/home/u/Downloads/zoom_amd64.deb"),
        "{dl_show}"
    );

    // permissions: events + state records
    let perms = events(&db, &["--kind", "web.permission_change"]);
    assert_eq!(perms.len(), 2);
    let state = footprint(&db, &["state", "--kind", "web.site_permission"]);
    assert!(state.contains("camera"));
    assert!(state.contains("desktop-notification"));
    assert!(state.contains("allow"));
    assert!(state.contains("deny"));

    // extension install event; system addon is state only
    let exts = events(&db, &["--kind", "ext.installed"]);
    assert_eq!(exts.len(), 1);
    assert_eq!(
        exts[0]["summary"],
        "uBlock Origin (uBlock0@raymondhill.net)"
    );
    let ext_state = footprint(
        &db,
        &["state", "--kind", "ext.installed", "--format", "jsonl"],
    );
    assert_eq!(ext_state.lines().count(), 2, "both addons are state");

    // provenance points at the real files
    let id = downloads[0]["id"].as_u64().unwrap();
    let show = footprint(&db, &["show", &id.to_string()]);
    assert!(show.contains("places.sqlite"), "{show}");
    assert!(
        show.contains(profile.join("places.sqlite").to_str().unwrap()),
        "{show}"
    );

    // chain correlation works across the from_visit graph
    let trace = footprint(&db, &["trace", "https://us09-web-zoom.com/j/999"]);
    assert!(trace.contains("redirected_from"), "{trace}");
    assert!(trace.contains("site-redirect.com"), "{trace}");
}
