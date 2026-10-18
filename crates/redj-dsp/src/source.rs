//! Random access to stereo audio for the interpolating readers.

/// Interleaved stereo audio addressed by frame index. Frames outside the
/// valid range read as silence.
pub trait Source {
    /// One stereo frame.
    fn frame(&self, i: i64) -> [f32; 2];

    /// `len` consecutive frames starting at `start` as one interleaved
    /// slice, if they are stored contiguously (fast path).
    fn contiguous(&self, start: i64, len: usize) -> Option<&[f32]>;
}

impl Source for [f32] {
    #[inline]
    fn frame(&self, i: i64) -> [f32; 2] {
        if i < 0 || (i as usize) * 2 + 1 >= self.len() {
            [0.0; 2]
        } else {
            let k = i as usize * 2;
            [self[k], self[k + 1]]
        }
    }

    #[inline]
    fn contiguous(&self, start: i64, len: usize) -> Option<&[f32]> {
        if start < 0 {
            return None;
        }
        let a = start as usize * 2;
        self.get(a..a + len * 2)
    }
}

impl Source for Vec<f32> {
    #[inline]
    fn frame(&self, i: i64) -> [f32; 2] {
        self.as_slice().frame(i)
    }

    #[inline]
    fn contiguous(&self, start: i64, len: usize) -> Option<&[f32]> {
        self.as_slice().contiguous(start, len)
    }
}
