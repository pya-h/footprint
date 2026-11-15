//! Drives the model the way widgets will: actions in, observable state out —
//! scope edits, chips, undo/redo, generation-based stale drops, the untimed
//! notice rule and navigation history.

use fp_gui::model::{Action, Counts, Model, ScopeAction, Selection, View};
use fp_gui::worker::Generation;

fn counts(generation: Generation, in_scope: u64, in_case: u64) -> Action {
    Action::Counts {
        generation,
        result: Ok(Counts { in_scope, in_case }),
    }
}

#[test]
fn scope_edit_bumps_generation_and_late_responses_drop() {
    let mut model = Model::new();
    model.apply(counts(0, 100, 500));
    assert_eq!(model.counts.value().map(|c| c.in_scope), Some(100));

    model.apply(Action::Scope(ScopeAction::SetText(Some("zoom".into()))));
    assert_eq!(model.generation, 1);
    // the old result stays visible but reads stale until replaced
    assert_eq!(
        model.counts.status(model.generation),
        fp_gui::model::LoadStatus::Stale
    );
    // a response for the pre-edit generation arrives late: dropped
    model.apply(counts(0, 999, 999));
    assert_eq!(model.counts.value().map(|c| c.in_scope), Some(100));
    // the current generation lands
    model.apply(counts(1, 3, 500));
    assert_eq!(model.counts.value().map(|c| c.in_scope), Some(3));
    assert_eq!(
        model.counts.status(model.generation),
        fp_gui::model::LoadStatus::Ready
    );
}

#[test]
fn chip_removal_round_trips_through_actions() {
    let mut model = Model::new();
    model.apply(Action::Scope(ScopeAction::SetWindow {
        since: Some(1_700_000_000_000_000),
        until: Some(1_700_000_100_000_000),
    }));
    model.apply(Action::Scope(ScopeAction::SetText(Some("zoom".into()))));
    let labels: Vec<String> = model
        .scope
        .chips()
        .iter()
        .map(|c| c.label.clone())
        .collect();
    assert_eq!(labels.len(), 2);
    assert!(labels[0].starts_with("time "));
    assert_eq!(labels[1], "text \"zoom\"");

    while !model.scope.is_empty() {
        let remove = model.scope.chips()[0].remove.clone();
        model.apply(Action::Scope(ScopeAction::Remove(remove)));
    }
    assert_eq!(model.generation, 4); // two edits + two removals
    assert!(model.scope.is_empty());
}

#[test]
fn undo_and_redo_scope_through_actions() {
    let mut model = Model::new();
    let gen0 = model.generation;
    model.apply(Action::Scope(ScopeAction::SetText(Some("a".into()))));
    model.apply(Action::Scope(ScopeAction::SetText(Some("b".into()))));
    model.apply(Action::UndoScope);
    assert_eq!(model.scope.filter().match_text.as_deref(), Some("a"));
    model.apply(Action::RedoScope);
    assert_eq!(model.scope.filter().match_text.as_deref(), Some("b"));
    assert!(model.generation > gen0);
}

#[test]
fn clear_scope_is_a_single_undoable_action() {
    let mut model = Model::new();
    model.apply(Action::Scope(ScopeAction::SetText(Some("a".into()))));
    model.apply(Action::Scope(ScopeAction::SetWindow {
        since: Some(1),
        until: None,
    }));
    model.apply(Action::Scope(ScopeAction::Clear));
    assert!(model.scope.is_empty());
    model.apply(Action::UndoScope);
    assert!(!model.scope.is_empty());
}

#[test]
fn untimed_notice_only_with_an_active_window_and_untimed_events() {
    let mut model = Model::new();
    // no window: no notice even when untimed events exist in scope
    model.apply(Action::Untimed {
        generation: 0,
        result: Ok(214),
    });
    assert_eq!(model.untimed_hidden(), None);

    // window + untimed events: notice with the count
    model.apply(Action::Scope(ScopeAction::SetWindow {
        since: Some(1_700_000_000_000_000),
        until: None,
    }));
    model.apply(Action::Untimed {
        generation: 1,
        result: Ok(214),
    });
    assert_eq!(model.untimed_hidden(), Some(214));

    // zero untimed events: no notice
    model.apply(Action::Untimed {
        generation: 1,
        result: Ok(0),
    });
    assert_eq!(model.untimed_hidden(), None);

    // dropping the window removes the notice
    model.apply(Action::Scope(ScopeAction::Remove(
        fp_gui::model::ChipRemove::SinceUntil,
    )));
    assert_eq!(model.untimed_hidden(), None);
}

#[test]
fn navigation_and_selection_share_one_history() {
    let mut model = Model::new();
    assert_eq!(model.view(), View::Dashboard);
    model.apply(Action::Go(View::Timeline));
    model.apply(Action::Select(Selection::Event(42)));
    assert_eq!(model.selection(), Selection::Event(42));

    model.apply(Action::Go(View::Hosts));
    assert_eq!(model.view(), View::Hosts);
    model.apply(Action::NavBack);
    assert_eq!(model.view(), View::Timeline);
    assert_eq!(model.selection(), Selection::Event(42));
    model.apply(Action::NavBack);
    assert_eq!(model.view(), View::Dashboard);
    model.apply(Action::NavForward);
    model.apply(Action::NavForward);
    assert_eq!(model.view(), View::Hosts);
    assert!(!model.nav.can_forward());
}

#[test]
fn copy_as_cli_reproduces_the_scope() {
    let mut model = Model::new();
    model.apply(Action::Scope(ScopeAction::SetWindow {
        since: Some(1_761_234_500_000_000),
        until: Some(1_761_235_200_000_000),
    }));
    model.apply(Action::Scope(ScopeAction::SetText(Some("zoom".into()))));
    let cli = model.scope.to_cli();
    assert!(cli.starts_with("footprint list "));
    assert!(cli.contains("--since '"));
    assert!(cli.contains("--until '"));
    assert!(cli.contains("--match zoom"));

    // removing the text chip removes its flag from the copy
    let remove = model.scope.chips()[1].remove.clone();
    model.apply(Action::Scope(ScopeAction::Remove(remove)));
    assert!(!model.scope.to_cli().contains("--match"));
}

#[test]
fn failed_queries_show_as_errors_without_touching_the_generation() {
    let mut model = Model::new();
    model.apply(Action::Counts {
        generation: 0,
        result: Err("database is locked".to_string()),
    });
    assert_eq!(
        model.counts.status(0),
        fp_gui::model::LoadStatus::Error("database is locked".to_string())
    );
}
