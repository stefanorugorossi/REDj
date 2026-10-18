//! AI director lifecycle and the AI panel.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Color32, RichText, Ui};
use redj_ai::{AgentConfig, LiveDirector, ModelInfo, OllamaClient, spawn_live_director};
use redj_control::{Console, Fallback};
use redj_engine::TransitionState;

use crate::settings::Settings;
use crate::widgets::{ACCENT, AI};

/// Owns the director thread and the list of installed models.
#[derive(Default)]
pub struct AiState {
    director: Option<LiveDirector>,
    running_model: String,
    pub models: Arc<Mutex<Vec<ModelInfo>>>,
}

impl std::fmt::Debug for AiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiState")
            .field("running", &self.director.is_some())
            .finish()
    }
}

impl AiState {
    /// Fetches the installed Ollama models in the background.
    pub fn refresh_models(&self, url: &str) {
        let models = self.models.clone();
        let url = url.to_owned();
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Runtime::new() else {
                return;
            };
            let client = OllamaClient::new(url, Duration::from_secs(30));
            if let Ok(list) = rt.block_on(client.list_models()) {
                *models.lock().unwrap_or_else(|e| e.into_inner()) = list;
            }
        });
    }

    /// Starts or stops the director to match the console's AI switch.
    pub fn sync(&mut self, console: &mut Console, s: &Settings) {
        let d = console.director_mut();
        d.settings.deadline_bars = s.ai_deadline_bars;
        d.settings.frozen_bars = s.frozen_bars;
        d.settings.fallback = if s.ai_loop_fallback {
            Fallback::LoopOutro
        } else {
            Fallback::Blend
        };
        let want = console.ai_on;
        if want && (self.director.is_none() || self.running_model != s.ai_model) {
            let caps = self
                .models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find(|m| m.name == s.ai_model)
                .map(|m| m.capabilities.clone());
            if let Some(c) = &caps
                && !c.iter().any(|x| x == "tools")
            {
                console.notices.push(format!(
                    "{} cannot call tools; choose another model in Settings",
                    s.ai_model
                ));
                console.ai_on = false;
                return;
            }
            let client = OllamaClient::new(&s.ollama_url, Duration::from_secs(900));
            let cfg = AgentConfig {
                model: s.ai_model.clone(),
                think: caps
                    .as_ref()
                    .is_some_and(|c| c.iter().any(|x| x == "thinking"))
                    .then_some(false),
                ..AgentConfig::default()
            };
            self.director = Some(spawn_live_director(console.session().clone(), client, cfg));
            self.running_model = s.ai_model.clone();
            console
                .session()
                .log(format!("AI director started with {}", s.ai_model), None);
        } else if !want && self.director.is_some() {
            self.director = None;
            console.session().log("AI director stopped", None);
        }
    }
}

/// The AI panel: next transition, reject button, log.
pub fn panel(ui: &mut Ui, console: &mut Console) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("AI director").strong().color(AI));
        if !console.ai_on {
            ui.label(RichText::new("off").color(Color32::from_gray(140)));
        } else if console.session().ai_busy() {
            ui.spinner();
            ui.label("planning…");
        } else {
            ui.label(RichText::new("on").color(AI));
        }
    });
    let st = *console.status();
    let live = console.session().live();
    let scheduled = console.director().scheduled().cloned();
    let mut reject = false;
    match &scheduled {
        Some(s) => {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Next: {}", s.name)).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Reject").color(Color32::WHITE))
                                    .fill(ACCENT),
                            )
                            .clicked()
                        {
                            reject = true;
                        }
                    });
                });
                let state = if s.loading {
                    "loading".to_owned()
                } else {
                    match st.transition.state {
                        TransitionState::Active => {
                            let span = (st.transition.end_beat - st.transition.start_beat).max(1.0);
                            let p = ((st.transition.beat - st.transition.start_beat) / span)
                                .clamp(0.0, 1.0);
                            format!("running {:.0}%", p * 100.0)
                        }
                        TransitionState::Armed => {
                            let left = (st.transition.start_beat - st.transition.beat) / 4.0;
                            format!("armed, starts in {left:.1} bars")
                        }
                        TransitionState::Idle => "waiting".into(),
                    }
                };
                ui.label(format!(
                    "{:?} · {} bars at bar {} · by {} · {}",
                    s.style, s.length_bars, s.at_bar, s.by, state
                ));
                if let Some(r) = &s.rationale {
                    ui.label(RichText::new(r).italics().color(Color32::from_gray(190)));
                }
                if st.transition.human != 0 {
                    ui.label(
                        RichText::new("you took over some controls")
                            .small()
                            .color(ACCENT),
                    );
                }
            });
        }
        None => {
            if let Some(l) = &live
                && let (Some(m), Some(dl)) = (l.main_deck, l.deadline_bar)
            {
                let bar = l.decks.get(m).map(|d| d.bar).unwrap_or(0.0);
                ui.label(format!(
                    "no transition scheduled · deadline at bar {dl:.0} ({:.0} bars left)",
                    (dl - bar).max(0.0)
                ));
            }
        }
    }
    if reject {
        console.reject_transition();
    }
    ui.separator();
    // Director log and AI events, newest first.
    let mut lines: Vec<(String, Option<String>)> = console
        .director()
        .log
        .iter()
        .rev()
        .take(12)
        .map(|l| (l.text.clone(), l.rationale.clone()))
        .collect();
    for e in console.session().events().into_iter().rev().take(6) {
        lines.push((e.what, e.rationale));
    }
    egui::ScrollArea::vertical()
        .id_salt("ai_log")
        .show(ui, |ui| {
            for (t, r) in lines {
                ui.label(RichText::new(t).small());
                if let Some(r) = r {
                    ui.label(
                        RichText::new(format!("  {r}"))
                            .small()
                            .italics()
                            .color(Color32::from_gray(150)),
                    );
                }
            }
        });
}
