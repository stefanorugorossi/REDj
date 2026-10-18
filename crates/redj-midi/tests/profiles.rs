//! Every built-in profile parses and maps the essentials.

use redj_midi::{Action, Knob, builtin_profiles};

#[test]
fn builtin_profiles_cover_the_essentials() {
    let profiles = builtin_profiles();
    assert!(profiles.len() >= 4);
    for p in &profiles {
        let has = |f: &dyn Fn(&Action) -> bool| p.inputs.iter().any(|b| f(&b.action));
        for deck in 0..2 {
            assert!(
                has(&|a| *a == Action::PlayToggle { deck }),
                "{}: play {deck}",
                p.name
            );
            assert!(
                has(&|a| *a == Action::Cue { deck }),
                "{}: cue {deck}",
                p.name
            );
            assert!(
                has(&|a| *a
                    == Action::Knob {
                        deck,
                        knob: Knob::Volume
                    }),
                "{}: fader {deck}",
                p.name
            );
            assert!(
                has(&|a| matches!(a, Action::Jog { deck: d } if *d == deck)),
                "{}: jog {deck}",
                p.name
            );
        }
        assert!(
            has(&|a| matches!(a, Action::Global { .. })),
            "{}: crossfader",
            p.name
        );
        // No two inputs share a message.
        let mut keys: Vec<(u8, u8)> = p.inputs.iter().map(|b| (b.status.0, b.data1.0)).collect();
        keys.sort_unstable();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "{}: duplicate input", p.name);
    }
}
