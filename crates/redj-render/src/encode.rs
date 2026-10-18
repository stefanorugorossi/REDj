//! File writers: 24-bit WAV, MP3 (LAME) and tags.

use std::path::Path;

use crate::{RenderError, TracklistEntry};

fn write_err(path: &Path, e: impl std::fmt::Display) -> RenderError {
    RenderError::Write {
        path: path.display().to_string(),
        msg: e.to_string(),
    }
}

/// Writes interleaved stereo as 24-bit PCM WAV.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), RenderError> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| write_err(path, e))?;
    const MAX: f32 = 8_388_607.0;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * MAX).round() as i32;
        w.write_sample(v).map_err(|e| write_err(path, e))?;
    }
    w.finalize().map_err(|e| write_err(path, e))
}

/// Writes interleaved stereo as 320 kbps CBR MP3.
#[cfg(feature = "mp3")]
pub fn write_mp3(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), RenderError> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, Quality};
    let mut b = Builder::new().ok_or_else(|| write_err(path, "cannot create LAME encoder"))?;
    b.set_num_channels(2).map_err(|e| write_err(path, e))?;
    b.set_sample_rate(sample_rate)
        .map_err(|e| write_err(path, e))?;
    b.set_brate(Bitrate::Kbps320)
        .map_err(|e| write_err(path, e))?;
    b.set_quality(Quality::Best)
        .map_err(|e| write_err(path, e))?;
    let mut enc = b.build().map_err(|e| write_err(path, e))?;
    let mut out: Vec<u8> = Vec::new();
    for chunk in samples.chunks(2 * 8192) {
        out.reserve(mp3lame_encoder::max_required_buffer_size(chunk.len() / 2));
        enc.encode_to_vec(InterleavedPcm(chunk), &mut out)
            .map_err(|e| write_err(path, e))?;
    }
    out.reserve(8192);
    enc.flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|e| write_err(path, e))?;
    std::fs::write(path, out).map_err(|e| write_err(path, e))
}

#[cfg(not(feature = "mp3"))]
pub fn write_mp3(path: &Path, _samples: &[f32], _sample_rate: u32) -> Result<(), RenderError> {
    Err(write_err(
        path,
        "this build has no MP3 support (feature `mp3`)",
    ))
}

/// Writes title, artist and the tracklist (as a comment) into a rendered
/// file's tags. Tagging failures are logged, not fatal.
pub fn tag_file(path: &Path, title: &str, list: &[TracklistEntry]) {
    use lofty::config::WriteOptions;
    use lofty::prelude::*;
    use lofty::tag::Tag;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut file = lofty::read_from_path(path)?;
        let tag_type = file.primary_tag_type();
        if file.primary_tag().is_none() {
            file.insert_tag(Tag::new(tag_type));
        }
        let tag = file.primary_tag_mut().expect("tag inserted above");
        tag.set_title(title.to_owned());
        tag.set_artist("REDj".to_owned());
        let comment = list
            .iter()
            .map(|t| {
                let secs = t.start_sec.max(0.0) as u64;
                let name = match &t.artist {
                    Some(a) => format!("{a} - {}", t.title),
                    None => t.title.clone(),
                };
                format!("{:02}:{:02} {name}", secs / 60, secs % 60)
            })
            .collect::<Vec<_>>()
            .join("\n");
        tag.set_comment(comment);
        tag.save_to_path(path, WriteOptions::default())?;
        Ok(())
    })();
    if let Err(e) = result {
        tracing::warn!("cannot tag {}: {e}", path.display());
    }
}
