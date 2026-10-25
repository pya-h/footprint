use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn run_footprint(args: &[&str]) -> (bool, String, String) {
    let bin = env!("CARGO_BIN_EXE_footprint");
    let out = Command::new(bin)
        .args(args)
        .output()
        .expect("run footprint");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn test_ioc_cli_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let case = dir.path().join("case.db");

    // 1. Add IOCs
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "ioc",
        "add",
        "domain",
        "zoom.us",
        "--note",
        "video domain",
    ]);
    assert!(ok);
    assert!(stdout.contains("added IOC #1: domain zoom.us (video domain)"));

    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "ioc",
        "add",
        "cidr",
        "192.168.1.0/24",
    ]);
    assert!(ok);
    assert!(stdout.contains("added IOC #2: cidr 192.168.1.0/24"));

    // 2. List IOCs
    let (ok, stdout, _) = run_footprint(&["-c", case.to_str().unwrap(), "ioc", "list"]);
    assert!(ok);
    assert!(stdout.contains("zoom.us"));
    assert!(stdout.contains("192.168.1.0/24"));

    // 3. Import IOCs from file
    let ioc_file = dir.path().join("feed.txt");
    fs::write(
        &ioc_file,
        "# Threat Intel Feed\nus09-web-zoom.com # impostor\nsha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef malware\n/tmp/evil* # temp evil\n",
    )
    .unwrap();

    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "ioc",
        "import",
        ioc_file.to_str().unwrap(),
    ]);
    assert!(ok);
    assert!(stdout.contains("imported 3 IOCs"));

    // Verify imported
    let (ok, stdout, _) = run_footprint(&["-c", case.to_str().unwrap(), "ioc", "list"]);
    assert!(ok);
    assert!(stdout.contains("us09-web-zoom.com"));
    assert!(stdout.contains("malware"));
    assert!(stdout.contains("/tmp/evil*"));
}

#[test]
fn test_ioc_check_inline_list_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let case = dir.path().join("case.db");

    // Synthesize case using fake zoom scenario
    let scenario = fp_fixtures::fake_zoom(dir.path()).unwrap();

    let (ok, _, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "collect",
        "--since",
        "2026-10-16T20:00",
        "--sources",
        "all",
        "--home",
        scenario.home.to_str().unwrap(),
        "--root",
        scenario.root.to_str().unwrap(),
        "--assume-tz",
        fp_fixtures::SCENARIO_TZ,
    ]);
    assert!(ok);

    // Add IOC for us09-web-zoom.com and zoom_amd64.deb
    let (ok, _, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "ioc",
        "add",
        "domain",
        "us09-web-zoom.com",
        "--note",
        "malicious meeting portal",
    ]);
    assert!(ok);

    let (ok, _, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "ioc",
        "add",
        "path",
        "*/zoom_amd64.deb",
        "--note",
        "fake installer",
    ]);
    assert!(ok);

    // 1. ioc check (table)
    let (ok, stdout, _) = run_footprint(&["-c", case.to_str().unwrap(), "ioc", "check"]);
    assert!(ok);
    assert!(stdout.contains("us09-web-zoom.com"));
    assert!(stdout.contains("malicious meeting portal"));

    // 2. ioc check (jsonl)
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "ioc",
        "check",
        "--format",
        "jsonl",
    ]);
    assert!(ok);
    assert!(stdout.contains("\"ioc_value\":\"us09-web-zoom.com\""));

    // 2b. ioc check (csv)
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "ioc",
        "check",
        "--format",
        "csv",
    ]);
    assert!(ok);
    assert!(stdout.contains("target_type,target_id"));
    assert!(stdout.contains("us09-web-zoom.com"));

    // 3. list shows inline [IOC: ...] badge
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "list",
        "--host",
        "us09-web-zoom.com",
    ]);
    assert!(ok);
    assert!(stdout.contains("[IOC: us09-web-zoom.com]"));

    // 3b. list --format csv
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "list",
        "--host",
        "us09-web-zoom.com",
        "--format",
        "csv",
    ]);
    assert!(ok);
    assert!(stdout.contains("id,time,source,kind,summary,ioc_hits"));
    assert!(stdout.contains("us09-web-zoom.com"));

    // 3c. list --group-by host --format csv
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "list",
        "--group-by",
        "host",
        "--format",
        "csv",
    ]);
    assert!(ok);
    assert!(stdout.contains("count,group"));
    assert!(stdout.contains("us09-web-zoom.com"));

    // 4. export --format jsonl --redact
    let jsonl_out = dir.path().join("case_export.jsonl");
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "export",
        "--format",
        "jsonl",
        "--out",
        jsonl_out.to_str().unwrap(),
        "--redact",
    ]);
    assert!(ok);
    assert!(stdout.contains("exported"));
    let mode = fs::metadata(&jsonl_out).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let jsonl_data = fs::read_to_string(&jsonl_out).unwrap();
    assert!(jsonl_data.contains("\"ioc_hits\":[\"us09-web-zoom.com\"]"));

    // 5. export --format csv
    let csv_out = dir.path().join("case_export.csv");
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "export",
        "--format",
        "csv",
        "--out",
        csv_out.to_str().unwrap(),
    ]);
    assert!(ok);
    assert!(stdout.contains("exported"));
    let csv_data = fs::read_to_string(&csv_out).unwrap();
    assert!(csv_data.starts_with("id,time_utc"));
    assert!(csv_data.contains("us09-web-zoom.com"));

    // 6. export --format html --redact
    let html_out = dir.path().join("case_export.html");
    let (ok, stdout, _) = run_footprint(&[
        "-c",
        case.to_str().unwrap(),
        "--tz",
        "UTC",
        "export",
        "--format",
        "html",
        "--out",
        html_out.to_str().unwrap(),
        "--redact",
    ]);
    assert!(ok);
    assert!(stdout.contains("exported"));
    let html_data = fs::read_to_string(&html_out).unwrap();
    assert!(html_data.contains("<!DOCTYPE html>"));
    assert!(html_data.contains("footprint"));
    assert!(html_data.contains("us09-web-zoom.com"));
    assert!(html_data.contains("IOC Detections"));
    // the global --tz names the zone of the time column
    assert!(html_data.contains("Time (UTC)"));
}
