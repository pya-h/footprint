//! Timeline view: overview strip + virtualized event table / group-by pivot.

use eframe::egui;

use crate::model::Action;
use crate::settings::Settings;
use crate::widgets::ViewCtx;
use crate::widgets::event_table::{TableState, show_timeline_table};
use crate::widgets::strip::{StripState, show_strip};
use crate::worker::BlockCache;

pub fn show_timeline(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    cache: &mut BlockCache,
    settings: &mut Settings,
    strip: &mut StripState,
    table: &mut TableState,
    actions: &mut Vec<Action>,
) {
    show_strip(ui, ctx.model, ctx.theme, ctx.tz, strip, actions);
    ui.add_space(6.0);
    show_timeline_table(ui, ctx, cache, settings, table, actions);
}
