//! Probing system environment: distro identification, absolute tool resolution,
//! and privilege/group inspection.

use crate::collector::Env;
use std::fs;
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
            let Ok(text) = fs::read_to_string(&p) else {
                continue;
            };
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

    /// Checks if the invoking or effective user can read system journal entries.
    /// Returns true if running as root (euid == 0) or user is a member of `adm` or `systemd-journal`.
    pub fn can_read_system_journal(env: &Env) -> bool {
        if env.euid == 0 || env.uid == 0 {
            return true;
        }
        let user = std::env::var("USER").unwrap_or_default();
        Self::user_in_any_group(env, &user, env.uid, &["adm", "systemd-journal"])
    }

    /// Checks `/etc/group` within `env.root` to see if `user` or `uid` belongs to any of `target_groups`.
    pub fn user_in_any_group(env: &Env, user: &str, uid: u32, target_groups: &[&str]) -> bool {
        let group_path = env.system_path("/etc/group");
        let Ok(contents) = fs::read_to_string(group_path) else {
            return false;
        };
        for line in contents.lines() {
            let mut parts = line.split(':');
            let (Some(grp_name), _, Some(gid_str), Some(members)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if !target_groups.contains(&grp_name) {
                continue;
            }
            if gid_str.parse::<u32>().is_ok_and(|g| g == uid) {
                return true;
            }
            if !user.is_empty() && members.split(',').any(|m| m.trim() == user) {
                return true;
            }
        }
        false
    }

    /// Reads an extended attribute from a file on Linux. Returns None if not supported or not present.
    pub fn get_xattr(path: &std::path::Path, name: &str) -> Option<String> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path_c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let name_c = CString::new(name.as_bytes()).ok()?;
        let len =
            unsafe { libc::getxattr(path_c.as_ptr(), name_c.as_ptr(), std::ptr::null_mut(), 0) };
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let read = unsafe {
            libc::getxattr(
                path_c.as_ptr(),
                name_c.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if read <= 0 {
            return None;
        }
        String::from_utf8(buf).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let mut env = Env::current();
        env.root = dir.path().to_path_buf();
        assert!(Probe::user_in_any_group(&env, "paya", 1000, &["adm"]));
        assert!(!Probe::user_in_any_group(&env, "other", 1001, &["adm"]));
    }
}
