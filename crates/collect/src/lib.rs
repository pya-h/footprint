pub mod auth;
pub mod chromium;
pub mod collector;
pub mod desktop;
pub mod event;
pub mod pkg;
pub mod redact;
pub mod shell;
pub mod utils;
pub mod xdm;

pub use collector::{CollectCtx, Collector, Env, Sink};

pub fn registry() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(chromium::ChromiumCollector),
        Box::new(xdm::XdmCollector),
        Box::new(pkg::DpkgCollector),
        Box::new(pkg::AptCollector),
        Box::new(auth::AuthCollector),
        Box::new(shell::ShellCollector),
        Box::new(desktop::DesktopCollector),
    ]
}
