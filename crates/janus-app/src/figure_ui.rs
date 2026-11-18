//! The Save figure dialog (figure.rs): the size in mm (one column, two columns or free), the resolution, the font in
//! pt, white or transparent, PNG or SVG, the file (as the export dialogs: typed or chosen, a warning before it
//! replaces one), Save (written in the background, in the Exports window) and Copy image. The same dialog serves the
//! series chart, the Analysis tabs and the transect; each says what it draws in `State::render_figure`.

use egui::Context;

use crate::app::State;
use crate::exports::{sanitize, start_export_job, write_text_file};
use crate::figure::{FigureOptions, Format, Rendered, render};

/// What a figure is of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// the series chart
    Chart,
    /// an Analysis tab (its number, as `ANALYSIS_TABS`)
    Analysis(usize),
    /// the transect panel
    Transect,
}

/// The dialog's state (in `ExportUi`).
#[derive(Default)]
pub struct FigureUi {
    /// what the dialog is open for (None: closed)
    pub source: Option<Source>,
    /// just asked: its file name is set at the next frame
    opening: bool,
    pub opts: FigureOptions,
    /// 0 one column (85 mm), 1 two columns (180 mm), 2 free
    width_choice: usize,
    /// the file, as typed
    pub path: String,
    message: String,
}

/// One column of a journal, two columns.
const COLUMNS: [f32; 2] = [85.0, 180.0];

/// `path` with extension `ext` instead of png or svg.
fn swap_ext(path: &str, ext: &str) -> String {
    let p = std::path::Path::new(path);
    match p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) {
        Some(e) if e == "png" || e == "svg" => p.with_extension(ext).to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

fn write_bytes(path: &str, bytes: &[u8]) -> Result<(), String> {
    let tmp = format!("{path}.part");
    let r = std::fs::write(&tmp, bytes).and_then(|_| {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
    });
    r.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {path}: {e}")
    })
}

impl Source {
    /// The dialog's title and the file name's tail.
    fn names(self) -> (String, String) {
        match self {
            Source::Chart => ("Series chart".into(), "chart".into()),
            Source::Transect => ("Transect".into(), "transect".into()),
            Source::Analysis(k) => {
                let name = crate::analysis::ANALYSIS_TABS.iter().find(|t| t.0 == k).map_or("Analysis", |t| t.1);
                (format!("Analysis: {name}"), name.to_lowercase().replace(' ', "_"))
            }
        }
    }
}

impl State {
    /// Opens the Save figure dialog for `source`.
    pub(crate) fn open_figure(&mut self, source: Source) {
        let f = &mut self.export.figure;
        f.source = Some(source);
        f.opening = true;
    }

    /// The figure of `source` drawn as `o` says (None: there is nothing to draw).
    pub(crate) fn render_figure(&mut self, source: Source, o: &FigureOptions) -> Option<Rendered> {
        self.session.as_ref()?;
        Some(match source {
            Source::Chart => render(o, &mut |ui| {
                self.series_plot(ui, true);
            }),
            Source::Analysis(k) => render(o, &mut |ui| self.analysis_tab(ui, k)),
            Source::Transect => render(o, &mut |ui| self.transect_ui(ui)),
        })
    }

    /// The dialog (shown while `figure.source` is set).
    pub(crate) fn figure_dialog(&mut self, ctx: &Context) {
        let Some(source) = self.export.figure.source else { return };
        let (title, tail) = source.names();
        if self.export.figure.opening {
            self.export.figure.opening = false;
            let name = format!(
                "{}_{tail}.{}",
                self.layers.get(self.active).map_or("janus".to_string(), |l| sanitize(&l.name)),
                self.export.figure.opts.format.ext()
            );
            self.export.figure.path =
                std::path::Path::new(&self.export_folder()).join(name).to_string_lossy().into_owned();
            self.export.figure.message.clear();
        }
        if self.session.is_none() {
            self.export.figure.source = None;
            return;
        }
        let mut o = self.export.figure.opts.clone();
        let mut choice = self.export.figure.width_choice;
        let (mut save, mut copy, mut cancel, mut can) = (false, false, false, true);
        let modal = egui::Modal::new(egui::Id::new("figure dialog")).show(ctx, |ui| {
            ui.heading(format!("Save figure: {title}"));
            ui.add_space(4.0);
            ui.label("Size:");
            ui.radio_value(&mut choice, 0, "One column (85 mm)");
            ui.radio_value(&mut choice, 1, "Two columns (180 mm)");
            ui.radio_value(&mut choice, 2, "Free");
            if choice < 2 && (o.width_mm - COLUMNS[choice]).abs() > 0.01 {
                o.width_mm = COLUMNS[choice];
                o.height_mm = (o.width_mm * 0.7).round();
            }
            ui.horizontal(|ui| {
                ui.add_enabled_ui(choice == 2, |ui| {
                    ui.add(egui::DragValue::new(&mut o.width_mm).range(20.0..=500.0).suffix(" mm"));
                    ui.label("wide");
                });
                ui.add(egui::DragValue::new(&mut o.height_mm).range(15.0..=500.0).suffix(" mm"));
                ui.label("high");
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Format:");
                ui.radio_value(&mut o.format, Format::Png, "PNG");
                ui.radio_value(&mut o.format, Format::Svg, "SVG (text stays text)");
            });
            ui.add_enabled_ui(o.format == Format::Png, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Resolution:");
                    for d in [150, 300, 600] {
                        ui.radio_value(&mut o.dpi, d, format!("{d} dpi"));
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("Font:");
                ui.add(egui::DragValue::new(&mut o.font_pt).range(5.0..=18.0).speed(0.1).suffix(" pt"));
                ui.label("Background:");
                ui.radio_value(&mut o.transparent, false, "White");
                ui.radio_value(&mut o.transparent, true, "Transparent");
            });
            if o.format == Format::Png {
                let [w, h] = o.pixels();
                if o.fits() {
                    ui.weak(format!("{w} x {h} px, light colours whatever the interface's theme."));
                } else {
                    ui.colored_label(
                        crate::theme::warning(),
                        format!("{w} x {h} px is too large: lower the size or the dpi."),
                    );
                    can = false;
                }
            } else {
                ui.weak("Vector: the paper's size is kept, the shapes as paths, the text as text.");
            }
            crate::panels::section(ui, "Save to");
            let ext = o.format.ext();
            let mut enter = false;
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.export.figure.path)
                        .desired_width(ui.text_style_height(&egui::TextStyle::Body) * 34.0),
                );
                enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                r.on_hover_text(format!("Full path of the file, or a name alone (saved in {}).", self.export_folder()));
                if ui.button("Browse...").clicked() {
                    let target = self.target_of(&self.export.figure.path, ext);
                    let t = std::path::Path::new(&target);
                    let known = !target.is_empty() && t.parent().is_some_and(|p| p.is_dir());
                    let name = if known {
                        t.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
                    } else {
                        format!("{tail}.{ext}")
                    };
                    let folder = if known {
                        t.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let filter = if o.format == Format::Png { "PNG image" } else { "SVG image" };
                    if let Some(p) = self.ask_save_path(&format!("Save figure: {title}"), &name, filter, ext, &folder) {
                        self.export.figure.path = p;
                    }
                }
            });
            let target = self.target_of(&self.export.figure.path, ext);
            let t = std::path::Path::new(&target);
            let file = || t.file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned());
            if target.is_empty() {
                ui.colored_label(crate::theme::warning(), "Type a file name, or choose one with Browse...");
                can = false;
            } else if !t.parent().is_some_and(|p| p.is_dir()) {
                ui.colored_label(
                    crate::theme::warning(),
                    format!(
                        "The folder {} does not exist.",
                        t.parent().map_or(String::new(), |p| p.display().to_string())
                    ),
                );
                can = false;
            } else if t.is_dir() {
                ui.colored_label(crate::theme::warning(), format!("{} is a folder: add a file name.", file()));
                can = false;
            } else if t.exists() {
                ui.colored_label(crate::theme::warning(), format!("{} exists and will be replaced.", file()));
            } else {
                ui.weak(format!("Saved as {target}"));
            }
            if !self.export.figure.message.is_empty() {
                ui.weak(self.export.figure.message.clone());
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                save = ui.add_enabled(can, egui::Button::new("Save").min_size(egui::vec2(100.0, 0.0))).clicked()
                    || (enter && can);
                copy = ui
                    .add_enabled(o.fits(), egui::Button::new("Copy image").min_size(egui::vec2(100.0, 0.0)))
                    .on_hover_text("The figure as a PNG image in the clipboard (at the size and dpi above)")
                    .clicked();
                cancel = ui.add(egui::Button::new("Cancel").min_size(egui::vec2(100.0, 0.0))).clicked();
            });
            target
        });
        // the format changed: the typed file's extension follows
        if o.format != self.export.figure.opts.format {
            self.export.figure.path = swap_ext(&self.export.figure.path, o.format.ext());
        }
        self.export.figure.opts = o.clone();
        self.export.figure.width_choice = choice;
        if cancel || modal.should_close() {
            self.export.figure.source = None;
            return;
        }
        if copy {
            let png = FigureOptions { format: Format::Png, ..o.clone() };
            if let Some(r) = self.render_figure(source, &png) {
                ctx.copy_image(r.to_color_image());
                self.export.figure.message = "Copied to the clipboard.".into();
            }
        }
        if save {
            let target = modal.inner;
            self.export.figure.source = None;
            self.remember_export_folder(&target);
            self.save_figure(source, o, target, ctx);
        }
    }

    /// Draws the figure now (the panels are State's) and writes it in the background, in the Exports window.
    pub(crate) fn save_figure(&mut self, source: Source, o: FigureOptions, path: String, ctx: &Context) {
        let Some(r) = self.render_figure(source, &o) else { return };
        let [w, h] = o.pixels();
        let title = match o.format {
            Format::Png => format!("{} as PNG, {w} x {h} px", source.names().0),
            Format::Svg => format!("{} as SVG, {} x {} mm", source.names().0, o.width_mm, o.height_mm),
        };
        let p = path.clone();
        let job = start_export_job(title, path, ctx, move |_| match o.format {
            Format::Png => write_bytes(&p, &r.to_png()?),
            Format::Svg => write_text_file(&p, &r.to_svg(o.width_mm, o.height_mm)),
        });
        self.exports.push(job);
        self.show_exports = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_follows_the_format() {
        assert_eq!(swap_ext("C:\\a\\chart.png", "svg"), "C:\\a\\chart.svg");
        assert_eq!(swap_ext("fig.SVG", "png"), "fig.png");
        assert_eq!(swap_ext("fig", "png"), "fig");
        assert_eq!(swap_ext("fig.tif", "png"), "fig.tif");
    }
}
