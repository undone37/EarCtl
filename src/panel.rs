//! Popup-Panel im Stil eines macOS-Menüleisten-Panels (egui).
use crate::{Cmd, EQ, Sh, Status, anc, eq_bands, eq_name, lock, mode_name, tr};
use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, FontId, Layout, RichText, Sense, Vec2,
    ViewportCommand,
};

const ACCENT: Color32 = Color32::from_rgb(0x2f, 0x7c, 0xf6);
const WIDTH: f32 = 320.0;
const BANDS: [&str; 6] = ["Clear Bass", "400", "1k", "2.5k", "6.3k", "16k"];

pub fn show(sh: Sh, tx: Cmd) -> Result<(), Box<dyn std::error::Error>> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("sonyctl")
            .with_app_id("sonyctl")
            .with_inner_size([WIDTH, 400.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native(
        "sonyctl",
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
    // lokale Kopien, damit Slider beim Ziehen nicht vom Gerätestatus überschrieben werden
    level: u8,
    bands: Vec<i8>,
}

impl Panel {
    /// Befehl senden und den geteilten Status sofort optimistisch anpassen.
    fn send(&self, p: Vec<u8>, f: impl FnOnce(&mut Status, &mut u8)) {
        let mut g = lock(&self.sh);
        let mut amb = g.amb;
        if let Some(s) = &mut g.st {
            f(s, &mut amb);
        }
        g.amb = amb;
        drop(g);
        let _ = self.tx.send(p);
    }

    fn body(&mut self, ui: &mut egui::Ui, s: &Status, amb: u8) {
        let charging = if s.charging { " ⚡" } else { "" };
        header(
            ui,
            tr("Verbunden", "Connected"),
            Some(format!("{}%{charging}", s.bat)),
        );
        ui.separator();

        section(ui, tr("Geräuschsteuerung", "Noise Control"));
        for (i, icon) in [(2, "🔈"), (1, "👂"), (0, "🔇")] {
            if row(ui, icon, mode_name(i), None, s.mode == i).clicked() {
                self.send(anc(i, s.voice, amb), |s, _| s.mode = i);
            }
            if i == 1 && s.mode == 1 {
                ui.horizontal(|ui| {
                    ui.add_space(44.0);
                    ui.spacing_mut().slider_width = WIDTH - 130.0;
                    let r = ui.add(egui::Slider::new(&mut self.level, 1..=20));
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        let l = self.level;
                        self.send(anc(1, s.voice, l), |s, a| (s.level, *a) = (l, l));
                    }
                });
                if row(
                    ui,
                    "🎤",
                    tr("Fokus auf Stimme", "Focus on Voice"),
                    None,
                    s.voice,
                )
                .clicked()
                {
                    self.send(anc(1, !s.voice, amb), |s, _| s.voice = !s.voice);
                }
            }
        }
        ui.separator();

        if let Some(on) = s.stc
            && row(ui, "💬", "Speak-to-Chat", None, on).clicked()
        {
            self.send(vec![0xf8, 0x05, 0x01, !on as u8], |s, _| s.stc = Some(!on));
        }
        if let Some(on) = s.dsee
            && row(ui, "✨", "DSEE Extreme", None, on).clicked()
        {
            self.send(vec![0xe8, 0x02, 0x00, !on as u8], |s, _| s.dsee = Some(!on));
        }
        if row(ui, "🎵", "Equalizer", Some(eq_name(s.eq)), self.eq_open).clicked() {
            self.eq_open ^= true;
        }
        if self.eq_open {
            self.equalizer(ui, s);
        }
        ui.separator();

        ui.horizontal(|ui| {
            if ui
                .button(tr("Kopfhörer ausschalten", "Turn off headphones"))
                .clicked()
            {
                let _ = self.tx.send(vec![0x22, 0x00, 0x01]);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(tr("Beenden", "Quit")).clicked() {
                    std::process::exit(0);
                }
            });
        });
    }

    fn equalizer(&mut self, ui: &mut egui::Ui, s: &Status) {
        ui.horizontal(|ui| {
            ui.add_space(44.0);
            egui::ComboBox::from_id_salt("eq")
                .selected_text(eq_name(s.eq))
                .width(WIDTH - 90.0)
                .show_ui(ui, |ui| {
                    for &(_, id, _) in EQ {
                        if ui.selectable_label(s.eq == id, eq_name(id)).clicked() {
                            self.send(vec![0x58, 0x01, id, 0x00], |s, _| s.eq = id);
                        }
                    }
                });
        });
        if self.bands.len() != BANDS.len() {
            return; // Gerät meldet keine Bänder (z. B. Preset "Off")
        }
        ui.add_space(4.0);
        let mut changed = false;
        ui.columns(BANDS.len(), |cols| {
            for (i, ui) in cols.iter_mut().enumerate() {
                ui.vertical_centered(|ui| {
                    let r = ui.add(
                        egui::Slider::new(&mut self.bands[i], -10..=10)
                            .vertical()
                            .show_value(false),
                    );
                    changed |= r.drag_stopped() || (r.changed() && !r.dragged());
                    ui.small(format!("{:+}", self.bands[i]));
                    ui.small(BANDS[i]);
                });
            }
        });
        if changed {
            // Bänder gelten fürs aktive Preset, feste Presets vorher auf "Manual" umstellen
            let id = if s.eq >= 0xa0 { s.eq } else { 0xa0 };
            if id != s.eq {
                let _ = self.tx.send(vec![0x58, 0x01, id, 0x00]);
            }
            let b = self.bands.clone();
            self.send(eq_bands(&b), move |s, _| (s.eq, s.bands) = (id, b));
        }
    }
}

impl eframe::App for Panel {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4] // transparent, damit die runden Ecken sichtbar sind
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Popup-Verhalten: schließt bei Fokusverlust oder Esc
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
                Some(s) => self.body(ui, s, amb),
                None => {
                    header(ui, tr("Nicht verbunden", "Not connected"), None);
                    ui.separator();
                    if ui.button(tr("Beenden", "Quit")).clicked() {
                        std::process::exit(0);
                    }
                }
            }
        });

        // Fenster an den Inhalt anpassen (EQ auf/zu)
        let want = r.response.rect.size();
        let have = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
        if have.is_none_or(|h| (h - want).length() > 1.0) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(want));
        }
    }
}

fn header(ui: &mut egui::Ui, sub: &str, battery: Option<String>) {
    let r = ui.horizontal(|ui| {
        ui.label(RichText::new("🎧").size(30.0));
        ui.vertical(|ui| {
            ui.label(RichText::new("WH-1000XM4").strong().size(16.0));
            ui.label(RichText::new(sub).weak());
        });
        if let Some(b) = battery {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(b).size(15.0));
            });
        }
    });
    // nur die Kopfzeile verschiebt das Fenster, sonst schnappt sie Slidern die Maus weg
    if r.response.interact(Sense::drag()).drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).small().strong().weak());
}

/// Zeile mit rundem Icon-Button wie im macOS-Kontrollzentrum.
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
