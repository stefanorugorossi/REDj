use std::sync::Arc;

use redj_ai::{AgentConfig, AgentEvent, MockModel, Role, SetRequest, plan_set};
use redj_control::Session;
use redj_core::{BeatGrid, Library, Loudness, Section, SectionKind, TrackId, TrackInfo, TrackTags};
use serde_json::json;

fn track(id: &str, bpm: f64) -> TrackInfo {
    let s = |kind, a: f64, b: f64| Section {
        kind,
        start_bar: a,
        end_bar: b,
        energy: 0.5,
    };
    TrackInfo {
        id: TrackId(id.into()),
        path: format!("{id}.wav").into(),
        tags: TrackTags::default(),
        duration_sec: 128.0 * 240.0 / bpm,
        sample_rate: 44_100,
        channels: 2,
        grid: BeatGrid::constant(bpm, 0.0),
        sections: vec![
            s(SectionKind::Intro, 0.0, 16.0),
            s(SectionKind::Drop, 16.0, 112.0),
            s(SectionKind::Outro, 112.0, 128.0),
        ],
        bar_energy: vec![0.5; 128],
        loudness: Loudness {
            integrated_lufs: -9.0,
            true_peak_dbtp: -1.0,
            range_lu: 3.0,
        },
        key: None,
        analyzer_version: 1,
    }
}

fn session() -> Arc<Session> {
    let lib: Library = [
        track("aaaa000000000001", 124.0),
        track("bbbb000000000002", 126.0),
    ]
    .into_iter()
    .collect();
    Arc::new(Session::new(lib))
}

#[tokio::test]
async fn mock_model_plans_a_set_through_mcp() {
    let s = session();
    let bad_plan = json!({"entries": [
        {"track": "aaaa"},
        {"track": "bbbb", "transition": {"at_bar": 120, "length_bars": 16, "style": "bass_swap"}}
    ]});
    let good_plan = json!({"entries": [
        {"track": "aaaa"},
        {"track": "bbbb", "transition": {"at_bar": 112, "length_bars": 16, "style": "bass_swap",
            "rationale": "outro over intro"}}
    ]});
    let model = Arc::new(MockModel::new([
        MockModel::tool_call("list_tracks", json!({})),
        MockModel::tool_call("submit_plan", json!({ "plan": bad_plan })),
        // String-encoded arguments are accepted too.
        MockModel::tool_call(
            "submit_plan",
            json!(json!({ "plan": good_plan }).to_string()),
        ),
        MockModel::text("unreachable: the run stops once a plan is accepted"),
    ]));
    let mut events = Vec::new();
    let out = plan_set(
        s.clone(),
        model.clone(),
        AgentConfig::default(),
        &SetRequest {
            minutes: Some(10.0),
            ..SetRequest::default()
        },
        |e| events.push(e),
    )
    .await
    .unwrap();
    assert!(out.stopped);
    assert_eq!(out.turns, 3);
    assert_eq!(out.tool_calls, 3);
    assert!(s.accepted_plan().is_some());

    let reqs = model.requests();
    assert_eq!(reqs.len(), 3);
    // Stable prefix: same system prompt and tools in every request.
    assert_eq!(reqs[0].messages[0].role, Role::System);
    assert!(reqs[0].messages[0].content.contains("AI DJ"));
    assert!(reqs[0].messages[0].content.contains("BARS"));
    assert_eq!(reqs[0].tools, reqs[2].tools);
    let names: Vec<&str> = reqs[0]
        .tools
        .iter()
        .map(|t| t.function.name.as_str())
        .collect();
    assert!(names.contains(&"submit_plan") && !names.contains(&"render_mix"));
    assert_eq!(reqs[0].options.num_ctx, 32_768);
    assert_eq!(reqs[0].think, Some(false));
    // The model saw the validation error with its path before retrying.
    let last = reqs[2].messages.last().unwrap();
    assert_eq!(last.role, Role::Tool);
    assert!(
        last.content.contains("/entries/1/transition/at_bar"),
        "{}",
        last.content
    );
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { ok: true, name, .. } if name == "list_tracks")
    ));
}

#[tokio::test]
async fn too_many_turns_is_an_error() {
    let model = MockModel::new((0..3).map(|_| MockModel::tool_call("list_tracks", json!({}))));
    let cfg = AgentConfig {
        max_turns: 3,
        ..AgentConfig::default()
    };
    let r = plan_set(session(), model, cfg, &SetRequest::default(), |_| {}).await;
    assert!(matches!(r, Err(redj_ai::AiError::TooManyTurns(3))));
}
