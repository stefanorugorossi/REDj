//! The session: library, accepted plan, and the operations exposed to AI
//! clients and the UI.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, RwLock, RwLockReadGuard};

use redj_core::{Library, TrackId};
use redj_plan::{Issue, MixPlan, compile};
use redj_render::{RenderOptions, RenderReport, render_plan, tracklist};
use serde::Serialize;

use crate::autoplan::{AutoPlanOptions, auto_plan};
use crate::live::{LiveSnapshot, Proposal, ProposalReply, TransitionRequest};
use crate::summary::{Compatibility, TrackDetail, TrackSummary};

/// Result of checking or submitting a plan, shaped for a language model.
#[derive(Debug, Clone, Serialize)]
pub struct PlanReport {
    pub ok: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<Issue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Issue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<f64>,
    /// `"mm:ss name"` per track, where it becomes the main track.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tracklist: Vec<String>,
}

/// Something that happened in the session (shown in the UI log).
#[derive(Debug, Clone, Serialize)]
pub struct SessionEvent {
    pub what: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Default)]
pub struct Session {
    library: RwLock<Library>,
    accepted: Mutex<Option<MixPlan>>,
    output_dir: Option<PathBuf>,
    events: Mutex<Vec<SessionEvent>>,
    live: Mutex<Option<LiveSnapshot>>,
    proposals: Mutex<Vec<Proposal>>,
    cancel_requested: AtomicBool,
    /// True while the built-in AI director is waiting for the model.
    ai_busy: AtomicBool,
}

impl Session {
    pub fn new(library: Library) -> Self {
        Self {
            library: RwLock::new(library),
            ..Self::default()
        }
    }

    /// Directory where `render` writes files (rendering is disabled
    /// without one).
    pub fn with_output_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.output_dir = Some(dir.into());
        self
    }

    pub fn output_dir(&self) -> Option<&Path> {
        self.output_dir.as_deref()
    }

    pub fn library(&self) -> RwLockReadGuard<'_, Library> {
        self.library.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_library(&self, lib: Library) {
        *self.library.write().unwrap_or_else(|e| e.into_inner()) = lib;
    }

    pub fn log(&self, what: impl Into<String>, rationale: Option<String>) {
        let ev = SessionEvent {
            what: what.into(),
            rationale,
        };
        tracing::info!("{}", ev.what);
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ev);
    }

    pub fn events(&self) -> Vec<SessionEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn summaries(&self) -> Vec<TrackSummary> {
        self.library().iter().map(TrackSummary::new).collect()
    }

    pub fn detail(&self, id: &str) -> Option<TrackDetail> {
        self.library().resolve(id).map(TrackDetail::new)
    }

    /// Pairwise compatibility of consecutive ids, or of all ordered pairs
    /// when `all_pairs` is set.
    pub fn compatibility(
        &self,
        ids: &[String],
        all_pairs: bool,
    ) -> Result<Vec<Compatibility>, String> {
        let lib = self.library();
        let tracks = ids
            .iter()
            .map(|id| {
                lib.resolve(id)
                    .ok_or_else(|| format!("unknown track id {id:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        if all_pairs {
            for a in &tracks {
                for b in &tracks {
                    if a.id != b.id {
                        out.push(Compatibility::new(a, b));
                    }
                }
            }
        } else {
            for w in tracks.windows(2) {
                out.push(Compatibility::new(w[0], w[1]));
            }
        }
        Ok(out)
    }

    /// Validates a plan without accepting it.
    pub fn check(&self, plan: &MixPlan) -> PlanReport {
        let lib = self.library();
        let c = compile(plan, &lib);
        let errors: Vec<Issue> = c.errors().cloned().collect();
        let warnings: Vec<Issue> = c.warnings().cloned().collect();
        let (minutes, list) = match &c.timeline {
            Some(t) => {
                let list = tracklist(t, &lib)
                    .into_iter()
                    .map(|e| {
                        let s = e.start_sec.max(0.0) as u64;
                        format!("{:02}:{:02} {}", s / 60, s % 60, e.title)
                    })
                    .collect();
                (Some((t.duration_sec() / 6.0).round() / 10.0), list)
            }
            None => (None, Vec::new()),
        };
        PlanReport {
            ok: c.is_ok(),
            errors,
            warnings,
            minutes,
            tracklist: list,
        }
    }

    /// Validates a plan and, if it has no errors, makes it the accepted
    /// plan of the session.
    pub fn submit(&self, plan: MixPlan) -> PlanReport {
        let report = self.check(&plan);
        if report.ok {
            self.log(
                format!(
                    "plan accepted: {} tracks, {:.1} min",
                    plan.entries.len(),
                    report.minutes.unwrap_or(0.0)
                ),
                plan.entries.iter().find_map(|e| e.rationale.clone()),
            );
            *self.accepted.lock().unwrap_or_else(|e| e.into_inner()) = Some(plan);
        }
        report
    }

    pub fn accepted_plan(&self) -> Option<MixPlan> {
        self.accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Deterministic plan from the built-in planner.
    pub fn suggest(&self, opts: &AutoPlanOptions) -> MixPlan {
        auto_plan(&self.library(), opts)
    }

    /// Renders the accepted plan into the output directory.
    pub fn render_accepted(&self, file_name: &str, mp3: bool) -> Result<RenderReport, String> {
        let dir = self
            .output_dir
            .as_ref()
            .ok_or("rendering is not enabled in this session")?;
        let plan = self
            .accepted_plan()
            .ok_or("no plan has been accepted yet; call submit_plan first")?;
        let name = Path::new(file_name)
            .file_name()
            .ok_or("invalid file name")?
            .to_string_lossy()
            .into_owned();
        let out = dir.join(Path::new(&name).with_extension("wav"));
        let lib = self.library().clone();
        let opts = RenderOptions {
            mp3,
            ..RenderOptions::default()
        };
        let rep = render_plan(&plan, &lib, &out, &opts).map_err(|e| e.to_string())?;
        self.log(format!("rendered {}", rep.wav.display()), None);
        Ok(rep)
    }

    pub fn has_track(&self, id: &TrackId) -> bool {
        self.library().get(id).is_some()
    }

    /// Publishes the live state (called by the console every frame).
    pub fn set_live(&self, s: Option<LiveSnapshot>) {
        *self.live.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    /// Current live state, `None` when no live console is running.
    pub fn live(&self) -> Option<LiveSnapshot> {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Queues a transition request for the console; the receiver gets the
    /// validation result.
    pub fn propose(&self, request: TransitionRequest, by: &str) -> Receiver<ProposalReply> {
        let (p, rx) = Proposal::new(request, by);
        if self.live().is_none() {
            let _ = p
                .reply
                .send(ProposalReply::error("no live console is running"));
        } else {
            self.proposals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(p);
        }
        rx
    }

    /// Requests waiting for the console.
    pub fn take_proposals(&self) -> Vec<Proposal> {
        std::mem::take(&mut *self.proposals.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Asks the console to cancel the next transition.
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::Relaxed);
    }

    pub fn take_cancel_request(&self) -> bool {
        self.cancel_requested.swap(false, Ordering::Relaxed)
    }

    pub fn set_ai_busy(&self, busy: bool) {
        self.ai_busy.store(busy, Ordering::Relaxed);
    }

    pub fn ai_busy(&self) -> bool {
        self.ai_busy.load(Ordering::Relaxed)
    }
}
