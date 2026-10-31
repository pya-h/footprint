use crate::collector::CollectCtx;
use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::fs::{self, DirBuilder, File};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Text evidence (logs, histories, prefs) above this size is refused rather than read into memory.
const MAX_TEXT_BYTES: u64 = 256 << 20;
/// SQLite evidence above this size is refused; real browser histories stay far below it.
const MAX_DB_BYTES: u64 = 4 << 30;

pub fn in_window(ctx: &CollectCtx, ts: i64) -> bool {
    ctx.since.is_none_or(|lo| ts >= lo) && ctx.until.is_none_or(|hi| ts <= hi)
}

/// The one gate every evidence read goes through.
pub fn guard(path: &Path) -> Result<()> {
    if fp_core::deny::is_secret(path) {
        bail!("refusing to open a secret file: {}", path.display());
    }
    Ok(())
}

/// Evidence is a regular file; a FIFO planted as `.bash_history` would block the read forever
/// and a device would never end.
fn regular_file(path: &Path) -> Result<fs::Metadata> {
    let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(meta)
}

/// Opens evidence for reading only if it is a regular file. The open itself never blocks (a
/// FIFO opened for reading waits for a writer otherwise) and the type is checked on the open
/// descriptor, so a swap between check and open can't slip a FIFO or a device through.
pub fn open_regular(path: &Path) -> Result<File> {
    guard(path)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(file)
}

/// A private staging subdir per source instance, so side files of two profiles never mix.
pub fn stage_dir(ctx: &CollectCtx, inst_id: u64) -> Result<PathBuf> {
    let dir = ctx.temp_dir.join(format!("src-{inst_id}"));
    if !dir.is_dir() {
        DirBuilder::new().mode(0o700).create(&dir)?;
    }
    Ok(dir)
}

pub struct Snapshot {
    pub copy: PathBuf,
    pub sha256: String,
}

/// Copies a live SQLite DB with its `-wal`/`-journal` (named `<full name>-wal`) into `stage`.
pub fn copy_db_set(src: &Path, stage: &Path) -> Result<Snapshot> {
    guard(src)?;
    let meta = regular_file(src)?;
    if meta.len() > MAX_DB_BYTES {
        bail!(
            "{} is {} bytes, over the {MAX_DB_BYTES}-byte limit",
            src.display(),
            meta.len()
        );
    }
    let name = src.file_name().context("source has no file name")?;
    let copy = stage.join(name);
    fs::copy(src, &copy).with_context(|| format!("copy {}", src.display()))?;
    for suffix in ["-wal", "-journal"] {
        let mut side_name = name.to_os_string();
        side_name.push(suffix);
        let side = src.with_file_name(&side_name);
        if side.exists() {
            guard(&side)?;
            regular_file(&side)?;
            fs::copy(&side, stage.join(&side_name))
                .with_context(|| format!("copy {}", side.display()))?;
        }
    }
    let sha256 = sha256_file(&copy)?;
    Ok(Snapshot { copy, sha256 })
}

pub fn open_snapshot(snapshot: &Snapshot) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        &snapshot.copy,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

pub fn table_has_columns(conn: &Connection, table: &str, columns: &[&str]) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let present: Vec<String> = stmt
        .query_map([table], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(columns.iter().all(|c| present.iter().any(|p| p == c)))
}

pub struct TextFile {
    pub bytes: Vec<u8>,
    /// hash of the file as stored (compressed for `.gz`)
    pub sha256: String,
}

/// Reads a small evidence file whole, transparently gunzipping `*.gz`, with a size cap on both sides.
pub fn read_evidence(path: &Path) -> Result<TextFile> {
    let raw = read_capped(open_regular(path)?).with_context(|| path.display().to_string())?;
    let sha256 = hex::encode(Sha256::digest(&raw));
    let bytes = if path.extension().is_some_and(|e| e == "gz") {
        read_capped(GzDecoder::new(raw.as_slice()))
            .with_context(|| format!("gunzip {}", path.display()))?
    } else {
        raw
    };
    Ok(TextFile { bytes, sha256 })
}

pub fn read_capped(reader: impl Read) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    reader.take(MAX_TEXT_BYTES + 1).read_to_end(&mut out)?;
    if out.len() as u64 > MAX_TEXT_BYTES {
        bail!("over the {MAX_TEXT_BYTES}-byte limit");
    }
    Ok(out)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut open_regular(path)?, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// `base`, then `base.1`, `base.2.gz`, … oldest last; only the files that exist.
pub fn with_rotations(base: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (base.parent(), base.file_name().and_then(|n| n.to_str())) else {
        return Vec::new();
    };
    let mut rotated: Vec<(u32, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name();
            let suffix = file.to_str()?.strip_prefix(name)?.strip_prefix('.')?;
            let n = suffix.strip_suffix(".gz").unwrap_or(suffix).parse().ok()?;
            Some((n, entry.path()))
        })
        .collect();
    rotated.sort();
    base.exists()
        .then(|| base.to_path_buf())
        .into_iter()
        .chain(rotated.into_iter().map(|(_, p)| p))
        .collect()
}

pub use fp_core::net::extract_host;

pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes.max(0) as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// `file:///home/u/My%20File.pdf` → `/home/u/My File.pdf`; anything else is returned decoded as-is.
pub fn file_uri_to_path(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    percent_decode(path)
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Installs an executable `#!/bin/sh` script for tests that spawn a fake tool.
#[cfg(test)]
pub(crate) fn script_tool(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let staged = dir.join(format!(".{name}.staged"));
    fs::write(&staged, format!("#!/bin/sh\n{body}\n")).expect("write script");
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).expect("chmod script");
    // copied by a child process: a sibling test forking while this process held the write fd
    // would keep it open in its child and exec would fail with ETXTBSY
    let path = dir.join(name);
    let copied = std::process::Command::new("cp")
        .arg("-p")
        .arg(&staged)
        .arg(&path)
        .status()
        .expect("spawn cp");
    assert!(copied.success(), "cp {}", path.display());
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_from_urls_and_permission_patterns() {
        assert_eq!(
            extract_host("https://us09-web-zoom.com/j/123").as_deref(),
            Some("us09-web-zoom.com")
        );
        assert_eq!(
            extract_host("https://User@Zoom.US:443/x?a=b").as_deref(),
            Some("zoom.us")
        );
        assert_eq!(
            extract_host("https://me.simuul.xyz:443,*").as_deref(),
            Some("me.simuul.xyz")
        );
        assert_eq!(extract_host("http://[::1]:8080/").as_deref(), Some("::1"));
        assert_eq!(extract_host("about:blank"), None);
        assert_eq!(extract_host("file:///home/u/x"), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(317_514_752), "302.8 MiB");
        assert_eq!(human_size(-1), "-1 B");
    }

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            file_uri_to_path("file:///home/u/My%20CV%E2%9C%93.pdf"),
            "/home/u/My CV✓.pdf"
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }

    #[test]
    fn side_files_keep_the_full_name() {
        let src = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let db = src.path().join("downloads.db");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t (x);")
            .unwrap();
        fs::write(src.path().join("downloads.db-wal"), b"").unwrap();
        copy_db_set(&db, stage.path()).unwrap();
        assert!(stage.path().join("downloads.db-wal").exists());
        assert!(!stage.path().join("downloads-wal").exists());
    }

    #[test]
    fn copy_refuses_secrets() {
        let src = tempfile::tempdir().unwrap();
        let cookies = src.path().join("Cookies");
        fs::write(&cookies, b"secret").unwrap();
        let stage = tempfile::tempdir().unwrap();
        assert!(copy_db_set(&cookies, stage.path()).is_err());
        assert!(read_evidence(&cookies).is_err());
        assert_eq!(fs::read_dir(stage.path()).unwrap().count(), 0);
    }

    #[test]
    fn reads_gz_and_finds_rotations() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("dpkg.log");
        fs::write(&base, "new\n").unwrap();
        fs::write(dir.path().join("dpkg.log.1"), "older\n").unwrap();
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(b"oldest\n").unwrap();
        fs::write(dir.path().join("dpkg.log.2.gz"), gz.finish().unwrap()).unwrap();
        fs::write(dir.path().join("dpkg.log.bak"), "ignored").unwrap();

        let files = with_rotations(&base);
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["dpkg.log", "dpkg.log.1", "dpkg.log.2.gz"]);
        assert_eq!(read_evidence(&files[2]).unwrap().bytes, b"oldest\n");
    }

    #[test]
    fn a_fifo_is_refused_instead_of_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join(".bash_history");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let Err(err) = read_evidence(&fifo) else {
            panic!("a fifo must be refused");
        };
        assert!(format!("{err:#}").contains("not a regular file"), "{err:#}");
        assert!(copy_db_set(&fifo, dir.path()).is_err());
    }

    #[test]
    fn streaming_hash_matches() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
