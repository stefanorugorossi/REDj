use std::sync::OnceLock;

use redj_core::{BeatGrid, TrackId};
use redj_dsp::Source;

/// Frames per storage chunk.
pub const CHUNK_FRAMES: usize = 1 << 16;

/// Decoded track audio at the engine sample rate, interleaved stereo, plus
/// its beat grid.
///
/// Samples live in fixed-size chunks that a loader thread fills one by one
/// while the audio thread may already be playing: reading a chunk is a
/// single atomic load (`OnceLock::get`), never a lock, and chunks that are
/// not decoded yet read as silence.
#[derive(Debug)]
pub struct TrackAudio {
    pub id: TrackId,
    pub sample_rate: u32,
    pub grid: BeatGrid,
    frames: usize,
    chunks: Vec<OnceLock<Box<[f32]>>>,
}

impl TrackAudio {
    /// Empty audio of a known length, to be filled with [`Self::fill_chunk`].
    pub fn new_loading(id: TrackId, sample_rate: u32, grid: BeatGrid, frames: usize) -> Self {
        let n = frames.div_ceil(CHUNK_FRAMES);
        Self {
            id,
            sample_rate,
            grid,
            frames,
            chunks: (0..n).map(|_| OnceLock::new()).collect(),
        }
    }

    /// Fully loaded audio from an interleaved stereo buffer.
    pub fn new(id: TrackId, sample_rate: u32, grid: BeatGrid, samples: Vec<f32>) -> Self {
        let frames = samples.len() / 2;
        let a = Self::new_loading(id, sample_rate, grid, frames);
        for (i, c) in samples.chunks(CHUNK_FRAMES * 2).enumerate() {
            a.fill_chunk(i, c.into());
        }
        a
    }

    /// Publishes chunk `i` (interleaved stereo, `CHUNK_FRAMES` frames except
    /// for the last chunk). Later calls for the same chunk are ignored.
    pub fn fill_chunk(&self, i: usize, data: Box<[f32]>) {
        if let Some(c) = self.chunks.get(i) {
            let _ = c.set(data);
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Fraction of chunks decoded so far.
    pub fn loaded_fraction(&self) -> f32 {
        if self.chunks.is_empty() {
            return 1.0;
        }
        self.chunks.iter().filter(|c| c.get().is_some()).count() as f32 / self.chunks.len() as f32
    }

    /// Source frame position of a (fractional) track beat.
    #[inline]
    pub fn beat_to_frame(&self, beat: f64) -> f64 {
        self.grid.beat_to_sec(beat) * f64::from(self.sample_rate)
    }

    /// Track beat at a source frame position.
    #[inline]
    pub fn frame_to_beat(&self, frame: f64) -> f64 {
        self.grid.sec_to_beat(frame / f64::from(self.sample_rate))
    }
}

impl Source for TrackAudio {
    #[inline]
    fn frame(&self, i: i64) -> [f32; 2] {
        if i < 0 || i as usize >= self.frames {
            return [0.0; 2];
        }
        let i = i as usize;
        match self.chunks[i / CHUNK_FRAMES].get() {
            Some(c) => {
                let k = (i % CHUNK_FRAMES) * 2;
                [c[k], c[k + 1]]
            }
            None => [0.0; 2],
        }
    }

    #[inline]
    fn contiguous(&self, start: i64, len: usize) -> Option<&[f32]> {
        if start < 0 {
            return None;
        }
        let start = start as usize;
        let ci = start / CHUNK_FRAMES;
        let off = start % CHUNK_FRAMES;
        if off + len > CHUNK_FRAMES || start + len > self.frames {
            return None;
        }
        self.chunks.get(ci)?.get()?.get(off * 2..(off + len) * 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_reads_match_buffer() {
        let n = CHUNK_FRAMES * 2 + 100;
        let samples: Vec<f32> = (0..n * 2).map(|i| i as f32).collect();
        let a = TrackAudio::new(
            "t".into(),
            44_100,
            BeatGrid::constant(120.0, 0.0),
            samples.clone(),
        );
        assert_eq!(a.frames(), n);
        for i in [0, 1, CHUNK_FRAMES - 1, CHUNK_FRAMES, n - 1] {
            assert_eq!(a.frame(i as i64), [samples[i * 2], samples[i * 2 + 1]]);
        }
        assert_eq!(a.frame(n as i64), [0.0; 2]);
        assert!(a.contiguous(CHUNK_FRAMES as i64 - 4, 8).is_none());
        assert_eq!(a.contiguous(10, 2).unwrap(), &samples[20..24]);
    }

    #[test]
    fn unloaded_chunks_are_silent() {
        let a = TrackAudio::new_loading(
            "t".into(),
            44_100,
            BeatGrid::constant(120.0, 0.0),
            CHUNK_FRAMES * 2,
        );
        assert_eq!(a.frame(5), [0.0; 2]);
        a.fill_chunk(1, vec![1.0; CHUNK_FRAMES * 2].into());
        assert_eq!(a.frame(5), [0.0; 2]);
        assert_eq!(a.frame(CHUNK_FRAMES as i64 + 5), [1.0; 2]);
        assert_eq!(a.loaded_fraction(), 0.5);
    }
}
