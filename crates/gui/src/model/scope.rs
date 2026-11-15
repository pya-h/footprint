//! The one filter model the GUI has: a [`TimelineFilter`] rendered as chips,
//! with undo/redo history and a CLI rendering for "Copy as CLI".

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use fp_query::{TimelineFilter, Tz};
use fp_store::EventTiming;

pub const HISTORY_CAP: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipRemove {
    SinceUntil,
    Kind(usize),
    KindNot(usize),
    Host(usize),
    HostNot(usize),
    Source(usize),
    SourceNot(usize),
    Confidence(usize),
    Run,
    Timing,
    IocOnly,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub label: String,
    pub remove: ChipRemove,
}

#[derive(Debug, Clone, Default)]
pub struct Scope {
    filter: TimelineFilter,
    undo: Vec<TimelineFilter>,
    redo: Vec<TimelineFilter>,
}

const MICROS: i128 = 1_000_000;

// the CLI grammar parses whole-second naive times in the host zone, so a copied
// window is widened to whole seconds of host-local wall time to round-trip
fn floor_second(ts: i64) -> i64 {
    (i128::from(ts) / MICROS * MICROS).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn ceil_second(ts: i64) -> i64 {
    let t = i128::from(ts);
    let up = t.div_euclid(MICROS) * MICROS + if t.rem_euclid(MICROS) == 0 { 0 } else { MICROS };
    up.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn fmt_us(ts: i64) -> String {
    fp_query::format_time(ts, "exact", Tz::Local, "%Y-%m-%d %H:%M:%S")
}

fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '/' | ':' | '=' | '+' | '@' | '%' | '-')
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn push_list(args: &mut Vec<String>, flag: &str, values: &[String]) {
    if !values.is_empty() {
        args.push(flag.to_string());
        args.push(sh_quote(&values.join(",")));
    }
}

impl Scope {
    /// The filter views query with. Paging (limit/offset/reverse) is not scope:
    /// it lives only on the per-request `EventFilter`.
    pub fn filter(&self) -> TimelineFilter {
        TimelineFilter {
            limit: None,
            offset: None,
            reverse: false,
            ..self.filter.clone()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.filter() == TimelineFilter::default()
    }

    fn edit(&mut self, f: impl FnOnce(&mut TimelineFilter)) {
        let mut next = self.filter.clone();
        f(&mut next);
        if next == self.filter {
            return;
        }
        self.undo.push(self.filter.clone());
        if self.undo.len() > HISTORY_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.filter = next;
    }

    pub fn set_window(&mut self, since: Option<i64>, until: Option<i64>) {
        self.edit(|f| {
            f.since = since;
            f.until = until;
        });
    }

    pub fn set_text(&mut self, text: Option<String>) {
        self.edit(|f| f.match_text = text.filter(|t| !t.is_empty()));
    }

    pub fn set_kinds(&mut self, values: Vec<String>) {
        self.edit(|f| f.kinds = values);
    }

    pub fn set_kinds_not(&mut self, values: Vec<String>) {
        self.edit(|f| f.kinds_not = values);
    }

    pub fn set_kind_filter(&mut self, kinds: Vec<String>, kinds_not: Vec<String>) {
        self.edit(|f| {
            f.kinds = kinds;
            f.kinds_not = kinds_not;
        });
    }

    pub fn set_hosts(&mut self, values: Vec<String>) {
        self.edit(|f| f.hosts = values);
    }

    pub fn set_hosts_not(&mut self, values: Vec<String>) {
        self.edit(|f| f.hosts_not = values);
    }

    pub fn set_host_filter(&mut self, hosts: Vec<String>, hosts_not: Vec<String>) {
        self.edit(|f| {
            f.hosts = hosts;
            f.hosts_not = hosts_not;
        });
    }

    pub fn set_sources(&mut self, values: Vec<String>) {
        self.edit(|f| f.sources = values);
    }

    pub fn set_sources_not(&mut self, values: Vec<String>) {
        self.edit(|f| f.sources_not = values);
    }

    pub fn set_source_filter(&mut self, sources: Vec<String>, sources_not: Vec<String>) {
        self.edit(|f| {
            f.sources = sources;
            f.sources_not = sources_not;
        });
    }

    pub fn set_confidence(&mut self, values: Vec<fp_core::Confidence>) {
        self.edit(|f| f.confidence = values);
    }

    pub fn set_run(&mut self, run: Option<u64>) {
        self.edit(|f| f.run_id = run);
    }

    pub fn set_timing(&mut self, timing: EventTiming) {
        self.edit(|f| f.timing = timing);
    }

    pub fn set_ioc_only(&mut self, on: bool) {
        self.edit(|f| f.ioc_only = on);
    }

    pub fn show_untimed(&mut self) {
        self.edit(|f| {
            f.since = None;
            f.until = None;
            f.timing = EventTiming::Untimed;
        });
    }

    pub fn remove(&mut self, which: ChipRemove) {
        self.edit(|f| match which {
            ChipRemove::SinceUntil => {
                f.since = None;
                f.until = None;
            }
            ChipRemove::Kind(i) => {
                f.kinds.remove(i);
            }
            ChipRemove::KindNot(i) => {
                f.kinds_not.remove(i);
            }
            ChipRemove::Host(i) => {
                f.hosts.remove(i);
            }
            ChipRemove::HostNot(i) => {
                f.hosts_not.remove(i);
            }
            ChipRemove::Source(i) => {
                f.sources.remove(i);
            }
            ChipRemove::SourceNot(i) => {
                f.sources_not.remove(i);
            }
            ChipRemove::Confidence(i) => {
                f.confidence.remove(i);
            }
            ChipRemove::Run => f.run_id = None,
            ChipRemove::Timing => f.timing = EventTiming::Any,
            ChipRemove::IocOnly => f.ioc_only = false,
            ChipRemove::Text => f.match_text = None,
        });
    }

    pub fn clear(&mut self) -> bool {
        if self.is_empty() {
            return false;
        }
        self.edit(|f| *f = TimelineFilter::default());
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.filter.clone());
        self.filter = prev;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.filter.clone());
        self.filter = next;
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn chips(&self) -> Vec<Chip> {
        let f = self.filter();
        let mut chips = Vec::new();
        if f.since.is_some() || f.until.is_some() {
            let start = f.since.map(fmt_us).unwrap_or_default();
            let end = f.until.map(fmt_us).unwrap_or_default();
            chips.push(Chip {
                label: format!("time {start}..{end}"),
                remove: ChipRemove::SinceUntil,
            });
        }
        let mut push = |label: String, remove: ChipRemove| chips.push(Chip { label, remove });
        for (i, k) in f.kinds.iter().enumerate() {
            push(format!("kind {k}"), ChipRemove::Kind(i));
        }
        for (i, k) in f.kinds_not.iter().enumerate() {
            push(format!("kind != {k}"), ChipRemove::KindNot(i));
        }
        for (i, h) in f.hosts.iter().enumerate() {
            push(format!("host {h}"), ChipRemove::Host(i));
        }
        for (i, h) in f.hosts_not.iter().enumerate() {
            push(format!("host != {h}"), ChipRemove::HostNot(i));
        }
        for (i, s) in f.sources.iter().enumerate() {
            push(format!("source {s}"), ChipRemove::Source(i));
        }
        for (i, s) in f.sources_not.iter().enumerate() {
            push(format!("source != {s}"), ChipRemove::SourceNot(i));
        }
        for (i, c) in f.confidence.iter().enumerate() {
            push(format!("confidence {c}"), ChipRemove::Confidence(i));
        }
        if let Some(run) = f.run_id {
            push(format!("run {run}"), ChipRemove::Run);
        }
        match f.timing {
            EventTiming::Timed => push("timed only".to_string(), ChipRemove::Timing),
            EventTiming::Untimed => push("untimed only".to_string(), ChipRemove::Timing),
            EventTiming::Any => {}
        }
        if f.ioc_only {
            push("IOC hits only".to_string(), ChipRemove::IocOnly);
        }
        if let Some(text) = &f.match_text {
            push(format!("text \"{text}\""), ChipRemove::Text);
        }
        chips
    }

    /// The equivalent `footprint list` invocation; every scope part maps to a CLI flag.
    pub fn to_cli(&self) -> String {
        format!("footprint {}", self.cli_args().join(" "))
    }

    /// The argument vector behind [`to_cli`](Self::to_cli), one flag/value pair per
    /// entry, in the CLI's flag order — kept separate so tests can pin the parity.
    pub fn cli_args(&self) -> Vec<String> {
        let f = self.filter();
        let mut args: Vec<String> = vec!["list".to_string()];
        if let Some(t) = f.since {
            args.push("--since".to_string());
            args.push(sh_quote(&fmt_us(floor_second(t))));
        }
        if let Some(t) = f.until {
            args.push("--until".to_string());
            args.push(sh_quote(&fmt_us(ceil_second(t))));
        }
        push_list(&mut args, "--kind", &f.kinds);
        push_list(&mut args, "--source", &f.sources);
        push_list(&mut args, "--host", &f.hosts);
        if let Some(text) = &f.match_text {
            args.push("--match".to_string());
            args.push(sh_quote(text));
        }
        push_list(&mut args, "--exclude-kind", &f.kinds_not);
        push_list(&mut args, "--exclude-host", &f.hosts_not);
        push_list(&mut args, "--exclude-source", &f.sources_not);
        if let Some(run) = f.run_id {
            args.push("--run".to_string());
            args.push(run.to_string());
        }
        if !f.confidence.is_empty() {
            args.push("--confidence".to_string());
            let list = f
                .confidence
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            args.push(sh_quote(&list));
        }
        match f.timing {
            EventTiming::Timed => args.push("--timed".to_string()),
            EventTiming::Untimed => args.push("--untimed".to_string()),
            EventTiming::Any => {}
        }
        if f.ioc_only {
            args.push("--ioc-only".to_string());
        }
        args
    }

    /// Cache key for request/row-block caching; paging fields never reach it.
    pub fn hash(&self) -> u64 {
        let f = self.filter();
        let mut h = DefaultHasher::new();
        f.since.hash(&mut h);
        f.until.hash(&mut h);
        f.kinds.hash(&mut h);
        f.kinds_not.hash(&mut h);
        f.hosts.hash(&mut h);
        f.hosts_not.hash(&mut h);
        f.sources.hash(&mut h);
        f.sources_not.hash(&mut h);
        for c in &f.confidence {
            (*c as u8).hash(&mut h);
        }
        f.run_id.hash(&mut h);
        (f.timing as u8).hash(&mut h);
        f.ioc_only.hash(&mut h);
        f.match_text.hash(&mut h);
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(f: impl FnOnce(&mut TimelineFilter)) -> Scope {
        let mut s = Scope::default();
        s.edit(f);
        s
    }

    #[test]
    fn removing_every_chip_empties_the_scope() {
        let mut s = scope(|f| {
            f.since = Some(1_700_000_000_000_000);
            f.until = Some(1_700_000_100_000_000);
            f.kinds = vec!["web.*".into(), "dl.*".into()];
            f.kinds_not = vec!["file.*".into()];
            f.hosts = vec!["zoom.us".into()];
            f.hosts_not = vec!["google.com".into()];
            f.sources = vec!["brave/Default".into()];
            f.sources_not = vec!["journal".into()];
            f.confidence = vec![fp_core::Confidence::Derived];
            f.run_id = Some(3);
            f.timing = EventTiming::Untimed;
            f.ioc_only = true;
            f.match_text = Some("zoom".into());
        });
        assert_eq!(s.chips().len(), 13);
        while !s.is_empty() {
            let remove = s
                .chips()
                .first()
                .expect("chips while not empty")
                .remove
                .clone();
            s.remove(remove);
        }
    }

    #[test]
    fn undo_redo_walks_history_and_new_edit_clears_redo() {
        let mut s = Scope::default();
        s.set_text(Some("one".into()));
        s.set_text(Some("two".into()));
        assert!(s.undo());
        assert_eq!(s.filter().match_text.as_deref(), Some("one"));
        assert!(s.undo());
        assert!(s.is_empty());
        assert!(!s.undo());
        assert!(s.redo());
        assert!(s.redo());
        assert_eq!(s.filter().match_text.as_deref(), Some("two"));
        s.set_text(Some("three".into()));
        assert!(!s.can_redo());
    }

    #[test]
    fn history_is_capped_at_50() {
        let mut s = Scope::default();
        for i in 0..60 {
            s.set_text(Some(format!("n{i}")));
        }
        let mut undos = 0;
        while s.undo() {
            undos += 1;
        }
        assert_eq!(undos, HISTORY_CAP);
    }

    #[test]
    fn noop_edits_do_not_consume_history() {
        let mut s = Scope::default();
        s.set_text(Some("x".into()));
        s.set_text(Some("x".into()));
        assert!(s.undo());
        assert!(!s.undo());
    }

    #[test]
    fn to_cli_covers_every_part_and_round_trips_times() {
        let since = 1_761_234_500_700_000;
        let until = 1_761_234_900_200_000;
        let s = scope(|f| {
            f.since = Some(since);
            f.until = Some(until);
            f.kinds = vec!["web.*".into(), "dl.*".into()];
            f.hosts_not = vec!["google.com".into()];
            f.match_text = Some("zoom".into());
            f.confidence = vec![fp_core::Confidence::Direct, fp_core::Confidence::Derived];
            f.timing = EventTiming::Timed;
            f.ioc_only = true;
        });
        let cli = s.to_cli();
        assert!(cli.starts_with("footprint list "));
        for part in [
            "--since '",
            "--until '",
            "--kind 'web.*,dl.*'",
            "--exclude-host google.com",
            "--match zoom",
            "--confidence 'direct,derived'",
            "--timed",
            "--ioc-only",
        ] {
            assert!(cli.contains(part), "{cli} lacks {part}");
        }
        let since_str = cli
            .split("--since '")
            .nth(1)
            .expect("since")
            .split('\'')
            .next()
            .expect("value");
        let until_str = cli
            .split("--until '")
            .nth(1)
            .expect("until")
            .split('\'')
            .next()
            .expect("value");
        assert_eq!(
            fp_query::time::parse_in(since_str, Some(Tz::Local)).expect("since parses"),
            floor_second(since)
        );
        assert_eq!(
            fp_query::time::parse_in(until_str, Some(Tz::Local)).expect("until parses"),
            ceil_second(until)
        );
    }

    #[test]
    fn cli_args_pin_flag_parity_with_the_list_command() {
        let s = scope(|f| {
            f.since = Some(1_700_000_000_000_000);
            f.until = Some(1_700_000_100_000_000);
            f.kinds = vec!["web.*".into(), "dl.*".into()];
            f.sources = vec!["brave/Default".into()];
            f.hosts = vec!["zoom.us".into()];
            f.match_text = Some("zoom".into());
            f.kinds_not = vec!["file.*".into()];
            f.hosts_not = vec!["google.com".into()];
            f.sources_not = vec!["journald".into()];
            f.run_id = Some(3);
            f.confidence = vec![fp_core::Confidence::Direct, fp_core::Confidence::Derived];
            f.timing = EventTiming::Timed;
            f.ioc_only = true;
        });
        let args = s.cli_args();
        assert_eq!(args[0], "list");
        // flag order and names pinned against the CLI's scope flags; time values
        // are local-zone formatted, so only their position is checked here
        let flags: Vec<&str> = args.iter().filter_map(|a| a.strip_prefix("--")).collect();
        assert_eq!(
            flags,
            [
                "since",
                "until",
                "kind",
                "source",
                "host",
                "match",
                "exclude-kind",
                "exclude-host",
                "exclude-source",
                "run",
                "confidence",
                "timed",
                "ioc-only"
            ]
        );
        assert_eq!(
            args[args.iter().position(|a| a == "--kind").unwrap() + 1],
            "'web.*,dl.*'"
        );
        assert_eq!(
            args[args.iter().position(|a| a == "--match").unwrap() + 1],
            "zoom"
        );
        assert_eq!(
            args[args.iter().position(|a| a == "--confidence").unwrap() + 1],
            "'direct,derived'"
        );
        assert_eq!(
            args[args.iter().position(|a| a == "--run").unwrap() + 1],
            "3"
        );
    }

    #[test]
    fn to_cli_quotes_hostile_text() {
        let s = scope(|f| f.match_text = Some("a b;$HOME".into()));
        let cli = s.to_cli();
        assert!(cli.contains("--match 'a b;$HOME'"), "{cli}");
    }

    #[test]
    fn hash_tracks_scope_not_paging() {
        let mut a = scope(|f| f.kinds = vec!["web.*".into()]);
        let mut b = a.clone();
        b.edit(|f| {
            f.limit = Some(10);
            f.offset = Some(20);
            f.reverse = true;
        });
        assert_eq!(a.hash(), b.hash());
        a.set_text(Some("x".into()));
        assert_ne!(a.hash(), b.hash());
    }
}
