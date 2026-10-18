//! The live director: turns AI proposals (or the autopilot's) into
//! transitions on the engine, enforcing the frozen horizon and the
//! planning deadline, and keeps the AI informed through the session.

use std::sync::Arc;
use std::time::Instant;

use redj_core::{TrackId, TrackInfo, beats_to_bars};
use redj_engine::{Command, LiveTransition, TransitionState};
use redj_plan::{
    Entry, LoudnessPolicy, MixPlan, PLAN_VERSION, TempoChange, TempoPolicy, Transition,
    TransitionStyle, compile,
};

use super::Console;
use crate::autoplan::plan_transition;
use crate::live::{LiveDeck, LiveSnapshot, PendingView, ProposalReply, TransitionRequest};
use crate::summary::{Compatibility, mix_in_range, mix_out_range};

/// What happens when the AI has not delivered in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fallback {
    /// Mix into the most compatible unplayed track at the outro.
    #[default]
    Blend,
    /// Loop the outro of the playing track and keep waiting.
    LoopOutro,
}

/// Director settings.
#[derive(Debug, Clone, Copy)]
pub struct DirectorSettings {
    /// The next transition must be planned this many bars before its
    /// default start (the outgoing outro).
    pub deadline_bars: f64,
    /// Transitions may not be (re)scheduled to start within this many bars.
    pub frozen_bars: f64,
    pub fallback: Fallback,
}

impl Default for DirectorSettings {
    fn default() -> Self {
        Self {
            deadline_bars: 16.0,
            frozen_bars: 4.0,
            fallback: Fallback::Blend,
        }
    }
}

/// A line of the AI log shown in the UI.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub at: Instant,
    pub text: String,
    pub rationale: Option<String>,
}

/// The transition the director prepared.
#[derive(Debug, Clone)]
pub struct Scheduled {
    pub id: u32,
    pub out_deck: usize,
    pub in_deck: usize,
    pub track: TrackId,
    pub name: String,
    pub at_bar: f64,
    pub length_bars: f64,
    pub style: TransitionStyle,
    pub by: String,
    pub rationale: Option<String>,
    /// Bar of the incoming track where it starts.
    pub in_start_bar: f64,
    /// Waiting for the incoming track to load before arming.
    pub loading: bool,
    timeline: Arc<redj_plan::Timeline>,
    beat_offset: f64,
}

/// Director state (part of the console).
#[derive(Debug, Default)]
pub struct Director {
    pub settings: DirectorSettings,
    main_deck: Option<usize>,
    played: Vec<TrackId>,
    scheduled: Option<Scheduled>,
    next_id: u32,
    seen_completed: u32,
    seen_cancelled: u32,
    pub log: Vec<LogEntry>,
    looped_for_wait: bool,
}

impl Director {
    pub fn scheduled(&self) -> Option<&Scheduled> {
        self.scheduled.as_ref()
    }

    pub fn main_deck(&self) -> Option<usize> {
        self.main_deck
    }

    fn note(&mut self, text: impl Into<String>, rationale: Option<String>) {
        let text = text.into();
        tracing::info!("director: {text}");
        self.log.push(LogEntry {
            at: Instant::now(),
            text,
            rationale,
        });
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }
}

impl Console {
    pub fn director(&self) -> &Director {
        &self.director
    }

    pub fn director_mut(&mut self) -> &mut Director {
        &mut self.director
    }

    /// Rejects the scheduled or running AI transition (user action).
    pub fn reject_transition(&mut self) {
        if let Some(s) = self.director.scheduled.take() {
            self.send(Command::CancelTransition);
            self.director.note(
                format!("rejected by the user: {} ({:?})", s.name, s.style),
                s.rationale,
            );
        }
    }

    fn other_deck(&self, main: usize) -> Option<usize> {
        (0..self.decks).find(|&d| d != main && !self.status.decks[d].playing)
    }

    fn current_bar(&self, deck: usize) -> f64 {
        beats_to_bars(self.status.decks[deck].beat)
    }

    /// Bar where the autopilot would start the next transition, and the
    /// deadline for the AI.
    fn default_bars(&self, main: usize, info: &TrackInfo) -> (f64, f64) {
        let cb = self.current_bar(main);
        let earliest = cb + self.director.settings.frozen_bars;
        let mut at = mix_out_range(info).0;
        if at < earliest {
            at = (earliest / 4.0).ceil() * 4.0;
        }
        (at, at - self.director.settings.deadline_bars)
    }

    /// Validates a request and compiles the two-track plan.
    fn build(&self, req: &TransitionRequest) -> Result<(Scheduled, Vec<String>), Vec<String>> {
        let main = self
            .director
            .main_deck
            .ok_or_else(|| vec!["no track is playing".to_owned()])?;
        let out = self
            .loaded(main)
            .map(|l| l.info.clone())
            .ok_or_else(|| vec!["the playing deck has no track".to_owned()])?;
        if self.director.scheduled.as_ref().is_some_and(|s| !s.loading)
            && self.status.transition.state == TransitionState::Active
        {
            return Err(vec![
                "a transition is already running; wait until it ends".into(),
            ]);
        }
        let in_deck = self
            .other_deck(main)
            .ok_or_else(|| vec!["no free deck for the next track".to_owned()])?;
        let lib = self.session.library();
        let inc = lib.resolve(&req.track).cloned().ok_or_else(|| {
            vec![format!(
                "unknown track id {:?}; use ids from list_tracks",
                req.track
            )]
        })?;
        drop(lib);
        if inc.id == out.id {
            return Err(vec!["that track is already playing".into()]);
        }
        let cb = self.current_bar(main);
        let earliest = cb + self.director.settings.frozen_bars;
        if req.at_bar < earliest {
            return Err(vec![format!(
                "at_bar {} is too soon: the playing track is at bar {:.1} and the next {} bars are frozen; use at_bar >= {}",
                req.at_bar,
                cb,
                self.director.settings.frozen_bars,
                earliest.ceil()
            )]);
        }
        let start = cb.floor();
        let plan = MixPlan {
            version: PLAN_VERSION,
            title: None,
            tempo: TempoPolicy::Fixed {
                bpm: self.status.decks[main].bpm.max(1.0),
            },
            keylock: true,
            loudness: LoudnessPolicy {
                auto_gain: false,
                target_lufs: -9.0,
            },
            entries: vec![
                Entry {
                    track: out.id.clone(),
                    start_bar: start,
                    end_bar: None,
                    fade_out_bars: None,
                    gain_db: None,
                    loops: Vec::new(),
                    transition: None,
                    rationale: None,
                },
                Entry {
                    track: inc.id.clone(),
                    start_bar: req.start_bar.unwrap_or_else(|| mix_in_range(&inc).0),
                    end_bar: None,
                    fade_out_bars: None,
                    gain_db: None,
                    loops: Vec::new(),
                    transition: Some(Transition {
                        at_bar: req.at_bar,
                        length_bars: req.length_bars,
                        style: req.style,
                        tempo: TempoChange::Glide,
                        automation: Vec::new(),
                        rationale: req.rationale.clone(),
                    }),
                    rationale: None,
                },
            ],
        };
        let c = compile(&plan, &self.session.library());
        let warnings: Vec<String> = c.warnings().map(|i| i.to_string()).collect();
        let Some(timeline) = c.timeline else {
            // Paths refer to the internal two-entry plan; translate them.
            return Err(c
                .errors()
                .map(|i| {
                    i.to_string()
                        .replace("/entries/1/transition/", "")
                        .replace("/entries/1/", "next track ")
                        .replace("/entries/0/", "playing track ")
                })
                .collect());
        };
        let mut warn = warnings;
        if self.director.played.contains(&inc.id) {
            warn.push("this track was already played in this session".into());
        }
        Ok((
            Scheduled {
                id: 0,
                out_deck: main,
                in_deck,
                name: inc.display_name(),
                track: inc.id,
                at_bar: req.at_bar,
                length_bars: req.length_bars,
                style: req.style,
                by: String::new(),
                rationale: req.rationale.clone(),
                in_start_bar: plan.entries[1].start_bar,
                loading: true,
                timeline: Arc::new(timeline),
                beat_offset: start * 4.0,
            },
            warn,
        ))
    }

    /// Schedules a validated transition: loads the incoming track, arming
    /// happens once it is loaded.
    fn schedule(&mut self, mut s: Scheduled, by: &str) {
        if self.director.scheduled.is_some() {
            self.send(Command::CancelTransition);
        }
        self.director.next_id += 1;
        s.id = self.director.next_id;
        s.by = by.to_owned();
        let already = self.loaded(s.in_deck).is_some_and(|l| l.info.id == s.track);
        if !already {
            let (deck, id) = (s.in_deck, s.track.clone());
            self.load(deck, &id);
        }
        self.director.note(
            format!(
                "{by}: {} at bar {} ({:?}, {} bars)",
                s.name, s.at_bar, s.style, s.length_bars
            ),
            s.rationale.clone(),
        );
        self.director.looped_for_wait = false;
        self.director.scheduled = Some(s);
    }

    /// Runs the director (called from `poll`).
    pub(super) fn direct(&mut self) {
        let st = self.status;
        // Engine reports.
        if st.transition.completed > self.director.seen_completed {
            self.director.seen_completed = st.transition.completed;
            if let Some(s) = self.director.scheduled.take() {
                self.director.main_deck = Some(s.in_deck);
                self.director.played.push(s.track.clone());
                self.director
                    .note(format!("transition to {} done", s.name), None);
            }
        }
        if st.transition.cancelled > self.director.seen_cancelled {
            self.director.seen_cancelled = st.transition.cancelled;
            if let Some(s) = self.director.scheduled.take() {
                self.director.note(
                    format!("you took over; transition to {} cancelled", s.name),
                    None,
                );
            }
        }
        if self.session.take_cancel_request()
            && let Some(s) = self.director.scheduled.take()
        {
            self.send(Command::CancelTransition);
            self.director
                .note(format!("cancelled on request: {}", s.name), None);
        }
        // Main deck: keep it while it plays, otherwise the first playing deck.
        let main_ok = self
            .director
            .main_deck
            .is_some_and(|d| st.decks[d].playing && self.loaded(d).is_some());
        if !main_ok {
            self.director.main_deck =
                (0..self.decks).find(|&d| st.decks[d].playing && self.loaded(d).is_some());
            if let Some(d) = self.director.main_deck
                && let Some(l) = self.loaded(d)
                && !self.director.played.contains(&l.info.id)
            {
                let id = l.info.id.clone();
                self.director.played.push(id);
            }
        }
        // Arm a scheduled transition once its track is loaded.
        if let Some(s) = &self.director.scheduled
            && s.loading
            && self.loaded(s.in_deck).is_some_and(|l| l.info.id == s.track)
        {
            let t = Box::new(LiveTransition {
                id: s.id,
                out_deck: s.out_deck,
                in_deck: s.in_deck,
                timeline: s.timeline.clone(),
                beat_offset: s.beat_offset,
            });
            self.send(Command::ArmTransition(t));
            if let Some(s) = self.director.scheduled.as_mut() {
                s.loading = false;
            }
        }
        // Requests from AI clients.
        for p in self.session.take_proposals() {
            let reply = match self.build(&p.request) {
                Ok((s, warnings)) if self.ai_on || p.by != "ai" => {
                    self.schedule(s, &p.by);
                    ProposalReply {
                        ok: true,
                        errors: Vec::new(),
                        warnings,
                    }
                }
                Ok(_) => ProposalReply::error("the AI is switched off"),
                Err(errors) => ProposalReply {
                    ok: false,
                    errors,
                    warnings: Vec::new(),
                },
            };
            let _ = p.reply.send(reply);
        }
        // Autopilot when the AI is late.
        if self.ai_on
            && self.director.scheduled.is_none()
            && let Some(main) = self.director.main_deck
            && let Some(out) = self.loaded(main).map(|l| l.info.clone())
        {
            let (at, deadline) = self.default_bars(main, &out);
            let cb = self.current_bar(main);
            if cb >= deadline {
                match self.director.settings.fallback {
                    Fallback::Blend => self.autopilot(main, &out),
                    Fallback::LoopOutro => {
                        if !self.director.looped_for_wait && cb >= at - 0.25 {
                            self.send(Command::BeatLoop {
                                deck: main,
                                beats: 32.0,
                            });
                            self.director.looped_for_wait = true;
                            self.director
                                .note("autopilot: AI late, looping the outro while waiting", None);
                        }
                    }
                }
            }
        }
        self.publish_live();
    }

    fn autopilot(&mut self, main: usize, out: &TrackInfo) {
        let cb = self.current_bar(main);
        let lib = self.session.library();
        let loaded_ids: Vec<TrackId> = (0..self.decks)
            .filter_map(|d| self.loaded(d).map(|l| l.info.id.clone()))
            .collect();
        let best = lib
            .iter()
            .filter(|t| t.id != out.id && !self.director.played.contains(&t.id))
            .map(|t| {
                let c = Compatibility::new(out, t);
                let cost = c.tempo_change_pct.abs() / 2.0
                    + f64::from(c.key_distance.unwrap_or(2)) * 0.6
                    + if loaded_ids.contains(&t.id) {
                        -0.5
                    } else {
                        0.0
                    };
                (cost, t.clone())
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, t)| t);
        drop(lib);
        let Some(inc) = best else {
            return;
        };
        let min_bar = (cb + self.director.settings.frozen_bars).ceil();
        let (tr, start) = plan_transition(out, min_bar, &inc, self.director.next_id as usize, 16.0);
        let req = TransitionRequest {
            track: inc.id.0.clone(),
            at_bar: tr.at_bar.max(min_bar),
            length_bars: tr.length_bars,
            style: tr.style,
            start_bar: Some(start),
            rationale: Some(format!(
                "autopilot (AI did not deliver in time): {}",
                tr.rationale.unwrap_or_default()
            )),
        };
        match self.build(&req) {
            Ok((s, _)) => self.schedule(s, "autopilot"),
            Err(e) => {
                // Try a short echo out on the next downbeat as a last resort.
                let req = TransitionRequest {
                    at_bar: min_bar,
                    length_bars: 1.0,
                    style: TransitionStyle::EchoOut,
                    ..req
                };
                match self.build(&req) {
                    Ok((s, _)) => self.schedule(s, "autopilot"),
                    Err(e2) => {
                        if self
                            .director
                            .log
                            .last()
                            .is_none_or(|l| !l.text.starts_with("autopilot failed"))
                        {
                            self.director.note(
                                format!(
                                    "autopilot failed: {}",
                                    e.into_iter().chain(e2).collect::<Vec<_>>().join("; ")
                                ),
                                None,
                            );
                        }
                    }
                }
            }
        }
    }

    fn publish_live(&self) {
        let st = &self.status;
        let decks = (0..self.decks)
            .map(|d| {
                let l = self.loaded(d);
                LiveDeck {
                    deck: d,
                    track: l.map(|l| l.info.id.0.clone()),
                    name: l.map(|l| l.info.display_name()),
                    playing: st.decks[d].playing,
                    bar: (beats_to_bars(st.decks[d].beat) * 10.0).round() / 10.0,
                    bpm: (st.decks[d].bpm * 100.0).round() / 100.0,
                    end_bar: l.map(|l| l.info.end_bar().floor()).unwrap_or(0.0),
                    mix_out: l.map(|l| mix_out_range(&l.info)).unwrap_or((0.0, 0.0)),
                }
            })
            .collect();
        let main = self.director.main_deck;
        let (earliest, deadline, suggested) =
            match main.and_then(|m| self.loaded(m).map(|l| (m, l.info.clone()))) {
                Some((m, info)) => {
                    let (at, dl) = self.default_bars(m, &info);
                    let len = ((info.end_bar().floor() - at).min(16.0) / 4.0).floor() * 4.0;
                    (
                        Some((self.current_bar(m) + self.director.settings.frozen_bars).ceil()),
                        Some(dl),
                        (len >= 4.0).then_some((at, len)),
                    )
                }
                None => (None, None, None),
            };
        let next = self.director.scheduled.as_ref().map(|s| PendingView {
            track: s.track.0.clone(),
            name: s.name.clone(),
            at_bar: s.at_bar,
            length_bars: s.length_bars,
            style: s.style,
            by: s.by.clone(),
            state: if s.loading {
                "loading"
            } else if st.transition.state == TransitionState::Active {
                "running"
            } else {
                "armed"
            }
            .into(),
            rationale: s.rationale.clone(),
        });
        let needs_plan = self.ai_on
            && main.is_some()
            && next.is_none()
            && st.transition.state == TransitionState::Idle
            && deadline
                .zip(main)
                .is_some_and(|(d, m)| self.current_bar(m) < d);
        self.session.set_live(Some(LiveSnapshot {
            ai_on: self.ai_on,
            main_deck: main,
            master_bpm: (st.master_bpm * 100.0).round() / 100.0,
            decks,
            played: self.director.played.iter().map(|t| t.0.clone()).collect(),
            next,
            earliest_at_bar: earliest,
            deadline_bar: deadline,
            suggested_at_bar: suggested.map(|s| s.0),
            suggested_length_bars: suggested.map(|s| s.1),
            needs_plan,
        }));
    }
}
