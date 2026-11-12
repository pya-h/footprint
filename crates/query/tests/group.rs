use fp_query::TimelineFilter;
use fp_query::overview::{GroupBy, GroupRow, group};
use fp_store::Store;

fn case() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_or_create(dir.path().join("case.db")).unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO runs(id, started_utc) VALUES (1, 0);
             INSERT INTO sources(id, run_id, kind, app, profile) VALUES
               (1, 1, 'chromium.history', 'Brave', 'Default'),
               (2, 1, 'chromium.history', 'Brave', 'Work'),
               (3, 1, 'xdm.downloads', 'xdm', NULL),
               (4, 1, 'dpkg.log', NULL, NULL);
             INSERT INTO events(run_id, source_id, ts_utc, ts_precision, kind, subject, subject_host, confidence, dedup_key) VALUES
               (1, 1, 10, 'exact',      'web.visit',           'https://zoom.us/a',           'zoom.us',     'direct', 'a'),
               (1, 1, 30, 'exact',      'web.visit',           'https://zoom.us/b',           'zoom.us',     'direct', 'b'),
               (1, 2, 20, 'exact',      'web.download',        'https://cdn.zoom.us/x.deb',   'cdn.zoom.us', 'direct', 'c'),
               (1, 3, 40, 'exact',      'dl.manager_download', '/home/u/Downloads/x.deb',     NULL,          'direct', 'd'),
               (1, 3, 50, 'exact',      'dl.manager_download', '/home/u/Downloads/y.deb',     NULL,          'direct', 'e'),
               (1, 4, 0,  'order_only', 'shell.command',       'apt install ./x.deb',         NULL,          'direct', 'f'),
               (1, 4, 60, 'exact',      'pkg.install',         'zoom',                        NULL,          'direct', 'g');",
        )
        .unwrap();
    (dir, store)
}

fn keyed(rows: &[GroupRow]) -> Vec<(&str, u64)> {
    rows.iter().map(|r| (r.key.as_str(), r.count)).collect()
}

#[test]
fn host_source_kind_group_in_sql_with_bounds() {
    let (_dir, store) = case();
    let all = TimelineFilter::default();
    let utc = fp_query::Tz::Utc;

    // host: NULL hosts fold into "-" (two downloads, the untimed shell line, the install);
    // sorted by count desc then key asc
    let hosts = group(&store, &all, GroupBy::Host, None, utc).unwrap();
    assert_eq!(
        keyed(&hosts),
        [("-", 4), ("zoom.us", 2), ("cdn.zoom.us", 1)]
    );
    // first/last are the timed bounds of the group; the untimed shell row is excluded
    let dash = hosts.iter().find(|r| r.key == "-").unwrap();
    assert_eq!((dash.first_ts, dash.last_ts), (Some(40), Some(60)));
    let zoom = hosts.iter().find(|r| r.key == "zoom.us").unwrap();
    assert_eq!((zoom.first_ts, zoom.last_ts), (Some(10), Some(30)));

    // source: app/profile, else app, else kind
    let sources = group(&store, &all, GroupBy::Source, None, utc).unwrap();
    assert_eq!(
        keyed(&sources),
        [
            ("Brave/Default", 2),
            ("dpkg.log", 2),
            ("xdm", 2),
            ("Brave/Work", 1),
        ]
    );

    let kinds = group(&store, &all, GroupBy::Kind, None, utc).unwrap();
    assert_eq!(
        keyed(&kinds),
        [
            ("dl.manager_download", 2),
            ("web.visit", 2),
            ("pkg.install", 1),
            ("shell.command", 1),
            ("web.download", 1),
        ]
    );

    // limit bounds the number of groups, after the count/key sort
    let top = group(&store, &all, GroupBy::Kind, Some(2), utc).unwrap();
    assert_eq!(keyed(&top), [("dl.manager_download", 2), ("web.visit", 2)]);
}

#[test]
fn dir_matches_path_parent_semantics() {
    let (_dir, store) = case();
    let all = TimelineFilter::default();
    let utc = fp_query::Tz::Utc;

    let dirs = group(&store, &all, GroupBy::Dir, None, utc).unwrap();
    // subjects with a '/' use Path::parent verbatim (no slash collapsing); the rest fold into "-".
    // "zoom" has no '/' → "-"; "apt install ./x.deb" has one → parent "apt install ."
    assert_eq!(
        keyed(&dirs),
        [
            ("/home/u/Downloads", 2),
            ("https://zoom.us", 2),
            ("-", 1),
            ("apt install .", 1),
            ("https://cdn.zoom.us", 1),
        ]
    );
}

#[test]
fn minute_uses_the_render_zone_and_marks_untimed() {
    let (_dir, store) = case();
    let all = TimelineFilter::default();

    // all timed rows here land in the same UTC minute (ts in µs < 60s); the untimed row is its own bucket
    let minutes = group(&store, &all, GroupBy::Minute, None, fp_query::Tz::Utc).unwrap();
    let untimed = minutes.iter().find(|r| r.key == "(untimed)").unwrap();
    assert_eq!(untimed.count, 1);
    assert_eq!((untimed.first_ts, untimed.last_ts), (None, None));
    let timed_total: u64 = minutes
        .iter()
        .filter(|r| r.key != "(untimed)")
        .map(|r| r.count)
        .sum();
    assert_eq!(timed_total, 6);

    // a different zone changes the minute key, proving tz is honoured
    let fixed = fp_query::parse_tz("+05:45").unwrap();
    let shifted = group(&store, &all, GroupBy::Minute, None, fixed).unwrap();
    let utc_keys: Vec<&str> = minutes
        .iter()
        .filter(|r| r.key != "(untimed)")
        .map(|r| r.key.as_str())
        .collect();
    let shifted_keys: Vec<&str> = shifted
        .iter()
        .filter(|r| r.key != "(untimed)")
        .map(|r| r.key.as_str())
        .collect();
    assert_ne!(utc_keys, shifted_keys);
}
