//! The known-false-positive table for scanner findings.
//! Matched case-insensitively against the finding text; `proof` is the command that
//! demonstrates the finding is harmless.

pub struct Explanation {
    pub id: &'static str,
    /// matched as a case-insensitive substring of the finding
    pub pattern: &'static str,
    pub title: &'static str,
    pub why: &'static str,
    /// a read-only command that proves it; safe to run, never executes the finding
    pub proof: &'static str,
}

pub const EXPLANATIONS: &[Explanation] = &[
    Explanation {
        id: "lwp-request",
        pattern: "lwp-request",
        title: "rkhunter: lwp-request is a script",
        why: "lwp-request ships with libwww-perl and is a legitimate Perl script; rkhunter flags \
              any script in a binary's place. Seen on stock Debian/Ubuntu.",
        proof: "dpkg -S /usr/bin/lwp-request",
    },
    Explanation {
        id: "hash-changed-after-upgrade",
        pattern: "hash changed",
        title: "rkhunter: curl/ssh hash changed",
        why: "rkhunter records baselines at install; a package upgrade changes the binaries. \
              Cross-check with the package log before treating it as tampering.",
        proof: "zgrep -hE ' upgrade (curl|openssh|libcurl)' /var/log/dpkg.log*",
    },
    Explanation {
        id: "rust-coreutils-infected",
        pattern: "infected",
        title: "chkrootkit: basename/date/dirname/echo/env INFECTED",
        why: "Ubuntu 26.04 ships rust-coreutils as /usr/bin/basename and friends; chkrootkit's \
              signature test misfires on them. dpkg --verify on the owning package matches.",
        proof: "dpkg -S \"$(command -v basename)\"; dpkg --verify coreutils",
    },
    Explanation {
        id: "sing-box-port",
        pattern: "47018",
        title: "rkhunter: sing-box port 47018 looks like a URK",
        why: "sing-box (and v2ray) listen on high ports for the Tun inbound; rkhunter has no \
              signature for them and guesses 'URK'.",
        proof: "dpkg -l sing-box; ss -tlnp | grep 47018",
    },
    Explanation {
        id: "postgres-shm",
        pattern: "/dev/shm",
        title: "PostgreSQL files in /dev/shm",
        why: "PostgreSQL keeps shared buffers in POSIX shared memory under /dev/shm \
              (postmaster.pid, PGSQL.<pid>). Present whenever postgres runs.",
        proof: "ls /dev/shm | grep -E '^(PGSQL|postmaster)'; dpkg -l 'postgresql*'",
    },
];

pub fn explain(finding: &str) -> Vec<&'static Explanation> {
    let lower = finding.to_lowercase();
    EXPLANATIONS
        .iter()
        .filter(|e| lower.contains(&e.pattern.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_findings() {
        assert_eq!(
            explain("Warning: lwp-request is a script")[0].id,
            "lwp-request"
        );
        assert_eq!(
            explain("The file hash changed for /usr/bin/ssh")[0].id,
            "hash-changed-after-upgrade"
        );
        assert_eq!(
            explain("/usr/bin/basename: INFECTED")[0].id,
            "rust-coreutils-infected"
        );
        assert_eq!(
            explain("Port 47018: is it bound to a known service? URK?")[0].id,
            "sing-box-port"
        );
        assert_eq!(
            explain("Suspicious file /dev/shm/PGSQL.1234")[0].id,
            "postgres-shm"
        );
    }

    #[test]
    fn unknown_findings_have_no_explanation() {
        assert!(explain("Trojan.Win32.Evil in /home/u/x").is_empty());
        assert!(explain("").is_empty());
    }
}
