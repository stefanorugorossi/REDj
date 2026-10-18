//! Custom widgets: knobs, meters, waveforms.

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2};
use redj_control::waveform::{BIN_FRAMES, Waveform};
use redj_engine::TrackAudio;

pub const ACCENT: Color32 = Color32::from_rgb(230, 40, 50);
pub const LOW: Color32 = Color32::from_rgb(40, 110, 255);
pub const MID: Color32 = Color32::from_rgb(255, 150, 40);
pub const HIGH: Color32 = Color32::from_rgb(235, 235, 235);
pub const AI: Color32 = Color32::from_rgb(170, 110, 255);

/// A rotary knob: drag vertically, double-click to reset. Returns true
/// when the value changed.
pub fn knob(ui: &mut Ui, value: &mut f32, default: f32, label: &str, size: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size + 14.0), Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() {
        let d = -resp.drag_delta().y / 150.0;
        let nv = (*value + d).clamp(0.0, 1.0);
        changed = nv != *value;
        *value = nv;
    }
    if resp.double_clicked() {
        changed = *value != default;
        *value = default;
    }
    let c = pos2(rect.center().x, rect.top() + size / 2.0);
    let r = size / 2.0 - 2.0;
    let p = ui.painter();
    p.circle_filled(c, r, Color32::from_gray(45));
    p.circle_stroke(c, r, Stroke::new(1.0, Color32::from_gray(90)));
    let ang = |v: f32| std::f32::consts::PI * (0.75 + 1.5 * v);
    // Arc from the default position to the value.
    let steps = 24;
    let (a0, a1) = (ang(default.min(*value)), ang(default.max(*value)));
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * (r + 1.0)
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(2.5, ACCENT)));
    let a = ang(*value);
    p.line_segment(
        [c, c + vec2(a.cos(), a.sin()) * r],
        Stroke::new(2.0, Color32::WHITE),
    );
    p.text(
        pos2(rect.center().x, rect.bottom() - 6.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.0),
        Color32::from_gray(180),
    );
    resp.on_hover_text(format!("{label}: {:.0}%", *value * 100.0));
    changed
}

/// Vertical level meter (peak 0..1).
pub fn vu(ui: &mut Ui, level: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, height), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 1.0, Color32::from_gray(25));
    let db = 20.0 * level.max(1e-5).log10();
    let frac = ((db + 48.0) / 48.0).clamp(0.0, 1.0);
    let top = rect.bottom() - rect.height() * frac;
    let color = if db > -1.5 {
        ACCENT
    } else if db > -9.0 {
        Color32::from_rgb(240, 200, 40)
    } else {
        Color32::from_rgb(60, 200, 90)
    };
    p.rect_filled(
        Rect::from_min_max(pos2(rect.left(), top), rect.max),
        1.0,
        color,
    );
}

fn bin_color_bars(
    p: &egui::Painter,
    x: f32,
    mid_y: f32,
    half: f32,
    b: redj_control::waveform::WaveBin,
    w: f32,
) {
    let s = |v: u8| f32::from(v) / 255.0 * half;
    for (h, c) in [(s(b.low), LOW), (s(b.mid), MID), (s(b.high), HIGH)] {
        if h > 0.5 {
            p.rect_filled(
                Rect::from_min_max(pos2(x, mid_y - h), pos2(x + w, mid_y + h)),
                0.0,
                c.gamma_multiply(0.85),
            );
        }
    }
}

/// Markers drawn on a waveform.
#[derive(Debug, Default)]
pub struct Marks<'a> {
    pub cue: Option<f64>,
    pub hot_cues: &'a [Option<f64>],
    pub loop_range: Option<(f64, f64)>,
    pub loop_active: bool,
    /// AI transition region (source frames) and label.
    pub ai_region: Option<(f64, f64, &'a str)>,
}

/// Scrolling, zoomed waveform centred on the playhead, with the beat grid.
#[allow(clippy::too_many_arguments)]
pub fn zoomed_waveform(
    ui: &mut Ui,
    wf: Option<&Waveform>,
    audio: Option<&TrackAudio>,
    position: f64,
    rate: f64,
    zoom_sec: f32,
    marks: &Marks<'_>,
    height: f32,
) -> Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, Color32::from_gray(18));
    let (Some(wf), Some(audio)) = (wf, audio) else {
        return resp;
    };
    let sr = f64::from(wf.sample_rate);
    // Frames per pixel: playback speed scales the visible span so tempo
    // matched decks scroll at the same speed.
    let fpp = f64::from(zoom_sec) * sr * rate.max(0.25) / f64::from(width);
    let cx = rect.center().x;
    let frame_at = |x: f32| position + f64::from(x - cx) * fpp;
    let x_of = |f: f64| cx + ((f - position) / fpp) as f32;
    let mid_y = rect.center().y;
    let half = rect.height() / 2.0 - 2.0;
    if let Some((s, e, label)) = marks.ai_region {
        let r = Rect::from_min_max(
            pos2(x_of(s).max(rect.left()), rect.top()),
            pos2(x_of(e).min(rect.right()), rect.bottom()),
        );
        if r.width() > 0.0 {
            p.rect_filled(r, 0.0, AI.gamma_multiply(0.18));
            p.text(
                r.left_top() + vec2(3.0, 2.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(10.0),
                AI,
            );
        }
    }
    if let Some((s, e)) = marks.loop_range {
        let r = Rect::from_min_max(pos2(x_of(s), rect.top()), pos2(x_of(e), rect.bottom()));
        let c = if marks.loop_active {
            Color32::from_rgb(60, 200, 90)
        } else {
            Color32::from_gray(90)
        };
        p.rect_filled(r, 0.0, c.gamma_multiply(0.2));
    }
    let step = 1.0;
    let mut x = rect.left();
    while x < rect.right() {
        let f0 = frame_at(x);
        let f1 = frame_at(x + step);
        if f1 > 0.0 {
            let b0 = (f0.max(0.0) / BIN_FRAMES as f64) as usize;
            let b1 = ((f1.max(0.0) / BIN_FRAMES as f64) as usize).max(b0 + 1);
            let mut agg = redj_control::waveform::WaveBin::default();
            for b in wf.bins.get(b0..b1.min(wf.bins.len())).unwrap_or(&[]) {
                agg.low = agg.low.max(b.low);
                agg.mid = agg.mid.max(b.mid);
                agg.high = agg.high.max(b.high);
            }
            bin_color_bars(&p, x, mid_y, half, agg, step);
        }
        x += step;
    }
    // Beat grid.
    let b0 = audio.frame_to_beat(frame_at(rect.left())).floor() as i64;
    let b1 = audio.frame_to_beat(frame_at(rect.right())).ceil() as i64;
    if b1 - b0 < 400 {
        for b in b0..=b1 {
            let x = x_of(audio.beat_to_frame(b as f64));
            let down = b.rem_euclid(4) == 0;
            let c = if down {
                Color32::from_rgba_unmultiplied(255, 60, 60, 200)
            } else {
                Color32::from_white_alpha(60)
            };
            p.line_segment(
                [
                    pos2(x, rect.top()),
                    pos2(x, rect.top() + if down { 10.0 } else { 5.0 }),
                ],
                Stroke::new(1.0, c),
            );
            p.line_segment(
                [
                    pos2(x, rect.bottom()),
                    pos2(x, rect.bottom() - if down { 10.0 } else { 5.0 }),
                ],
                Stroke::new(1.0, c),
            );
            if down && b.rem_euclid(16) == 0 {
                p.text(
                    pos2(x + 2.0, rect.top() + 1.0),
                    Align2::LEFT_TOP,
                    format!("{}", b / 4),
                    FontId::monospace(9.0),
                    Color32::from_gray(160),
                );
            }
        }
    }
    let mark = |f: f64, c: Color32, label: &str| {
        let x = x_of(f);
        if x >= rect.left() && x <= rect.right() {
            p.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(1.5, c),
            );
            p.text(
                pos2(x + 2.0, rect.bottom() - 12.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(10.0),
                c,
            );
        }
    };
    if let Some(c) = marks.cue {
        mark(c, Color32::from_rgb(255, 160, 0), "CUE");
    }
    for (i, h) in marks.hot_cues.iter().enumerate() {
        if let Some(f) = h {
            mark(*f, Color32::from_rgb(60, 200, 90), &format!("{}", i + 1));
        }
    }
    p.line_segment(
        [pos2(cx, rect.top()), pos2(cx, rect.bottom())],
        Stroke::new(2.0, Color32::WHITE),
    );
    resp
}

/// Whole-track overview; returns the clicked fraction (0..1) for seeking.
pub fn overview(
    ui: &mut Ui,
    wf: Option<&Waveform>,
    frac: f64,
    height: f32,
    ai: Option<(f64, f64)>,
) -> Option<f64> {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, Color32::from_gray(22));
    if let Some(wf) = wf
        && !wf.bins.is_empty()
    {
        let n = wf.bins.len() as f32;
        let mid = rect.center().y;
        let half = rect.height() / 2.0 - 1.0;
        let cols = width.max(1.0) as usize;
        for c in 0..cols {
            let a = (c as f32 / cols as f32 * n) as usize;
            let b = (((c + 1) as f32 / cols as f32 * n) as usize)
                .max(a + 1)
                .min(wf.bins.len());
            let mut agg = redj_control::waveform::WaveBin::default();
            for bin in &wf.bins[a.min(b)..b] {
                agg.low = agg.low.max(bin.low);
                agg.mid = agg.mid.max(bin.mid);
                agg.high = agg.high.max(bin.high);
            }
            bin_color_bars(&p, rect.left() + c as f32, mid, half, agg, 1.0);
        }
        if let Some((s, e)) = ai {
            let r = Rect::from_min_max(
                pos2(rect.left() + (s as f32) * width, rect.top()),
                pos2(rect.left() + (e as f32) * width, rect.bottom()),
            );
            p.rect_filled(r, 0.0, AI.gamma_multiply(0.3));
        }
        let x = rect.left() + frac as f32 * width;
        p.rect_filled(
            Rect::from_min_max(rect.min, pos2(x, rect.bottom())),
            0.0,
            Color32::from_black_alpha(110),
        );
        p.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.5, Color32::WHITE),
        );
    }
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        return Some(f64::from((pos.x - rect.left()) / width).clamp(0.0, 1.0));
    }
    None
}

/// Formats seconds as m:ss.
pub fn mmss(sec: f64) -> String {
    let s = sec.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// A small colored toggle button.
pub fn toggle(ui: &mut Ui, on: bool, text: &str, color: Color32, size: Vec2) -> Response {
    let fill = if on { color } else { Color32::from_gray(50) };
    let txt = if on {
        Color32::BLACK
    } else {
        Color32::from_gray(220)
    };
    ui.add_sized(
        size,
        egui::Button::new(egui::RichText::new(text).color(txt).strong()).fill(fill),
    )
}
