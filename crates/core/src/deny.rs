//! Files that hold secrets. Collectors never open them; the shared open/copy helpers enforce this.

use std::path::{Component, Path};

/// Any path that passes through a component with one of these names.
const SECRET_COMPONENTS: &[&str] = &[
    // chromium family
    "Cookies",
    "Login Data",
    "Login Data For Account",
    "Web Data",
    "Local Extension Settings",
    "Sync Extension Settings",
    "IndexedDB",
    "Local Storage",
    "Session Storage",
    // firefox family
    "cookies.sqlite",
    "logins.json",
    "logins-backup.json",
    "key3.db",
    "key4.db",
    "cert9.db",
    "signons.sqlite",
    // gpg, keyrings, wallets
    "private-keys-v1.d",
    "secring.gpg",
    "keyrings",
    "kwalletd",
    "wallet.dat",
    "wallets",
    "keystore",
    // credentials & cloud secrets
    ".netrc",
    ".npmrc",
    ".pgpass",
    ".my.cnf",
    ".dockercfg",
];

pub fn is_secret(path: &Path) -> bool {
    let names: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let Some(&file) = names.last() else {
        return false;
    };
    let parent = names.len().checked_sub(2).map(|i| names[i]);

    names.iter().any(|n| SECRET_COMPONENTS.contains(n))
        || (parent == Some(".ssh")
            && !file.ends_with(".pub")
            && !file.starts_with("authorized_keys")
            && !file.starts_with("known_hosts")
            && file != "config"
            && file != "environment")
        || (parent == Some(".aws") && file == "credentials")
        || (parent == Some("solana") && file.ends_with(".json"))
        || file == ".env"
        || file.starts_with(".env.")
        || file.ends_with(".pem")
        || file.ends_with(".key")
        || file.ends_with(".p12")
        || file.ends_with(".pfx")
        || file.ends_with(".kdbx")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_secrets() {
        for p in [
            "/h/.config/BraveSoftware/Brave-Browser/Default/Cookies",
            "/h/.config/google-chrome/Profile 1/Login Data",
            "/h/.config/brave/Default/Local Extension Settings/nkbihfbeogaeaoehlefnkodbefgpgknn/000003.log",
            "/h/.mozilla/firefox/x.default/key4.db",
            "/h/.mozilla/firefox/x.default/logins.json",
            "/h/.ssh/id_ed25519",
            "/h/.ssh/custom_deploy_key",
            "/h/.netrc",
            "/h/.npmrc",
            "/h/.aws/credentials",
            "/h/certs/bundle.p12",
            "/h/passwords.kdbx",
            "/h/.gnupg/private-keys-v1.d/ABC.key",
            "/h/.local/share/keyrings/login.keyring",
            "/h/.config/solana/id.json",
            "/h/src/app/.env",
            "/h/src/app/.env.production",
        ] {
            assert!(is_secret(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn allows_evidence() {
        for p in [
            "/h/.config/BraveSoftware/Brave-Browser/Default/History",
            "/h/.config/BraveSoftware/Brave-Browser/Default/Preferences",
            "/h/.config/BraveSoftware/Brave-Browser/Default/Secure Preferences",
            "/h/.ssh/id_ed25519.pub",
            "/h/.ssh/authorized_keys",
            "/h/.ssh/known_hosts",
            "/h/.ssh/config",
            "/h/.xdm-app-data/downloads.db",
            "/var/log/dpkg.log",
            "/h/.bash_history",
            "/h/.environment",
        ] {
            assert!(!is_secret(Path::new(p)), "{p}");
        }
    }
}
