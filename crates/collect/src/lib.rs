pub mod auth;
pub mod auth_fallbacks;
pub mod chromium;
pub mod collector;
pub mod desktop;
pub mod desktop_fallbacks;
pub mod downloads;
pub mod event;
pub mod firefox;
pub mod fs;
pub mod persist;
pub mod pkg;
pub mod pkg_fallbacks;
pub mod probe;
pub mod redact;
pub mod shell;
pub mod utils;
pub mod xdm;

pub use collector::{CollectCtx, Collector, Env, Sink};
pub use probe::{DistroFamily, Probe};

pub fn registry() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(chromium::ChromiumCollector),
        Box::new(firefox::FirefoxCollector),
        Box::new(xdm::XdmCollector),
        Box::new(downloads::XattrDownloadCollector),
        Box::new(downloads::DlmgrDetectCollector),
        Box::new(pkg::DpkgCollector),
        Box::new(pkg::AptCollector),
        Box::new(pkg_fallbacks::PacmanCollector),
        Box::new(pkg_fallbacks::DnfCollector),
        Box::new(pkg_fallbacks::ZypperCollector),
        Box::new(pkg_fallbacks::SnapCollector),
        Box::new(pkg_fallbacks::FlatpakCollector),
        Box::new(auth::AuthCollector),
        Box::new(auth_fallbacks::AuthLogCollector),
        Box::new(auth_fallbacks::WtmpCollector),
        Box::new(shell::ShellCollector),
        Box::new(desktop::DesktopCollector),
        Box::new(desktop_fallbacks::KdeRecentCollector),
        Box::new(desktop_fallbacks::ZeitgeistCollector),
        Box::new(fs::FilesystemCollector),
        Box::new(persist::PersistCollector),
    ]
}
