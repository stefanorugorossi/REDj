//! Native egui user interface.

mod backend;
mod settings;
mod widgets;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use egui::{Color32, RichText, Ui, vec2};
use redj_analysis::{AnalysisOptions, default_cache, library_from_dirs};
use redj_control::{Console, Session};
use redj_core::{Library, TrackId};
use redj_engine::Command;
use redj_engine::driver::{Routing, output_devices};
use redj_midi::{GlobalKnob, Knob, Mapper, builtin_profiles};

pub use settings::Settings;
use widgets::*;

/// Command-line options of the GUI.
#[derive(Debug, Clone, Default)]
pub struct UiOptions {
    /// Music folders to load at startup (added to the saved ones).
    pub music: Vec<PathBuf>,
    /// Load the first two library tracks on decks A and B and play A.
    pub demo: bool,
}

/// Runs the GUI until the window closes.
pub fn run(opts: UiOptions) -> eframe::Result {
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("REDj")
            .with_inner_size([1280.0, 760.0])
            .with_maximized(true)
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "REDj",
        native,
        Box::new(move |cc| Ok(Box::new(RedjApp::new(cc, opts)))),
    )
}

struct MidiState {
    _conn: Option<redj_midi::io::MidiInputConn>,
    port: Option<String>,
    profile: Option<String>,
}

pub struct RedjApp {
    settings: Settings,
    console: Console,
    backend: backend::Backend,
    midi: MidiState,
    library_rx: Option<Receiver<(Library, usize)>>,
    analyzing: bool,
    filter: String,
    show_settings: bool,
    status_line: String,
    learn_target: Option<redj_midi::Action>,
    /// Pending `--demo` steps.
    demo: u8,
}

impl std::fmt::Debug for RedjApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedjApp").finish()
    }
}

fn style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
}

impl RedjApp {
    fn new(cc: &eframe::CreationContext<'_>, opts: UiOptions) -> Self {
        style(&cc.egui_ctx);
        let mut settings = Settings::load();
        for d in opts.music {
            if !settings.music_dirs.contains(&d) {
                settings.music_dirs.push(d);
            }
        }
        let session = Arc::new(Session::new(Library::new()));
        let (backend, handle, sr, err) = backend::start(
            settings.audio_device.as_deref(),
            Routing {
                master: settings.master_channels,
                phones: settings.phones_channels,
            },
            settings.decks,
        );
        let console = Console::new(session, handle, sr, settings.decks);
        let mut app = Self {
            settings,
            console,
            backend,
            midi: MidiState {
                _conn: None,
                port: None,
                profile: None,
            },
            library_rx: None,
            analyzing: false,
            filter: String::new(),
            show_settings: false,
            status_line: err.map(|e| format!("audio: {e}")).unwrap_or_default(),
            learn_target: None,
            demo: u8::from(opts.demo),
        };
        app.connect_midi();
        app.analyze_library();
        app
    }

    fn analyze_library(&mut self) {
        let dirs = self.settings.music_dirs.clone();
        if dirs.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let cache = default_cache();
            let (lib, failed) =
                library_from_dirs(&dirs, cache.as_ref(), &AnalysisOptions::default());
            let _ = tx.send((lib, failed.len()));
        });
        self.library_rx = Some(rx);
        self.analyzing = true;
    }

    fn connect_midi(&mut self) {
        let ports = redj_midi::io::input_ports();
        let profiles = builtin_profiles();
        let port = match &self.settings.midi_port {
            Some(p) => ports.iter().find(|x| *x == p).cloned(),
            None => ports
                .iter()
                .find(|p| profiles.iter().any(|pr| pr.matches_port(p)))
                .cloned(),
        };
        self.midi = MidiState {
            _conn: None,
            port: None,
            profile: None,
        };
        let Some(port) = port else {
            self.console.detach_midi();
            return;
        };
        let profile = profiles.into_iter().find(|p| p.matches_port(&port));
        let (tx, rx) = channel();
        match redj_midi::io::MidiInputConn::open(&port, tx) {
            Ok(conn) => {
                // LED feedback on the output port with the same name, if any.
                let out = redj_midi::io::output_ports()
                    .into_iter()
                    .find(|o| o == &port || profile.as_ref().is_some_and(|p| p.matches_port(o)))
                    .and_then(|o| redj_midi::io::MidiOutputConn::open(&o).ok());
                let sender: Option<Box<dyn FnMut(redj_midi::MidiMessage) + Send>> =
                    out.map(|mut o| Box::new(move |m| o.send(m)) as Box<dyn FnMut(_) + Send>);
                self.midi.profile = profile.as_ref().map(|p| p.name.clone());
                self.console.attach_midi(Mapper::new(profile), rx, sender);
                self.midi._conn = Some(conn);
                self.midi.port = Some(port);
            }
            Err(e) => self.status_line = format!("MIDI: {e}"),
        }
    }

    fn restart_audio(&mut self) {
        let session = self.console.session().clone();
        let (backend, handle, sr, err) = backend::start(
            self.settings.audio_device.as_deref(),
            Routing {
                master: self.settings.master_channels,
                phones: self.settings.phones_channels,
            },
            self.settings.decks,
        );
        self.backend = backend;
        self.console = Console::new(session, handle, sr, self.settings.decks);
        self.connect_midi();
        self.status_line = err
            .map(|e| format!("audio: {e}"))
            .unwrap_or_else(|| "audio restarted".into());
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("REDj").size(20.0).strong().color(ACCENT));
            ui.separator();
            let st = *self.console.status();
            ui.label(
                RichText::new(format!("{:.2} BPM", st.master_bpm))
                    .monospace()
                    .size(16.0),
            );
            ui.separator();
            let ai_on = self.console.ai_on;
            if toggle(ui, ai_on, "AI", AI, vec2(40.0, 22.0)).clicked() {
                self.console.ai_on = !ai_on;
            }
            ui.separator();
            ui.label(format!("audio: {}", self.backend.describe()));
            ui.label(match (&self.midi.port, &self.midi.profile) {
                (Some(p), Some(pr)) => format!("MIDI: {pr} ({p})"),
                (Some(p), None) => format!("MIDI: {p}"),
                _ => "MIDI: none".into(),
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Settings").clicked() {
                    self.show_settings = !self.show_settings;
                }
                if ui.button("Add music folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    if !self.settings.music_dirs.contains(&dir) {
                        self.settings.music_dirs.push(dir);
                        self.settings.save();
                    }
                    self.analyze_library();
                }
                let gr = 20.0 * st.limiter_gain.max(1e-5).log10();
                ui.label(RichText::new(format!("GR {gr:+.1} dB")).monospace().small());
                vu(ui, st.master_peak, 22.0);
            });
        });
    }

    fn deck_panel(&mut self, ui: &mut Ui, d: usize) {
        let st = self.console.status().decks[d];
        let loaded = self.console.loaded(d).cloned();
        let zoom = self.settings.zoom_sec;
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}", (b'A' + d as u8) as char))
                        .size(22.0)
                        .strong()
                        .color(ACCENT),
                );
                match &loaded {
                    Some(l) => {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(
                                    l.info
                                        .tags
                                        .title
                                        .clone()
                                        .unwrap_or_else(|| l.info.display_name()),
                                )
                                .strong()
                                .size(15.0),
                            );
                            ui.label(
                                RichText::new(l.info.tags.artist.clone().unwrap_or_default())
                                    .color(Color32::from_gray(170)),
                            );
                        });
                    }
                    None if self.console.pending(d).is_some() => {
                        ui.spinner();
                        ui.label("loading…");
                    }
                    None => {
                        ui.label(
                            RichText::new("empty — load a track from the library")
                                .color(Color32::from_gray(140)),
                        );
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(l) = &loaded {
                        let sr = f64::from(l.audio.sample_rate);
                        let left = (st.frames as f64 - st.position) / sr / st.rate.max(0.01);
                        ui.label(
                            RichText::new(format!("-{}", mmss(left)))
                                .monospace()
                                .size(16.0),
                        );
                        ui.label(RichText::new(mmss(st.position / sr)).monospace().size(16.0));
                        ui.separator();
                        if let Some(k) = l.info.key {
                            ui.label(RichText::new(k.camelot()).strong());
                        }
                        let pct = (st.rate - 1.0) * 100.0;
                        ui.label(
                            RichText::new(format!("{:+.2}%", pct))
                                .monospace()
                                .color(Color32::from_gray(170)),
                        );
                        ui.label(
                            RichText::new(format!("{:.2}", st.bpm))
                                .monospace()
                                .size(18.0)
                                .strong(),
                        );
                    }
                });
            });
            let marks = Marks {
                cue: st.loaded.then_some(st.cue),
                hot_cues: &st.hot_cues,
                loop_range: st.loop_range,
                loop_active: st.loop_active,
                ai_region: None,
            };
            let resp = zoomed_waveform(
                ui,
                loaded.as_ref().map(|l| &*l.waveform),
                loaded.as_ref().map(|l| &*l.audio),
                st.position,
                st.rate,
                zoom,
                &marks,
                110.0,
            );
            // Drag the waveform to scrub/nudge like a jog wheel.
            if resp.drag_started() {
                self.console.send(Command::JogTouch { deck: d, on: true });
            }
            if resp.dragged() {
                self.console.send(Command::Jog {
                    deck: d,
                    ticks: -resp.drag_delta().x * 2.0,
                });
            }
            if resp.drag_stopped() {
                self.console.send(Command::JogTouch { deck: d, on: false });
            }
            let frac = if st.frames > 0 {
                st.position / st.frames as f64
            } else {
                0.0
            };
            if let Some(f) = overview(ui, loaded.as_ref().map(|l| &*l.waveform), frac, 34.0, None) {
                self.console.send(Command::Seek {
                    deck: d,
                    frame: f * st.frames as f64,
                });
            }
            ui.horizontal(|ui| {
                let sz = vec2(52.0, 30.0);
                if toggle(ui, false, "CUE", Color32::from_rgb(255, 160, 0), sz)
                    .is_pointer_button_down_on()
                {
                    if !self.cue_held(d) {
                        self.console.send(Command::Cue {
                            deck: d,
                            pressed: true,
                        });
                        self.set_cue_held(ui, d, true);
                    }
                } else if self.cue_held(d) {
                    self.console.send(Command::Cue {
                        deck: d,
                        pressed: false,
                    });
                    self.set_cue_held(ui, d, false);
                }
                if toggle(
                    ui,
                    st.playing,
                    if st.playing { "PAUSE" } else { "PLAY" },
                    Color32::from_rgb(60, 200, 90),
                    sz,
                )
                .clicked()
                {
                    self.console.send(Command::TogglePlay { deck: d });
                }
                if toggle(ui, st.sync, "SYNC", Color32::from_rgb(60, 160, 255), sz).clicked() {
                    self.console.send(Command::Sync {
                        deck: d,
                        on: !st.sync,
                    });
                }
                if toggle(
                    ui,
                    st.is_master,
                    "MST",
                    Color32::from_rgb(255, 210, 60),
                    vec2(40.0, 30.0),
                )
                .clicked()
                {
                    self.console.send(Command::SetMaster { deck: d });
                }
                if toggle(
                    ui,
                    st.keylock,
                    "KEY",
                    Color32::from_rgb(200, 200, 200),
                    vec2(40.0, 30.0),
                )
                .clicked()
                {
                    self.console.send(Command::Keylock {
                        deck: d,
                        on: !st.keylock,
                    });
                }
                if toggle(
                    ui,
                    st.quantize,
                    "Q",
                    Color32::from_rgb(200, 200, 200),
                    vec2(28.0, 30.0),
                )
                .clicked()
                {
                    self.console.send(Command::Quantize {
                        deck: d,
                        on: !st.quantize,
                    });
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("LOOP").small().color(Color32::from_gray(150)));
                for beats in [1.0, 4.0, 8.0, 16.0] {
                    let on = st.loop_active
                        && st
                            .loop_range
                            .zip(loaded.as_ref())
                            .is_some_and(|((a, b), l)| {
                                ((l.audio.frame_to_beat(b) - l.audio.frame_to_beat(a)) - beats)
                                    .abs()
                                    < 1e-6
                            });
                    if toggle(
                        ui,
                        on,
                        &format!("{beats}"),
                        Color32::from_rgb(60, 200, 90),
                        vec2(30.0, 24.0),
                    )
                    .clicked()
                    {
                        self.console.send(Command::BeatLoop { deck: d, beats });
                    }
                }
                ui.separator();
                ui.label(RichText::new("JUMP").small().color(Color32::from_gray(150)));
                if ui
                    .add_sized(vec2(40.0, 24.0), egui::Button::new("-16"))
                    .clicked()
                {
                    self.console.send(Command::BeatJump {
                        deck: d,
                        beats: -16.0,
                    });
                }
                if ui
                    .add_sized(vec2(40.0, 24.0), egui::Button::new("+16"))
                    .clicked()
                {
                    self.console.send(Command::BeatJump {
                        deck: d,
                        beats: 16.0,
                    });
                }
            });
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("HOT CUE")
                        .small()
                        .color(Color32::from_gray(150)),
                );
                for i in 0..4 {
                    let set = st.hot_cues[i].is_some();
                    let r = toggle(
                        ui,
                        set,
                        &format!("{}", i + 1),
                        Color32::from_rgb(60, 200, 90),
                        vec2(30.0, 24.0),
                    )
                    .on_hover_text("click: set or jump; shift-click: clear");
                    if r.clicked() {
                        if ui.input(|inp| inp.modifiers.shift) {
                            self.console
                                .send(Command::ClearHotCue { deck: d, index: i });
                        } else {
                            self.console.send(Command::HotCue { deck: d, index: i });
                        }
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut t = self.console.knob(d, Knob::Tempo);
                    let r = ui.add(
                        egui::Slider::new(&mut t, 0.0..=1.0)
                            .show_value(false)
                            .text(format!("tempo ±{:.0}%", st.tempo_range * 100.0)),
                    );
                    if r.changed() {
                        self.console.set_knob(d, Knob::Tempo, t);
                    }
                    if r.double_clicked() {
                        self.console.set_knob(d, Knob::Tempo, 0.5);
                    }
                });
            });
        });
    }

    fn run_demo(&mut self) {
        match self.demo {
            1 if !self.analyzing => {
                let ids: Vec<TrackId> = self
                    .console
                    .browse_order()
                    .iter()
                    .take(2)
                    .cloned()
                    .collect();
                for (d, id) in ids.iter().enumerate() {
                    self.console.load(d, id);
                }
                self.demo = 2;
            }
            2 if self.console.status().decks[0].loaded => {
                self.console.send(Command::Play { deck: 0, on: true });
                self.demo = 0;
            }
            _ => {}
        }
    }

    fn cue_held(&self, d: usize) -> bool {
        CUE_HELD.with(|c| c.borrow()[d])
    }

    fn set_cue_held(&self, ui: &Ui, d: usize, v: bool) {
        CUE_HELD.with(|c| c.borrow_mut()[d] = v);
        ui.ctx().request_repaint();
    }

    fn mixer_strip(&mut self, ui: &mut Ui, d: usize) {
        let st = self.console.status().decks[d];
        ui.vertical(|ui| {
            ui.set_width(56.0);
            ui.label(
                RichText::new(format!("{}", (b'A' + d as u8) as char))
                    .strong()
                    .color(ACCENT),
            );
            for (k, label, default) in [
                (Knob::Trim, "TRIM", 0.5),
                (Knob::EqHigh, "HI", 0.5),
                (Knob::EqMid, "MID", 0.5),
                (Knob::EqLow, "LOW", 0.5),
                (Knob::Filter, "FILTER", 0.5),
                (Knob::Echo, "ECHO", 0.0),
            ] {
                let mut v = self.console.knob(d, k);
                if knob(ui, &mut v, default, label, 28.0) {
                    self.console.set_knob(d, k, v);
                }
            }
            if toggle(
                ui,
                st.pfl,
                "CUE",
                Color32::from_rgb(255, 160, 0),
                vec2(44.0, 22.0),
            )
            .clicked()
            {
                self.console.send(Command::Pfl {
                    deck: d,
                    on: !st.pfl,
                });
            }
            ui.horizontal(|ui| {
                let mut v = self.console.knob(d, Knob::Volume);
                if ui
                    .add(
                        egui::Slider::new(&mut v, 0.0..=1.0)
                            .vertical()
                            .show_value(false),
                    )
                    .changed()
                {
                    self.console.set_knob(d, Knob::Volume, v);
                }
                vu(ui, st.peak, 90.0);
            });
        });
    }

    fn mixer(&mut self, ui: &mut Ui) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for d in 0..self.console.deck_count() {
                    ui.group(|ui| self.mixer_strip(ui, d));
                }
            });
            ui.horizontal(|ui| {
                ui.label("XF");
                let mut x = self.console.global(GlobalKnob::Crossfader);
                let r = ui.add(egui::Slider::new(&mut x, 0.0..=1.0).show_value(false));
                if r.changed() {
                    self.console.set_global(GlobalKnob::Crossfader, x);
                }
                if r.double_clicked() {
                    self.console.set_global(GlobalKnob::Crossfader, 0.5);
                }
            });
            ui.horizontal(|ui| {
                for (k, label, default) in [
                    (GlobalKnob::MasterLevel, "MASTER", 0.75),
                    (GlobalKnob::CueMix, "CUE/MST", 0.0),
                    (GlobalKnob::PhonesLevel, "PHONES", 0.75),
                ] {
                    let mut v = self.console.global(k);
                    if knob(ui, &mut v, default, label, 34.0) {
                        self.console.set_global(k, v);
                    }
                }
                vu(ui, self.console.status().phones_peak, 46.0);
            });
        });
    }

    fn library(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Library").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filter")
                    .desired_width(220.0),
            );
            if self.analyzing {
                ui.spinner();
                ui.label("analyzing…");
            }
            ui.label(format!("{} tracks", self.console.browse_order().len()));
        });
        let filter = self.filter.to_lowercase();
        let order: Vec<TrackId> = self.console.browse_order().to_vec();
        let cursor = self.console.browse_cursor;
        let decks = self.console.deck_count();
        let session = self.console.session().clone();
        let mut load: Option<(usize, TrackId)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("library")
                    .striped(true)
                    .num_columns(7)
                    .show(ui, |ui| {
                        for h in ["", "Title", "Artist", "BPM", "Key", "Length", "Energy"] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        let lib = session.library();
                        for (i, id) in order.iter().enumerate() {
                            let Some(t) = lib.get(id) else { continue };
                            let name = t.display_name();
                            if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            ui.horizontal(|ui| {
                                for d in 0..decks {
                                    if ui
                                        .small_button(format!("{}", (b'A' + d as u8) as char))
                                        .on_hover_text("load")
                                        .clicked()
                                    {
                                        load = Some((d, id.clone()));
                                    }
                                }
                            });
                            let title = t.tags.title.clone().unwrap_or(name);
                            let text = if i == cursor {
                                RichText::new(title).color(ACCENT)
                            } else {
                                RichText::new(title)
                            };
                            ui.label(text);
                            ui.label(t.tags.artist.clone().unwrap_or_default());
                            ui.label(format!("{:.1}", t.grid.bpm));
                            ui.label(t.key.map(|k| k.camelot()).unwrap_or_default());
                            ui.label(mmss(t.duration_sec));
                            let e = if t.bar_energy.is_empty() {
                                0.0
                            } else {
                                t.bar_energy.iter().sum::<f32>() / t.bar_energy.len() as f32
                            };
                            ui.add(egui::ProgressBar::new(e).desired_width(60.0));
                            ui.end_row();
                        }
                    });
            });
        if let Some((d, id)) = load {
            self.console.load(d, &id);
        }
    }

    fn settings_window(&mut self, ui: &mut Ui) {
        let mut open = self.show_settings;
        let mut restart = false;
        let mut reconnect = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(true)
            .show(ui.ctx(), |ui| {
                ui.heading("Audio");
                let devices = output_devices();
                let current = self.settings.audio_device.clone();
                egui::ComboBox::from_label("Output device")
                    .selected_text(
                        current
                            .as_ref()
                            .and_then(|id| {
                                devices.iter().find(|d| &d.id == id).map(|d| d.name.clone())
                            })
                            .unwrap_or_else(|| "System default".into()),
                    )
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(current.is_none(), "System default")
                            .clicked()
                        {
                            self.settings.audio_device = None;
                        }
                        for d in &devices {
                            let label =
                                format!("{} ({} ch, {} Hz)", d.name, d.channels, d.sample_rate);
                            if ui
                                .selectable_label(current.as_ref() == Some(&d.id), label)
                                .clicked()
                            {
                                self.settings.audio_device = Some(d.id.clone());
                            }
                        }
                    });
                let channels = devices
                    .iter()
                    .find(|d| {
                        Some(&d.id) == self.settings.audio_device.as_ref()
                            || (self.settings.audio_device.is_none() && d.is_default)
                    })
                    .map(|d| d.channels)
                    .unwrap_or(2);
                let pairs: Vec<(u16, u16)> =
                    (0..channels / 2).map(|i| (2 * i, 2 * i + 1)).collect();
                let label = |p: (u16, u16)| format!("{}-{}", p.0 + 1, p.1 + 1);
                egui::ComboBox::from_label("Master channels")
                    .selected_text(label(self.settings.master_channels))
                    .show_ui(ui, |ui| {
                        for &p in &pairs {
                            ui.selectable_value(&mut self.settings.master_channels, p, label(p));
                        }
                    });
                egui::ComboBox::from_label("Headphone channels")
                    .selected_text(
                        self.settings
                            .phones_channels
                            .map(label)
                            .unwrap_or_else(|| "off".into()),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.settings.phones_channels, None, "off");
                        for &p in &pairs {
                            ui.selectable_value(
                                &mut self.settings.phones_channels,
                                Some(p),
                                label(p),
                            );
                        }
                    });
                ui.horizontal(|ui| {
                    ui.label("Decks");
                    ui.selectable_value(&mut self.settings.decks, 2, "2");
                    ui.selectable_value(&mut self.settings.decks, 4, "4");
                });
                if ui
                    .button("Apply audio settings (reloads the engine)")
                    .clicked()
                {
                    restart = true;
                }
                ui.separator();
                ui.heading("MIDI");
                let ports = redj_midi::io::input_ports();
                egui::ComboBox::from_label("Controller")
                    .selected_text(
                        self.settings
                            .midi_port
                            .clone()
                            .unwrap_or_else(|| "Auto-detect".into()),
                    )
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.settings.midi_port.is_none(), "Auto-detect")
                            .clicked()
                        {
                            self.settings.midi_port = None;
                            reconnect = true;
                        }
                        for p in &ports {
                            if ui
                                .selectable_label(self.settings.midi_port.as_ref() == Some(p), p)
                                .clicked()
                            {
                                self.settings.midi_port = Some(p.clone());
                                reconnect = true;
                            }
                        }
                    });
                ui.horizontal(|ui| {
                    ui.label("MIDI learn:");
                    for (label, action) in [
                        ("AI on/off", redj_midi::Action::AiToggle),
                        ("Play A", redj_midi::Action::PlayToggle { deck: 0 }),
                        ("Play B", redj_midi::Action::PlayToggle { deck: 1 }),
                        (
                            "Crossfader",
                            redj_midi::Action::Global {
                                knob: GlobalKnob::Crossfader,
                            },
                        ),
                    ] {
                        if ui.button(label).clicked() {
                            self.console.mapper_mut().learn(action);
                            self.learn_target = Some(action);
                        }
                    }
                });
                if let Some(t) = self.learn_target {
                    if self.console.mapper_mut().is_learning() {
                        ui.label(
                            RichText::new(format!("move a control to map {t:?}…")).color(ACCENT),
                        );
                    } else {
                        self.learn_target = None;
                    }
                }
                ui.separator();
                ui.heading("Display");
                ui.add(
                    egui::Slider::new(&mut self.settings.zoom_sec, 2.0..=30.0)
                        .text("waveform seconds"),
                );
                ui.separator();
                ui.heading("Music folders");
                let mut remove = None;
                for (i, d) in self.settings.music_dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(d.display().to_string());
                        if ui.small_button("remove").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.settings.music_dirs.remove(i);
                }
            });
        self.show_settings = open;
        if restart {
            self.settings.save();
            self.restart_audio();
        }
        if reconnect {
            self.settings.save();
            self.connect_midi();
        }
    }
}

thread_local! {
    static CUE_HELD: std::cell::RefCell<[bool; 4]> = const { std::cell::RefCell::new([false; 4]) };
}

impl eframe::App for RedjApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.library_rx
            && let Ok((lib, failed)) = rx.try_recv()
        {
            let n = lib.len();
            self.console.session().set_library(lib);
            self.console.refresh_browse();
            self.analyzing = false;
            self.library_rx = None;
            self.status_line = if failed > 0 {
                format!("{n} tracks analyzed, {failed} skipped")
            } else {
                format!("{n} tracks analyzed")
            };
        }
        self.console.poll();
        self.run_demo();
        for n in self.console.notices.drain(..) {
            self.status_line = n;
        }
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(RichText::new(&self.status_line).small());
        });
        egui::Panel::bottom("library")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| self.library(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            let decks = self.console.deck_count();
            let mixer_w = 76.0 * decks as f32 + 24.0;
            let deck_w = ((ui.available_width() - mixer_w) / 2.0).max(300.0);
            let left: Vec<usize> = (0..decks).filter(|d| d % 2 == 0).collect();
            let right: Vec<usize> = (0..decks).filter(|d| d % 2 == 1).collect();
            egui::Panel::left("decks_left")
                .exact_size(deck_w)
                .resizable(false)
                .show_separator_line(false)
                .show(ui, |ui| {
                    for d in &left {
                        ui.group(|ui| self.deck_panel(ui, *d));
                    }
                });
            egui::Panel::right("decks_right")
                .exact_size(deck_w)
                .resizable(false)
                .show_separator_line(false)
                .show(ui, |ui| {
                    for d in &right {
                        ui.group(|ui| self.deck_panel(ui, *d));
                    }
                });
            self.mixer(ui);
        });
        if self.show_settings {
            self.settings_window(ui);
        }
        ui.ctx().request_repaint();
    }

    fn on_exit(&mut self) {
        self.settings.save();
    }
}
