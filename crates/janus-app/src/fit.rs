//! Sizes that follow the Font size setting (10 to 28 px): a text's width, a combo box's for its choices, an em.
//! Widgets given a width in points stay that wide at every font size and their text runs past them (the layout
//! audit, `--shots layout`, finds such places); these give the width the text needs instead.

use egui::{TextStyle, Ui};

/// The body text's size in points: an em.
pub(crate) fn em(ui: &Ui) -> f32 {
    TextStyle::Body.resolve(ui.style()).size
}

/// A text's width in a style's font, unwrapped.
pub(crate) fn text_width(ui: &Ui, text: &str, style: TextStyle) -> f32 {
    let font = style.resolve(ui.style());
    ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::WHITE).size().x
}

/// A combo box's width for its choices: the widest, its padding and the arrow.
pub(crate) fn combo_width<'a>(ui: &Ui, choices: impl IntoIterator<Item = &'a str>) -> f32 {
    let widest = choices.into_iter().map(|c| text_width(ui, c, TextStyle::Button)).fold(0.0, f32::max);
    let s = ui.spacing();
    widest + 2.0 * s.button_padding.x + s.icon_width + s.icon_spacing + 2.0
}

/// A button's width for its label.
pub(crate) fn button_width(ui: &Ui, label: &str) -> f32 {
    text_width(ui, label, TextStyle::Button) + 2.0 * ui.spacing().button_padding.x + 2.0
}

/// A button's height for one line of text at the current font.
pub(crate) fn button_height(ui: &Ui) -> f32 {
    let s = ui.spacing();
    ui.text_style_height(&TextStyle::Button).max(s.interact_size.y - 2.0 * s.button_padding.y)
        + 2.0 * s.button_padding.y
}

/// How much larger the interface's text is than at the default font size (1 at 13 px): a size in points given
/// for the default font, times this, keeps its proportion to the text at any font size.
pub(crate) fn scale_of(ctx: &egui::Context) -> f32 {
    // never under 1: smaller fonts keep the default's room (a button's padding does not shrink with its text)
    let body = ctx.style_of(ctx.theme()).text_styles.get(&TextStyle::Body).map_or(13.0, |f| f.size);
    (body / crate::theme::text_size(crate::theme::DEFAULT_FONT_SIZE)).max(1.0)
}

/// A size in points chosen at the default font size, at the current one (the same at 13 px).
pub(crate) fn scaled(ui: &Ui, pts: f32) -> f32 {
    pts * scale_of(ui.ctx())
}

/// Whether a plot's legend of `entries` lines leaves the plot readable: at most half its height (with fewer
/// points of plot, at a large font or in a short panel, the legend would cover the curves and the axis).
pub(crate) fn legend_fits(ui: &Ui, plot_height: f32, entries: usize) -> bool {
    let line = ui.text_style_height(&TextStyle::Body) + 4.0;
    (entries.max(1) as f32 * line + 12.0) <= 0.5 * plot_height
}

/// A table cell's text: as it is when it fits `max_width`, else cut short there with an ellipsis, whole on hover
/// (a long message does not widen its column, and push the next ones past the panel; a grid's rows keep one
/// line, wrapped text would run over the next row).
pub(crate) fn cell(ui: &mut Ui, text: &str, max_width: f32) {
    if text_width(ui, text, TextStyle::Body) <= max_width {
        ui.label(text);
    } else {
        let h = ui.text_style_height(&TextStyle::Body);
        ui.add_sized([max_width, h], egui::Label::new(text).truncate()).on_hover_text(text);
    }
}
