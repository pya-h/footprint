pub mod badge;
pub mod chip;
pub mod event_table;
pub mod facets;
pub mod inspector;
pub mod json_tree;
pub mod strip;

pub struct ViewCtx<'a> {
    pub model: &'a crate::model::Model,
    pub theme: &'a crate::theme::Theme,
    pub tz: fp_query::Tz,
}
