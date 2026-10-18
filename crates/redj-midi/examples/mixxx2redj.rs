//! Converts a Mixxx controller mapping (`*.midi.xml`, GPL-2.0-or-later) into
//! a REDj profile. Standard Mixxx controls translate directly; controls
//! bound to mapping scripts are recognized by name where possible (jog,
//! shift, play, cue, sync, load, tempo) and skipped otherwise.
//!
//! `cargo run -p redj-midi --example mixxx2redj -- in.midi.xml out.json`

use std::collections::HashSet;

use redj_midi::{
    Action, Byte, Encoding, GlobalKnob, InputBinding, Knob as K, LedSource, OutputBinding, Profile,
};

fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let a = block.find(&open)? + open.len();
    let b = block[a..].find(&close)? + a;
    Some(block[a..b].trim())
}

fn has(block: &str, flag: &str) -> bool {
    block.contains(&format!("<{flag}/>")) || block.contains(&format!("<{flag} />"))
}

fn blocks<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(a) = rest.find(&open) {
        let Some(b) = rest[a..].find(&close) else {
            break;
        };
        out.push(&rest[a + open.len()..a + b]);
        rest = &rest[a + b + close.len()..];
    }
    out
}

fn byte(s: &str) -> Option<u8> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u8::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// Deck index from a Mixxx group such as `[Channel2]` or
/// `[EqualizerRack1_[Channel2]_Effect1]`.
fn deck(group: &str) -> Option<usize> {
    let i = group.find("[Channel")? + "[Channel".len();
    let n: usize = group[i..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    n.checked_sub(1)
}

fn action(group: &str, key: &str) -> Option<(Action, Encoding)> {
    use Action::*;
    let abs = Encoding::Absolute;
    if group == "[Master]" || group == "[Mixer]" {
        return match key {
            "crossfader" => Some((
                Global {
                    knob: GlobalKnob::Crossfader,
                },
                abs,
            )),
            "headMix" | "head_mix" => Some((
                Global {
                    knob: GlobalKnob::CueMix,
                },
                abs,
            )),
            "gain" => Some((
                Global {
                    knob: GlobalKnob::MasterLevel,
                },
                abs,
            )),
            "headGain" | "head_gain" => Some((
                Global {
                    knob: GlobalKnob::PhonesLevel,
                },
                abs,
            )),
            _ => None,
        };
    }
    if group.starts_with("[Library]") || group.starts_with("[Playlist]") {
        return match key {
            "MoveVertical" | "SelectTrackKnob" => Some((Browse, Encoding::RelativeTwos)),
            "GoToItem" | "MoveFocusForward" => Some((BrowsePress, abs)),
            _ => None,
        };
    }
    let d = deck(group)?;
    if group.starts_with("[EqualizerRack1_") {
        let knob = match key {
            "parameter1" => K::EqLow,
            "parameter2" => K::EqMid,
            "parameter3" => K::EqHigh,
            _ => return None,
        };
        return Some((Knob { deck: d, knob }, abs));
    }
    if group.starts_with("[QuickEffectRack1_") {
        return (key == "super1").then_some((
            Knob {
                deck: d,
                knob: K::Filter,
            },
            abs,
        ));
    }
    if let Some(script) = key.split('.').nth(1) {
        // Script-bound control: guess from the function name.
        let f = script.to_ascii_lowercase();
        let jog = f.contains("jog")
            || f.contains("wheel")
            || f.contains("platter")
            || f.contains("scratch");
        return if jog && f.contains("touch") {
            Some((JogTouch { deck: d }, abs))
        } else if jog && (f.contains("search") || f.contains("seek")) {
            Some((JogSearch { deck: d }, Encoding::Relative64))
        } else if jog {
            Some((Jog { deck: d }, Encoding::Relative64))
        } else if f.contains("shift") {
            Some((Shift { deck: d }, abs))
        } else if (f.contains("tempo") || f.contains("pitch") || f.contains("rate"))
            && (f.contains("slider")
                || f.contains("fader")
                || f.contains("msb")
                || f.contains("lsb"))
        {
            Some((
                Knob {
                    deck: d,
                    knob: K::Tempo,
                },
                abs,
            ))
        } else if f == "play" || f.starts_with("playbutton") || f.starts_with("playpause") {
            Some((PlayToggle { deck: d }, abs))
        } else if f.starts_with("sync") {
            Some((Sync { deck: d }, abs))
        } else if f.starts_with("load") {
            Some((Load { deck: d }, abs))
        } else {
            None
        };
    }
    let num = |prefix: &str, suffix: &str| -> Option<f64> {
        key.strip_prefix(prefix)?.strip_suffix(suffix)?.parse().ok()
    };
    if let Some(n) = num("hotcue_", "_activate") {
        return Some((
            HotCue {
                deck: d,
                index: n as usize - 1,
            },
            abs,
        ));
    }
    if let Some(n) = num("hotcue_", "_clear") {
        return Some((
            HotCueClear {
                deck: d,
                index: n as usize - 1,
            },
            abs,
        ));
    }
    if let Some(b) = num("beatloop_", "_toggle").or_else(|| num("beatloop_", "_activate")) {
        return Some((BeatLoop { deck: d, beats: b }, abs));
    }
    if let Some(b) = num("beatjump_", "_forward") {
        return Some((BeatJump { deck: d, beats: b }, abs));
    }
    if let Some(b) = num("beatjump_", "_backward") {
        return Some((BeatJump { deck: d, beats: -b }, abs));
    }
    let a = match key {
        "play" | "play_stutter" => PlayToggle { deck: d },
        "cue_default" | "cue_gotoandplay" => Cue { deck: d },
        "start" | "start_stop" | "start_play" => ToStart { deck: d },
        "sync_enabled" | "beatsync" | "beatsync_tempo" => Sync { deck: d },
        "sync_leader" | "sync_master" => SetMaster { deck: d },
        "pfl" => Pfl { deck: d },
        "loop_in" => LoopIn { deck: d },
        "loop_out" => LoopOut { deck: d },
        "reloop_toggle" | "reloop_exit" => Reloop { deck: d },
        "loop_halve" => LoopHalve { deck: d },
        "loop_double" => LoopDouble { deck: d },
        "keylock" => KeylockToggle { deck: d },
        "quantize" => QuantizeToggle { deck: d },
        "LoadSelectedTrack" => Load { deck: d },
        "volume" => Knob {
            deck: d,
            knob: K::Volume,
        },
        "pregain" => Knob {
            deck: d,
            knob: K::Trim,
        },
        "rate" => Knob {
            deck: d,
            knob: K::Tempo,
        },
        "filterLow" => Knob {
            deck: d,
            knob: K::EqLow,
        },
        "filterMid" => Knob {
            deck: d,
            knob: K::EqMid,
        },
        "filterHigh" => Knob {
            deck: d,
            knob: K::EqHigh,
        },
        _ => return None,
    };
    Some((a, abs))
}

fn led(group: &str, key: &str) -> Option<LedSource> {
    let d = deck(group)?;
    let num = |prefix: &str, suffix: &str| -> Option<f64> {
        key.strip_prefix(prefix)?.strip_suffix(suffix)?.parse().ok()
    };
    if let Some(n) = num("hotcue_", "_enabled").or_else(|| num("hotcue_", "_status")) {
        return Some(LedSource::HotCueSet {
            deck: d,
            index: n as usize - 1,
        });
    }
    if let Some(b) = num("beatloop_", "_enabled") {
        return Some(LedSource::BeatLoop { deck: d, beats: b });
    }
    Some(match key {
        "play_indicator" | "play" => LedSource::Playing { deck: d },
        "cue_indicator" => LedSource::CueIndicator { deck: d },
        "pfl" => LedSource::Pfl { deck: d },
        "sync_enabled" => LedSource::Sync { deck: d },
        "loop_enabled" => LedSource::LoopActive { deck: d },
        "track_loaded" => LedSource::Loaded { deck: d },
        "VuMeter" | "vu_meter" => LedSource::Vu { deck: d },
        _ => return None,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: mixxx2redj <mapping.midi.xml> <profile.json> [port match]");
        std::process::exit(2);
    }
    let xml = std::fs::read_to_string(&args[1]).expect("read mapping");
    let name = tag(&xml, "name").unwrap_or("Unknown controller").to_owned();
    let port = args.get(3).cloned().unwrap_or_else(|| {
        // "Pioneer DDJ-400" -> "DDJ-400": the model name without the vendor.
        name.split_whitespace()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" ")
    });

    struct Raw {
        status: u8,
        midino: u8,
        action: Action,
        encoding: Encoding,
        invert: bool,
        msb: bool,
        lsb: bool,
    }
    let mut raws = Vec::new();
    let mut skipped = 0;
    for b in blocks(&xml, "control") {
        let (Some(group), Some(key), Some(status), Some(midino)) = (
            tag(b, "group"),
            tag(b, "key"),
            tag(b, "status").and_then(byte),
            tag(b, "midino").and_then(byte),
        ) else {
            continue;
        };
        match action(group, key) {
            Some((action, mut encoding)) => {
                if has(b, "selectknob") || has(b, "diff") {
                    encoding = Encoding::RelativeTwos;
                }
                raws.push(Raw {
                    status,
                    midino,
                    action,
                    encoding,
                    invert: has(b, "invert"),
                    msb: has(b, "fourteen-bit-msb"),
                    lsb: has(b, "fourteen-bit-lsb"),
                });
            }
            None => skipped += 1,
        }
    }
    // Pair 14-bit controls: explicit Mixxx flags, or the Pioneer convention
    // of the same continuous control on CC n and n + 0x20.
    let mut inputs: Vec<InputBinding> = Vec::new();
    let mut used = HashSet::new();
    for (i, r) in raws.iter().enumerate() {
        if used.contains(&i) || r.lsb {
            continue;
        }
        let mut lsb = None;
        if r.action.is_continuous() {
            if let Some((j, l)) = raws.iter().enumerate().find(|(j, l)| {
                *j != i
                    && l.status == r.status
                    && l.action == r.action
                    && (l.lsb || (!r.msb && l.midino == r.midino.wrapping_add(0x20)))
            }) {
                lsb = Some(Byte(l.midino));
                used.insert(j);
            }
        }
        let key = (r.status, r.midino);
        if !used.insert(i) || inputs.iter().any(|b| (b.status.0, b.data1.0) == key) {
            continue;
        }
        inputs.push(InputBinding {
            status: Byte(r.status),
            data1: Byte(r.midino),
            lsb,
            invert: r.invert,
            encoding: r.encoding,
            action: r.action,
        });
    }
    let mut outputs: Vec<OutputBinding> = Vec::new();
    for b in blocks(&xml, "output") {
        let (Some(group), Some(key), Some(status), Some(midino)) = (
            tag(b, "group"),
            tag(b, "key"),
            tag(b, "status").and_then(byte),
            tag(b, "midino").and_then(byte),
        ) else {
            continue;
        };
        if let Some(source) = led(group, key)
            && !outputs
                .iter()
                .any(|o| (o.status.0, o.data1.0) == (status, midino))
        {
            outputs.push(OutputBinding {
                status: Byte(status),
                data1: Byte(midino),
                source,
                on: tag(b, "on").and_then(byte).unwrap_or(0x7F),
                off: tag(b, "off").and_then(byte).unwrap_or(0),
            });
        }
    }
    let decks = inputs
        .iter()
        .filter_map(|b| match b.action {
            Action::PlayToggle { deck } | Action::Knob { deck, .. } => Some(deck + 1),
            _ => None,
        })
        .max()
        .unwrap_or(2)
        .max(2);
    let file = std::path::Path::new(&args[1])
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let profile = Profile {
        name: name.clone(),
        port_match: vec![port],
        decks,
        source: format!(
            "Generated by mixxx2redj from the Mixxx mapping {file} (GPL-2.0-or-later). Script-only features are not translated. Untested on hardware."
        ),
        inputs,
        outputs,
        init: Vec::new(),
    };
    std::fs::write(
        &args[2],
        serde_json::to_string_pretty(&profile).expect("serialize"),
    )
    .expect("write profile");
    eprintln!(
        "{name}: {} inputs, {} outputs, {} controls skipped (script-only)",
        profile.inputs.len(),
        profile.outputs.len(),
        skipped
    );
}
