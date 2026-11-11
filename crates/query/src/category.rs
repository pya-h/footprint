#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum Category {
    Web,
    Downloads,
    Packages,
    Access,
    Shell,
    Files,
    Persistence,
    Extensions,
    Other,
}

impl Category {
    pub const ALL: [Self; 9] = [
        Self::Web,
        Self::Downloads,
        Self::Packages,
        Self::Access,
        Self::Shell,
        Self::Files,
        Self::Persistence,
        Self::Extensions,
        Self::Other,
    ];

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Web => "Web",
            Self::Downloads => "Downloads",
            Self::Packages => "Packages",
            Self::Access => "Access",
            Self::Shell => "Shell",
            Self::Files => "Files",
            Self::Persistence => "Persistence",
            Self::Extensions => "Extensions",
            Self::Other => "Other",
        }
    }
}

pub fn category(kind: &str) -> Category {
    if kind == "web.download" || kind.starts_with("dl.") {
        Category::Downloads
    } else if kind.starts_with("web.") {
        Category::Web
    } else if kind.starts_with("pkg.") {
        Category::Packages
    } else if kind.starts_with("priv.") || kind.starts_with("auth.") {
        Category::Access
    } else if kind.starts_with("shell.") {
        Category::Shell
    } else if kind.starts_with("file.") {
        Category::Files
    } else if kind.starts_with("persist.") {
        Category::Persistence
    } else if kind.starts_with("ext.") {
        Category::Extensions
    } else {
        Category::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{Category, category};
    use fp_core::EventKind;

    #[test]
    fn categorizes_all_known_kinds_and_unknown_values() {
        let groups: &[(Category, &[EventKind])] = &[
            (
                Category::Web,
                &[
                    EventKind::WebVisit,
                    EventKind::WebRedirect,
                    EventKind::WebPermissionChange,
                    EventKind::WebPermissionUsed,
                    EventKind::WebSearch,
                ],
            ),
            (
                Category::Downloads,
                &[EventKind::WebDownload, EventKind::DlManagerDownload],
            ),
            (
                Category::Packages,
                &[
                    EventKind::PkgInstall,
                    EventKind::PkgUpgrade,
                    EventKind::PkgRemove,
                ],
            ),
            (
                Category::Access,
                &[
                    EventKind::PrivSudo,
                    EventKind::PrivSu,
                    EventKind::AuthLogin,
                    EventKind::AuthSsh,
                    EventKind::AuthSession,
                ],
            ),
            (Category::Shell, &[EventKind::ShellCommand]),
            (
                Category::Files,
                &[
                    EventKind::FileOpened,
                    EventKind::FileCreated,
                    EventKind::FileModified,
                ],
            ),
            (
                Category::Persistence,
                &[EventKind::PersistUnitAdded, EventKind::PersistAutostart],
            ),
            (Category::Extensions, &[EventKind::ExtInstalled]),
            (
                Category::Other,
                &[EventKind::NetListen, EventKind::ProcExec, EventKind::Other],
            ),
        ];
        assert_eq!(groups.len(), Category::all().len());
        for (expected, kinds) in groups {
            assert!(!expected.label().is_empty());
            for kind in *kinds {
                assert_eq!(category(kind.as_str()), *expected, "{kind:?}");
            }
        }
        assert_eq!(category("web.download.extra"), Category::Web);
        assert_eq!(category("web"), Category::Other);
        assert_eq!(category("WEB.visit"), Category::Other);
        assert_eq!(category("unknown.kind"), Category::Other);
        assert_eq!(category(""), Category::Other);
    }
}
