use std::sync::Arc;

use redj_dsp::SincTable;
use redj_plan::{Param, Timeline};

use crate::audio::TrackAudio;
use crate::reader::DeckReader;
use crate::strip::{Strip, StripParams};

const SLOTS: usize = 4;

#[derive(Debug)]
struct Slot {
    strip: Strip,
    reader: DeckReader,
    clip: Option<usize>,
}

/// Plays a compiled [`Timeline`]: every output frame is mapped to a master
/// beat, every active clip's playhead and automation are evaluated at that
/// beat, so loops, jumps and automation steps land on their exact sample
/// regardless of block size.
#[derive(Debug)]
pub struct PlanPlayer {
    timeline: Arc<Timeline>,
    /// Audio per clip (same order as `timeline.clips`).
    sources: Vec<Arc<TrackAudio>>,
    slots: Vec<Slot>,
    sr: f64,
    pos: u64,
    end_frame: u64,
    /// Indices of clips intersecting the current block (preallocated).
    active: Vec<usize>,
}

impl PlanPlayer {
    /// `sources[i]` is the audio of `timeline.clips[i]`, at `sample_rate`.
    pub fn new(timeline: Arc<Timeline>, sources: Vec<Arc<TrackAudio>>, sample_rate: u32) -> Self {
        assert_eq!(timeline.clips.len(), sources.len(), "one source per clip");
        let sinc = Arc::new(SincTable::default());
        let slots = (0..SLOTS)
            .map(|_| {
                let mut reader = DeckReader::new(sample_rate, sinc.clone());
                reader.set_keylock(timeline.keylock);
                Slot {
                    strip: Strip::new(sample_rate),
                    reader,
                    clip: None,
                }
            })
            .collect();
        let sr = f64::from(sample_rate);
        let end_frame = (timeline.duration_sec() * sr).ceil() as u64;
        let n = timeline.clips.len();
        Self {
            timeline,
            sources,
            slots,
            sr,
            pos: 0,
            end_frame,
            active: Vec::with_capacity(n),
        }
    }

    pub fn timeline(&self) -> &Arc<Timeline> {
        &self.timeline
    }

    /// Current output position in frames.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Frame at which the set ends.
    pub fn end_frame(&self) -> u64 {
        self.end_frame
    }

    pub fn is_finished(&self) -> bool {
        self.pos >= self.end_frame
    }

    /// Master beat at the current position.
    pub fn master_beat(&self) -> f64 {
        self.timeline.tempo.sec_to_beat(self.pos as f64 / self.sr)
    }

    /// Jumps to an output frame (decks restart their state).
    pub fn seek(&mut self, frame: u64) {
        self.pos = frame;
        for s in &mut self.slots {
            s.clip = None;
        }
    }

    fn params(&self, ci: usize, beat: f64) -> StripParams {
        let c = &self.timeline.clips[ci];
        let ev = |p: Param| c.lane(p).eval(beat) as f32;
        let bpm = self.timeline.tempo.bpm_at(beat);
        StripParams {
            volume: ev(Param::Volume),
            trim_db: c.trim_db as f32,
            eq_db: [ev(Param::EqLow), ev(Param::EqMid), ev(Param::EqHigh)],
            filter: ev(Param::Filter),
            echo_send: ev(Param::Echo),
            echo_delay_samples: (c.echo_beats * 60.0 / bpm * self.sr) as f32,
            echo_feedback: c.echo_feedback as f32,
        }
    }

    /// Renders the next `out.len() / 2` stereo frames (overwrites `out`).
    pub fn process(&mut self, out: &mut [f32]) {
        out.iter_mut().for_each(|s| *s = 0.0);
        let frames = out.len() / 2;
        if frames == 0 {
            return;
        }
        let tempo = &self.timeline.tempo;
        let b0 = tempo.sec_to_beat(self.pos as f64 / self.sr);
        let b1 = tempo.sec_to_beat((self.pos + frames as u64) as f64 / self.sr);
        self.active.clear();
        for (i, c) in self.timeline.clips.iter().enumerate() {
            if c.start_beat <= b1 && c.end_beat > b0 {
                self.active.push(i);
            }
        }
        for k in 0..frames {
            let beat = tempo.sec_to_beat((self.pos + k as u64) as f64 / self.sr);
            for a in 0..self.active.len() {
                let ci = self.active[a];
                let clip = &self.timeline.clips[ci];
                if beat < clip.start_beat || beat >= clip.end_beat {
                    continue;
                }
                let params = self.params(ci, beat);
                let slot = &mut self.slots[clip.slot % SLOTS];
                if slot.clip != Some(ci) {
                    slot.clip = Some(ci);
                    slot.strip.reset(&params);
                    slot.reader.reset();
                }
                let src = &*self.sources[ci];
                let (l, r) = match clip.track_beat(beat) {
                    Some(tb) => slot.reader.read(src, src.beat_to_frame(tb)),
                    None => (0.0, 0.0),
                };
                let o = slot.strip.process(l, r, &params);
                out[2 * k] += o.main[0];
                out[2 * k + 1] += o.main[1];
            }
        }
        self.pos += frames as u64;
    }
}
