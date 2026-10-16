//! Synthetic evidence in the real on-disk formats, for tests. Nothing here comes from a real machine.

use flate2::{Compression, write::GzEncoder};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const WEBKIT_EPOCH_OFFSET_S: i64 = 11_644_473_600;
const DOTNET_EPOCH_OFFSET_S: i64 = 62_135_596_800;
/// `DateTimeKind.Local` in the top bits of `DateTime.ToBinary()`
const DOTNET_KIND_LOCAL: u64 = 0x8000_0000_0000_0000;

/// `ui::PageTransition` values used by the scenario.
pub mod transition {
    pub const LINK: u32 = 0;
    pub const TYPED: u32 = 1;
    pub const CHAIN_START: u32 = 0x1000_0000;
    pub const CHAIN_END: u32 = 0x2000_0000;
    pub const CLIENT_REDIRECT: u32 = 0x4000_0000;
    pub const SERVER_REDIRECT: u32 = 0x8000_0000;
}

pub fn webkit(unix_s: i64) -> i64 {
    (unix_s + WEBKIT_EPOCH_OFFSET_S) * 1_000_000
}

pub fn dotnet_local(unix_s: i64) -> i64 {
    let ticks = (unix_s + DOTNET_EPOCH_OFFSET_S) as u64 * 10_000_000;
    (ticks | DOTNET_KIND_LOCAL) as i64
}

pub struct Visit {
    pub id: i64,
    pub url: &'static str,
    pub title: &'static str,
    pub at: i64,
    pub from_visit: i64,
    pub transition: u32,
}

pub struct Download {
    pub id: i64,
    pub target_path: String,
    pub at: i64,
    pub total_bytes: i64,
    pub state: i64,
    pub tab_url: &'static str,
    pub chain: Vec<&'static str>,
}

/// The tables and columns of a Chromium `History` file that collectors read.
const HISTORY_SCHEMA: &str = "
CREATE TABLE urls (id INTEGER PRIMARY KEY AUTOINCREMENT, url LONGVARCHAR, title LONGVARCHAR,
    visit_count INTEGER DEFAULT 0 NOT NULL, typed_count INTEGER DEFAULT 0 NOT NULL,
    last_visit_time INTEGER NOT NULL, hidden INTEGER DEFAULT 0 NOT NULL);
CREATE TABLE visits (id INTEGER PRIMARY KEY AUTOINCREMENT, url INTEGER NOT NULL, visit_time INTEGER NOT NULL,
    from_visit INTEGER, transition INTEGER DEFAULT 0 NOT NULL, segment_id INTEGER,
    visit_duration INTEGER DEFAULT 0 NOT NULL, opener_visit INTEGER);
CREATE TABLE downloads (id INTEGER PRIMARY KEY, guid VARCHAR NOT NULL, current_path LONGVARCHAR NOT NULL,
    target_path LONGVARCHAR NOT NULL, start_time INTEGER NOT NULL, received_bytes INTEGER NOT NULL,
    total_bytes INTEGER NOT NULL, state INTEGER NOT NULL, danger_type INTEGER NOT NULL,
    interrupt_reason INTEGER NOT NULL, end_time INTEGER NOT NULL, opened INTEGER NOT NULL,
    last_access_time INTEGER NOT NULL, referrer VARCHAR NOT NULL, tab_url VARCHAR NOT NULL,
    tab_referrer_url VARCHAR NOT NULL, mime_type VARCHAR(255) NOT NULL, original_mime_type VARCHAR(255) NOT NULL);
CREATE TABLE downloads_url_chains (id INTEGER NOT NULL, chain_index INTEGER NOT NULL, url LONGVARCHAR NOT NULL,
    PRIMARY KEY (id, chain_index));
CREATE TABLE keyword_search_terms (keyword_id INTEGER NOT NULL, url_id INTEGER NOT NULL, term LONGVARCHAR NOT NULL,
    normalized_term LONGVARCHAR NOT NULL);
";

/// Writes a `History` DB. With `hold_wal`, the returned connection keeps the file in WAL mode with
/// rows that only exist in `History-wal`, the way a running browser does.
pub fn chromium_history(
    path: &Path,
    visits: &[Visit],
    downloads: &[Download],
    hold_wal: bool,
) -> rusqlite::Result<Option<Connection>> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(HISTORY_SCHEMA)?;
    if hold_wal {
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.query_row("PRAGMA wal_autocheckpoint = 0", [], |_| Ok(()))?;
    }
    for v in visits {
        conn.execute(
            "INSERT INTO urls (id, url, title, last_visit_time) VALUES (?1, ?2, ?3, ?4)",
            params![v.id, v.url, v.title, webkit(v.at)],
        )?;
        conn.execute(
            "INSERT INTO visits (id, url, visit_time, from_visit, transition) VALUES (?1, ?1, ?2, ?3, ?4)",
            params![v.id, webkit(v.at), v.from_visit, v.transition],
        )?;
    }
    for d in downloads {
        conn.execute(
            "INSERT INTO downloads VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?5, ?6, 0, 0, ?4, 0, 0, '', ?7, '', 'application/octet-stream', '')",
            params![d.id, format!("guid-{}", d.id), d.target_path, webkit(d.at), d.total_bytes, d.state, d.tab_url],
        )?;
        for (i, url) in d.chain.iter().enumerate() {
            conn.execute(
                "INSERT INTO downloads_url_chains VALUES (?1, ?2, ?3)",
                params![d.id, i as i64, url],
            )?;
        }
    }
    if hold_wal {
        Ok(Some(conn))
    } else {
        conn.close().map_err(|(_, e)| e)?;
        Ok(None)
    }
}

pub struct XdmRow {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub targetdir: String,
    pub size: i64,
    pub at: i64,
}

pub fn xdm_db(path: &Path, rows: &[XdmRow]) -> rusqlite::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE downloads(id TEXT PRIMARY KEY, completed INT, name TEXT, date_added INT, size INT,
            status INT, progress INT, download_type TEXT, filenamefetchmode INT, maxspeedlimitinkib INT,
            targetdir TEXT, primary_url TEXT, referer_url TEXT, auth INT, user TEXT, pass TEXT, proxy INT,
            proxy_host TEXT, proxy_port INT, proxy_user TEXT, proxy_pass TEXT, proxy_type INT) WITHOUT ROWID;",
    )?;
    for r in rows {
        conn.execute(
            "INSERT INTO downloads (id, completed, name, date_added, size, targetdir, primary_url, pass, proxy_pass)
             VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, 'xdm-site-password', 'xdm-proxy-password')",
            params![r.id, r.name, dotnet_local(r.at), r.size, r.targetdir, r.url],
        )?;
    }
    Ok(())
}

pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, contents)
}

pub fn write_gz(path: &Path, contents: &str) -> io::Result<()> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(contents.as_bytes())?;
    write(path, gz.finish()?)
}

pub fn json_file(path: &Path, value: &Value) -> io::Result<()> {
    write(path, value.to_string())
}

/// 2026-10-16 at `h:m:s` in the scenario's local zone (+03:30), as unix seconds.
pub fn local(h: i64, m: i64, s: i64) -> i64 {
    const MIDNIGHT_LOCAL_UTC: i64 = 1_792_096_200;
    MIDNIGHT_LOCAL_UTC + h * 3600 + m * 60 + s
}

pub const SCENARIO_TZ: &str = "+03:30";
pub const FAKE_ZOOM_DOMAINS: [&str; 5] = [
    "page-redirect.com",
    "site-redirect.com",
    "us09-web-zoom.com",
    "schedulio.io",
    "mblocklab.com",
];

pub struct Scenario {
    pub home: PathBuf,
    pub root: PathBuf,
    /// keeps Brave's `Default/History` locked in WAL mode, as a running browser would
    pub browser_lock: Option<Connection>,
}

/// The fake-Zoom interview lure of 2026-10-16: a Gmail link through two redirectors to a fake Zoom
/// page, a camera/mic grant, the "SDK update" button redirecting to the real zoom.us download,
/// XDM fetching the deb, and the install through sudo apt.
pub fn fake_zoom(base: &Path) -> io::Result<Scenario> {
    let home = base.join("home/u");
    let root = base.join("root");
    let brave = home.join(".config/BraveSoftware/Brave-Browser");
    let programs = home.join("Downloads/Programs");
    let sqlite = |e: rusqlite::Error| io::Error::other(e);
    use transition::*;

    let visits = [
        Visit {
            id: 1,
            url: "https://mail.google.com/mail/u/0/#inbox/FMfcgz",
            title: "Interview invitation - Gmail",
            at: local(20, 43, 50),
            from_visit: 0,
            transition: TYPED | CHAIN_START | CHAIN_END,
        },
        Visit {
            id: 2,
            url: "https://page-redirect.com/fegvyj18sy",
            title: "",
            at: local(20, 44, 6),
            from_visit: 1,
            transition: LINK | CHAIN_START,
        },
        Visit {
            id: 3,
            url: "https://site-redirect.com/r/fegvyj18sy",
            title: "",
            at: local(20, 44, 7),
            from_visit: 2,
            transition: LINK | SERVER_REDIRECT,
        },
        Visit {
            id: 4,
            url: "https://us09-web-zoom.com/j/88461?pwd=x1",
            title: "Zoom Meeting",
            at: local(20, 44, 8),
            from_visit: 3,
            transition: LINK | CLIENT_REDIRECT | CHAIN_END,
        },
        Visit {
            id: 5,
            url: "https://mblocklab.com/",
            title: "MBlock Lab",
            at: local(20, 35, 0),
            from_visit: 0,
            transition: TYPED | CHAIN_START | CHAIN_END,
        },
        Visit {
            id: 6,
            url: "https://us09-web-zoom.com/api/meetings/download",
            title: "",
            at: local(21, 3, 16),
            from_visit: 4,
            transition: LINK | CHAIN_START,
        },
        Visit {
            id: 7,
            url: "https://zoom.us/download",
            title: "Download Center - Zoom",
            at: local(21, 3, 17),
            from_visit: 6,
            transition: LINK | SERVER_REDIRECT | CHAIN_END,
        },
    ];
    let downloads = [Download {
        id: 1,
        target_path: String::new(),
        at: local(21, 5, 0),
        total_bytes: 0,
        state: 2,
        tab_url: "https://zoom.us/download",
        chain: vec![
            "https://zoom.us/client/latest/zoom_amd64.deb",
            "https://cdn.zoom.us/prod/7.2.1.5760/zoom_amd64.deb",
        ],
    }];
    let browser_lock = chromium_history(&brave.join("Default/History"), &visits, &downloads, true)
        .map_err(sqlite)?;
    write(&brave.join("Default/Cookies"), "secret cookie jar")?;

    let profile1 = [Visit {
        id: 1,
        url: "https://schedulio.io/book/interview-31",
        title: "Schedulio",
        at: local(20, 30, 0),
        from_visit: 0,
        transition: LINK | CHAIN_START | CHAIN_END,
    }];
    chromium_history(&brave.join("Profile 1/History"), &profile1, &[], false).map_err(sqlite)?;

    let fake_origin = "https://us09-web-zoom.com:443,*";
    json_file(
        &brave.join("Default/Preferences"),
        &json!({
            "profile": { "content_settings": { "exceptions": {
                "media_stream_camera": { fake_origin: { "last_used": webkit(local(20, 46, 0)).to_string() } },
                "media_stream_mic": { fake_origin: { "last_used": webkit(local(20, 46, 1)).to_string() } },
                "notifications": { "https://umbra-labs.example:443,*": {
                    "setting": 1, "last_modified": webkit(local(20, 50, 0) - 30 * 86_400).to_string() } },
                "client_hints": { fake_origin: { "last_modified": webkit(local(20, 44, 9)).to_string(), "setting": { "client_hints": [1] } } },
                "site_engagement": { fake_origin: { "last_modified": webkit(local(20, 44, 9)).to_string(), "setting": { "rawScore": 3.0 } } },
            } } },
            "extensions": { "settings": {
                "cjpalhdlnbpafiamejdnhcphjbkeiagm": {
                    "first_install_time": webkit(local(20, 50, 0)).to_string(),
                    "location": 1, "from_webstore": true,
                    "manifest": { "name": "uBlock Origin", "version": "1.60.0" } },
                "mhjfbmdgcfjbbpaeojofohoefgiehjai": {
                    "install_time": webkit(local(20, 50, 0)).to_string(),
                    "location": 5, "manifest": { "name": "Chrome PDF Viewer" } },
            } },
        }),
    )?;
    json_file(
        &brave.join("Default/Secure Preferences"),
        &json!({ "extensions": { "settings": {
        "nkbihfbeogaeaoehlefnkodbefgpgknn": {
            "first_install_time": webkit(local(20, 52, 0)).to_string(),
            "location": 1, "from_webstore": true,
            "manifest": { "name": "MetaMask", "version": "12.0.0" } },
    } } }),
    )?;

    let targetdir = programs.to_string_lossy().into_owned();
    xdm_db(
        &home.join(".xdm-app-data/downloads.db"),
        &[
            XdmRow {
                id: "0f9c",
                name: "zoom_amd64.deb",
                url: "https://cdn.zoom.us/prod/7.2.1.5760/zoom_amd64.deb",
                targetdir: targetdir.clone(),
                size: 317_514_752,
                at: local(21, 6, 20),
            },
            XdmRow {
                id: "77aa",
                name: "zoom_amd64.deb",
                url: "https://cdn.zoom.us/prod/6.9.0.100/zoom_amd64.deb",
                targetdir,
                size: 300_000_000,
                at: local(9, 0, 0) - 45 * 86_400,
            },
        ],
    )
    .map_err(sqlite)?;
    write(&programs.join("zoom_amd64.deb"), "!<arch>\n")?;

    write(
        &root.join("var/log/dpkg.log"),
        "2026-10-16 21:08:00 startup archives unpack\n\
         2026-10-16 21:08:00 install zoom:amd64 <none> 7.2.1.5760\n\
         2026-10-16 21:08:00 status half-installed zoom:amd64 7.2.1.5760\n\
         2026-10-16 21:08:03 status installed zoom:amd64 7.2.1.5760\n",
    )?;
    write(
        &root.join("var/log/apt/history.log"),
        "\nStart-Date: 2026-10-16  21:07:58\n\
         Commandline: apt install ./zoom_amd64.deb\n\
         Requested-By: u (1000)\n\
         Install: zoom:amd64 (7.2.1.5760), libxcb-xtest0:amd64 (1.15-1, automatic)\n\
         End-Date: 2026-10-16  21:08:05\n",
    )?;
    write_gz(
        &root.join("var/log/apt/history.log.1.gz"),
        "\nStart-Date: 2026-09-01  10:00:00\nCommandline: apt upgrade\nUpgrade: curl:amd64 (8.5.0-1, 8.5.0-2)\nEnd-Date: 2026-09-01  10:00:09\n",
    )?;

    write(
        &home.join(".bash_history"),
        "ls\ncd ~/Downloads/Programs\nsudo apt install ./zoom_amd64.deb\n",
    )?;
    write(
        &home.join(".local/share/fish/fish_history"),
        format!(
            "- cmd: file ~/Downloads/Programs/zoom_amd64.deb\n  when: {}\n  paths:\n    - ~/Downloads/Programs/zoom_amd64.deb\n- cmd: dpkg -I zoom_amd64.deb\n  when: {}\n",
            local(21, 7, 40),
            local(21, 7, 45)
        ),
    )?;
    write(
        &home.join(".zsh_history"),
        [
            format!(": {}:0;echo caf", local(21, 9, 0)).as_bytes(),
            &[0xc3, 0xa9],
            b"\n",
        ]
        .concat(),
    )?;
    write(
        &home.join(".psql_history"),
        "ALTER USER postgres WITH PASSWORD 'hunter2';\n\\q\n",
    )?;
    write(
        &home.join(".local/share/recently-used.xbel"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks" xmlns:mime="http://www.freedesktop.org/standards/shared-mime-info">
  <bookmark href="file://{}/zoom_amd64.deb" added="2026-10-16T17:37:30Z" modified="2026-10-16T17:37:30Z" visited="2026-10-16T17:37:30Z">
    <info><metadata owner="http://freedesktop.org">
      <mime:mime-type type="application/vnd.debian.binary-package"/>
      <bookmark:applications>
        <bookmark:application name="Files" exec="&apos;nautilus %u&apos;" modified="2026-10-16T17:37:30Z" count="1"/>
      </bookmark:applications>
    </metadata></info>
  </bookmark>
</xbel>
"#,
            programs.display()
        ),
    )?;

    Ok(Scenario {
        home,
        root,
        browser_lock,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_history_keeps_rows_in_the_wal_only() {
        let dir = tempfile::tempdir().unwrap();
        let scenario = fake_zoom(dir.path()).unwrap();
        let history = scenario
            .home
            .join(".config/BraveSoftware/Brave-Browser/Default/History");
        assert!(history.with_file_name("History-wal").exists());
        let main_only =
            Connection::open(format!("file:{}?immutable=1", history.display())).unwrap();
        let visits: i64 = main_only
            .query_row("SELECT COUNT(*) FROM visits", [], |r| r.get(0))
            .unwrap();
        assert_eq!(visits, 0);
    }

    #[test]
    fn time_encodings() {
        assert_eq!(webkit(1_792_172_917), 13_436_646_517_000_000);
        assert_eq!(local(21, 18, 37), 1_792_172_917);
        assert!(dotnet_local(0) < 0);
    }
}
