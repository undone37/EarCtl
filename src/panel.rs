//! Popup panel in the style of a macOS menu bar panel (egui).
use crate::sony::{EQ, eq_name};
use crate::{Act, Band, Cmd, Sh, Status, battery_text, lock, mode_name, tr};
use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, FontId, Layout, RichText, Sense, Vec2,
    ViewportCommand,
};

const ACCENT: Color32 = Color32::from_rgb(0x2f, 0x7c, 0xf6);
const WIDTH: f32 = 320.0;

pub fn show(sh: Sh, tx: Cmd) -> Result<(), Box<dyn std::error::Error>> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("earctl")
            .with_app_id("earctl")
            .with_inner_size([WIDTH, 400.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native(
        "earctl",
        opts,
        Box::new(move |cc| {
            lock(&sh).ctx = Some(cc.egui_ctx.clone());
            Ok(Box::new(Panel {
                sh,
                tx,
                focused: false,
                eq_open: false,
                level: 20,
                bands: vec![],
            }))
        }),
    )?;
    Ok(())
}

struct Panel {
    sh: Sh,
    tx: Cmd,
    focused: bool,
    eq_open: bool,
    // local copies so the device status doesn't overwrite sliders while dragging
    level: u8,
    bands: Vec<Band>,
}

impl Panel {
    /// Send a command and optimistically update the shared status right away.
    fn send(&self, act: Act, f: impl FnOnce(&mut Status, &mut u8)) {
        let mut g = lock(&self.sh);
        let mut amb = g.amb;
        if let Some(s) = &mut g.st {
            f(s, &mut amb);
        }
        g.amb = amb;
        drop(g);
        let _ = self.tx.send(act);
    }

    fn body(&mut self, ui: &mut egui::Ui, s: &Status) {
        let mut sub = tr("Verbunden", "Connected").to_string();
        if let Some((_, level)) = s.parts.iter().find(|p| p.0 == "Case") {
            sub += &format!(" · Case {level}%");
        }
        header(ui, &s.name, &sub, Some(battery_text(s)));
        ui.separator();

        section(ui, tr("Geräuschsteuerung", "Noise Control"));
        for (i, icon) in [(2, "🔈"), (1, "👂"), (0, "🔇")] {
            if row(ui, icon, mode_name(i), None, s.mode == i).clicked() {
                self.send(Act::Mode(i), |s, _| s.mode = i);
            }
            if i == 1 && s.mode == 1 && s.level.is_some() {
                ui.horizontal(|ui| {
                    ui.add_space(44.0);
                    ui.spacing_mut().slider_width = WIDTH - 130.0;
                    let r = ui.add(egui::Slider::new(&mut self.level, 1..=20));
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        let l = self.level;
                        let act = Act::Ambient {
                            level: Some(l),
                            voice: None,
                        };
                        self.send(act, |s, a| (s.level, *a) = (Some(l), l));
                    }
                });
                if row(
                    ui,
                    "🎤",
                    tr("Fokus auf Stimme", "Focus on Voice"),
                    None,
                    s.voice == Some(true),
                )
                .clicked()
                {
                    let v = s.voice != Some(true);
                    let act = Act::Ambient {
                        level: None,
                        voice: Some(v),
                    };
                    self.send(act, |s, _| s.voice = Some(v));
                }
            }
        }
        ui.separator();

        if let Some(on) = s.stc
            && row(ui, "💬", "Speak-to-Chat", None, on).clicked()
        {
            self.send(Act::Stc(!on), |s, _| s.stc = Some(!on));
        }
        if let Some(on) = s.dsee
            && row(ui, "✨", "DSEE Extreme", None, on).clicked()
        {
            self.send(Act::Dsee(!on), |s, _| s.dsee = Some(!on));
        }
        if s.eq.is_some() || !s.bands.is_empty() {
            let sub = s.eq.map(eq_name);
            if row(ui, "🎵", "Equalizer", sub, self.eq_open).clicked() {
                self.eq_open ^= true;
            }
            if self.eq_open {
                self.equalizer(ui, s.eq);
            }
        }
        ui.separator();

        ui.horizontal(|ui| {
            if s.power_off
                && ui
                    .button(tr("Kopfhörer ausschalten", "Turn off headphones"))
                    .clicked()
            {
                let _ = self.tx.send(Act::PowerOff);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(tr("Beenden", "Quit")).clicked() {
                    std::process::exit(0);
                }
            });
        });
    }

    fn equalizer(&mut self, ui: &mut egui::Ui, eq: Option<u8>) {
        if let Some(eq) = eq {
            ui.horizontal(|ui| {
                ui.add_space(44.0);
                egui::ComboBox::from_id_salt("eq")
                    .selected_text(eq_name(eq))
                    .width(WIDTH - 90.0)
                    .show_ui(ui, |ui| {
                        for &(_, id, _) in EQ {
                            if ui.selectable_label(eq == id, eq_name(id)).clicked() {
                                self.send(Act::EqPreset(id), |s, _| s.eq = Some(id));
                            }
                        }
                    });
            });
        }
        if self.bands.is_empty() {
            return; // device reports no bands (e.g. preset "Off")
        }
        ui.add_space(4.0);
        let mut changed = false;
        ui.columns(self.bands.len(), |cols| {
            for (b, ui) in self.bands.iter_mut().zip(cols) {
                ui.vertical_centered(|ui| {
                    let r = ui.add(
                        egui::Slider::new(&mut b.gain, -b.max..=b.max)
                            .step_by(b.step as f64)
                            .vertical()
                            .show_value(false),
                    );
                    changed |= r.drag_stopped() || (r.changed() && !r.dragged());
                    ui.small(if b.step < 1.0 {
                        format!("{:+.1}", b.gain)
                    } else {
                        format!("{:+.0}", b.gain)
                    });
                    ui.small(&b.label);
                });
            }
        });
        if changed {
            // Sony: the driver switches fixed presets to "Manual" first, bands apply to the active one
            let id = eq.map(|e| if e >= 0xa0 { e } else { 0xa0 });
            let bands = self.bands.clone();
            let gains = bands.iter().map(|b| b.gain).collect();
            self.send(Act::EqBands(gains), move |s, _| {
                (s.eq, s.bands) = (id, bands)
            });
        }
    }
}

impl eframe::App for Panel {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4] // transparent so the rounded corners show
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // popup behaviour: close on focus loss or Esc
        match ctx.input(|i| i.viewport().focused) {
            Some(true) => self.focused = true,
            Some(false) if self.focused => ctx.send_viewport_cmd(ViewportCommand::Close),
            _ => {}
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }

        let (st, amb) = {
            let g = lock(&self.sh);
            (g.st.clone(), g.amb)
        };
        if let Some(s) = &st
            && ctx.dragged_id().is_none()
        {
            self.level = amb.max(1);
            self.bands = s.bands.clone();
        }

        let frame = egui::Frame::window(ui.style())
            .corner_radius(14)
            .inner_margin(14)
            .shadow(egui::epaint::Shadow::NONE);
        let r = frame.show(ui, |ui| {
            ui.set_width(WIDTH - 30.0);
            match &st {
                Some(s) => self.body(ui, s),
                None => {
                    header(ui, "earctl", tr("Nicht verbunden", "Not connected"), None);
                    ui.separator();
                    if ui.button(tr("Beenden", "Quit")).clicked() {
                        std::process::exit(0);
                    }
                }
            }
        });

        // fit the window to its content (EQ expanded/collapsed)
        let want = r.response.rect.size();
        let have = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
        if have.is_none_or(|h| (h - want).length() > 1.0) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(want));
        }
    }
}

fn header(ui: &mut egui::Ui, name: &str, sub: &str, battery: Option<String>) {
    let r = ui.horizontal(|ui| {
        ui.label(RichText::new("🎧").size(30.0));
        ui.vertical(|ui| {
            ui.label(RichText::new(name).strong().size(16.0));
            ui.label(RichText::new(sub).weak());
        });
        if let Some(b) = battery {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(b).size(15.0));
            });
        }
    });
    // only the header moves the window, otherwise it steals the mouse from sliders
    if r.response.interact(Sense::drag()).drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).small().strong().weak());
}

/// Row with a round icon button, like the macOS Control Center.
fn row(ui: &mut egui::Ui, icon: &str, label: &str, sub: Option<&str>, on: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
    let v = ui.visuals();
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 8.0, v.widgets.hovered.weak_bg_fill);
    }
    let c = rect.left_center() + Vec2::new(18.0, 0.0);
    let (bg, fg) = if on {
        (ACCENT, Color32::WHITE)
    } else {
        (v.widgets.inactive.bg_fill, v.text_color())
    };
    p.circle_filled(c, 15.0, bg);
    p.text(
        c,
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(15.0),
        fg,
    );
    let x = rect.left() + 44.0;
    let y = rect.center().y;
    let font = FontId::proportional(14.0);
    match sub {
        Some(sub) => {
            p.text(
                egui::pos2(x, y - 8.0),
                Align2::LEFT_CENTER,
                label,
                font,
                v.text_color(),
            );
            p.text(
                egui::pos2(x, y + 9.0),
                Align2::LEFT_CENTER,
                sub,
                FontId::proportional(11.0),
                v.weak_text_color(),
            );
        }
        None => {
            p.text(
                egui::pos2(x, y),
                Align2::LEFT_CENTER,
                label,
                font,
                v.text_color(),
            );
        }
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
