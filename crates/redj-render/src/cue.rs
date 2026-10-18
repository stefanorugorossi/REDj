//! Tracklist and CUE sheet.

use redj_core::{Library, TrackId};
use redj_plan::Timeline;

/// One line of the tracklist.
#[derive(Debug, Clone, PartialEq)]
pub struct TracklistEntry {
    pub track: TrackId,
    pub artist: Option<String>,
    pub title: String,
    /// Time where the track becomes the main track.
    pub start_sec: f64,
}

/// Tracklist of a compiled set.
pub fn tracklist(timeline: &Timeline, lib: &Library) -> Vec<TracklistEntry> {
    timeline
        .markers
        .iter()
        .map(|m| {
            let info = lib.get(&m.track);
            TracklistEntry {
                track: m.track.clone(),
                artist: info.and_then(|t| t.tags.artist.clone()),
                title: info
                    .map(|t| t.tags.title.clone().unwrap_or_else(|| t.display_name()))
                    .unwrap_or_else(|| m.track.0.clone()),
                start_sec: timeline.tempo.beat_to_sec(m.main_beat),
            }
        })
        .collect()
}

fn quote(s: &str) -> String {
    s.replace('"', "'")
}

/// `mm:ss:ff` with 75 frames per second.
fn cue_time(sec: f64) -> String {
    let frames = (sec.max(0.0) * 75.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}",
        frames / 75 / 60,
        frames / 75 % 60,
        frames % 75
    )
}

/// CUE sheet text for an audio file.
pub fn cue_sheet(title: &str, file_name: &str, list: &[TracklistEntry]) -> String {
    let mut s = format!(
        "REM GENERATOR \"REDj {}\"\nPERFORMER \"REDj\"\nTITLE \"{}\"\nFILE \"{}\" WAVE\n",
        env!("CARGO_PKG_VERSION"),
        quote(title),
        quote(file_name)
    );
    for (i, t) in list.iter().enumerate() {
        s.push_str(&format!(
            "  TRACK {:02} AUDIO\n    TITLE \"{}\"\n",
            i + 1,
            quote(&t.title)
        ));
        if let Some(a) = &t.artist {
            s.push_str(&format!("    PERFORMER \"{}\"\n", quote(a)));
        }
        s.push_str(&format!("    INDEX 01 {}\n", cue_time(t.start_sec)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_format() {
        assert_eq!(cue_time(0.0), "00:00:00");
        assert_eq!(cue_time(61.5), "01:01:38");
        let list = vec![TracklistEntry {
            track: TrackId("x".into()),
            artist: Some("A \"B\"".into()),
            title: "T".into(),
            start_sec: 0.0,
        }];
        let s = cue_sheet("Mix", "mix.wav", &list);
        assert!(s.contains("FILE \"mix.wav\" WAVE"));
        assert!(s.contains("PERFORMER \"A 'B'\""));
        assert!(s.contains("INDEX 01 00:00:00"));
    }
}
