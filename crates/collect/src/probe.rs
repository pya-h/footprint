//! Probing system environment: distro identification, absolute tool resolution,
//! and privilege/group inspection.

use crate::collector::Env;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistroFamily {
    Debian,
    RedHat,
    Arch,
    Suse,
    Unknown,
}

pub struct Probe;

/// The effective and supplementary groups of this process.
fn process_groups() -> Vec<u32> {
    // SAFETY: a zero-sized call only returns the count; the second fills at most `n` entries
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut groups = vec![0; n.max(0) as usize];
    let filled = unsafe { libc::getgroups(n, groups.as_mut_ptr()) };
    groups.truncate(filled.max(0) as usize);
    groups.push(unsafe { libc::getegid() });
    groups
}

impl Probe {
    /// Resolves a tool by absolute path within `env.root` (/usr/bin, /bin, /usr/sbin, /sbin, /usr/local/bin).
    pub fn tool_path(env: &Env, name: &str) -> Option<PathBuf> {
        let candidates = [
            format!("/usr/bin/{name}"),
            format!("/bin/{name}"),
            format!("/usr/sbin/{name}"),
            format!("/sbin/{name}"),
            format!("/usr/local/bin/{name}"),
        ];
        for candidate in &candidates {
            let p = env.system_path(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    pub fn has_tool(env: &Env, name: &str) -> bool {
        Self::tool_path(env, name).is_some()
    }

    /// Identifies distro family from `/etc/os-release` or `/usr/lib/os-release`.
    pub fn distro(env: &Env) -> DistroFamily {
        let paths = ["/etc/os-release", "/usr/lib/os-release"];
        for rel in paths {
            let p = env.system_path(rel);
            let Ok(file) = crate::utils::read_evidence(&p) else {
                continue;
            };
            let text = String::from_utf8_lossy(&file.bytes);
            return Self::parse_os_release(&text);
        }
        DistroFamily::Unknown
    }

    pub fn parse_os_release(text: &str) -> DistroFamily {
        let mut ids = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("ID=") {
                ids.push(val.trim_matches('"').to_lowercase());
            } else if let Some(val) = line.strip_prefix("ID_LIKE=") {
                for part in val.trim_matches('"').split_whitespace() {
                    ids.push(part.to_lowercase());
                }
            }
        }
        for id in &ids {
            match id.as_str() {
                "debian" | "ubuntu" | "linuxmint" | "pop" | "kali" => return DistroFamily::Debian,
                "fedora" | "rhel" | "centos" | "rocky" | "alma" => return DistroFamily::RedHat,
                "arch" | "manjaro" | "endeavouros" => return DistroFamily::Arch,
                "suse" | "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" => {
                    return DistroFamily::Suse;
                }
                _ => {}
            }
        }
        DistroFamily::Unknown
    }

    /// Root, or a process that holds `adm` or `systemd-journal`. The process's own groups
    /// count, not `/etc/group`'s member list: a user added there but not logged in again
    /// still can't read the journal.
    pub fn can_read_system_journal(env: &Env) -> bool {
        if env.euid == 0 || env.uid == 0 {
            return true;
        }
        let wanted = Self::group_ids(env, &["adm", "systemd-journal"]);
        process_groups().iter().any(|g| wanted.contains(g))
    }

    /// The gids `/etc/group` (under `env.root`) gives the named groups.
    pub fn group_ids(env: &Env, names: &[&str]) -> Vec<u32> {
        let Ok(file) = crate::utils::read_evidence(&env.system_path("/etc/group")) else {
            return Vec::new();
        };
        String::from_utf8_lossy(&file.bytes)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split(':');
                let name = parts.next()?;
                let gid = parts.nth(1)?.parse().ok()?;
                names.contains(&name).then_some(gid)
            })
            .collect()
    }

    /// Reads an extended attribute of the path itself (a symlink is not followed).
    /// `None` when absent or unsupported.
    pub fn get_xattr(path: &std::path::Path, name: &str) -> Option<String> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path_c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let name_c = CString::new(name.as_bytes()).ok()?;
        // SAFETY: NUL-terminated strings; a null buffer with size 0 only asks for the length
        let len =
            unsafe { libc::lgetxattr(path_c.as_ptr(), name_c.as_ptr(), std::ptr::null_mut(), 0) };
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`
        let read = unsafe {
            libc::lgetxattr(
                path_c.as_ptr(),
                name_c.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if read <= 0 {
            return None;
        }
        // the value may have shrunk between the two calls
        buf.truncate(read as usize);
        String::from_utf8(buf).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_distro_families() {
        assert_eq!(
            Probe::parse_os_release("ID=ubuntu\nID_LIKE=debian\n"),
            DistroFamily::Debian
        );
        assert_eq!(
            Probe::parse_os_release("NAME=\"Fedora Linux\"\nID=fedora\n"),
            DistroFamily::RedHat
        );
        assert_eq!(
            Probe::parse_os_release("ID=arch\nID_LIKE=\"archlinux\"\n"),
            DistroFamily::Arch
        );
        assert_eq!(
            Probe::parse_os_release("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"suse opensuse\"\n"),
            DistroFamily::Suse
        );
        assert_eq!(
            Probe::parse_os_release("NAME=Custom\n"),
            DistroFamily::Unknown
        );
    }

    #[test]
    fn group_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        fs::create_dir_all(&etc).unwrap();
        fs::write(
            etc.join("group"),
            "root:x:0:\nadm:x:4:syslog,paya\nsystemd-journal:x:101:\nusers:x:100:paya\n",
        )
        .unwrap();

        let env = Env::test(dir.path().join("home"), dir.path().to_path_buf());
        assert_eq!(Probe::group_ids(&env, &["adm"]), [4]);
        assert_eq!(
            Probe::group_ids(&env, &["adm", "systemd-journal"]),
            [4, 101]
        );
        assert!(Probe::group_ids(&env, &["wheel"]).is_empty());
    }
}
