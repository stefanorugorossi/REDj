use redj_dsp::eq::band_gain;
use redj_dsp::{DjFilter, Echo, IsolatorEq, Smoother};

/// Target values of a channel strip for one frame.
#[derive(Debug, Clone, Copy)]
pub struct StripParams {
    /// Fader, linear amplitude 0..1.
    pub volume: f32,
    /// Input gain in dB.
    pub trim_db: f32,
    /// Low, mid, high in dB (-40 = kill).
    pub eq_db: [f32; 3],
    /// Filter knob -1..1.
    pub filter: f32,
    /// Echo send 0..1.
    pub echo_send: f32,
    pub echo_delay_samples: f32,
    pub echo_feedback: f32,
}

impl Default for StripParams {
    fn default() -> Self {
        Self {
            volume: 1.0,
            trim_db: 0.0,
            eq_db: [0.0; 3],
            filter: 0.0,
            echo_send: 0.0,
            echo_delay_samples: 0.0,
            echo_feedback: 0.5,
        }
    }
}

/// Output of a strip for one frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct StripOut {
    /// Post-fader signal plus echo trails (to the master).
    pub main: [f32; 2],
    /// Pre-fader signal after EQ and filter (to the headphone cue).
    pub pfl: [f32; 2],
}

/// Channel strip: trim, three-band isolator EQ, filter, echo, fader.
/// All parameters are smoothed per sample (a few milliseconds) so steps in
/// automation are click-free yet land on their exact sample.
#[derive(Debug, Clone)]
pub struct Strip {
    eq: IsolatorEq,
    filter: DjFilter,
    echo: Echo,
    trim: Smoother,
    volume: Smoother,
    bands: [Smoother; 3],
    filter_knob: Smoother,
    send: Smoother,
}

impl Strip {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        Self {
            eq: IsolatorEq::new(f64::from(sample_rate)),
            filter: DjFilter::new(sr),
            echo: Echo::new(sr, 4.0),
            trim: Smoother::new(1.0, 5.0, sr),
            volume: Smoother::new(1.0, 2.0, sr),
            bands: [Smoother::new(1.0, 2.0, sr); 3],
            filter_knob: Smoother::new(0.0, 8.0, sr),
            send: Smoother::new(0.0, 3.0, sr),
        }
    }

    /// Clears all state and jumps the smoothers to `p`.
    pub fn reset(&mut self, p: &StripParams) {
        self.eq.reset();
        self.filter.reset();
        self.echo.reset();
        self.trim.reset(redj_dsp::db_to_gain(p.trim_db));
        self.volume.reset(p.volume);
        for (s, &db) in self.bands.iter_mut().zip(&p.eq_db) {
            s.reset(band_gain(db) as f32);
        }
        self.filter_knob.reset(p.filter);
        self.send.reset(p.echo_send);
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32, p: &StripParams) -> StripOut {
        self.trim.set(redj_dsp::db_to_gain(p.trim_db));
        self.volume.set(p.volume);
        for (s, &db) in self.bands.iter_mut().zip(&p.eq_db) {
            s.set(band_gain(db) as f32);
        }
        self.filter_knob.set(p.filter);
        self.send.set(p.echo_send);

        let g = self.trim.next_value();
        let gains = [
            f64::from(self.bands[0].next_value()),
            f64::from(self.bands[1].next_value()),
            f64::from(self.bands[2].next_value()),
        ];
        let (l, r) = self.eq.process(l * g, r * g, gains);
        let (l, r) = self.filter.process(l, r, self.filter_knob.next_value());
        let send = self.send.next_value();
        self.echo.set_delay_samples(p.echo_delay_samples);
        let (el, er) = self.echo.process(l * send, r * send, p.echo_feedback);
        let v = self.volume.next_value();
        StripOut {
            main: [l * v + el, r * v + er],
            pfl: [l, r],
        }
    }
}
