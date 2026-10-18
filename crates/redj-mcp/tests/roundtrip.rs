use std::sync::Arc;

use redj_control::Session;
use redj_core::{BeatGrid, Library, Loudness, Section, SectionKind, TrackId, TrackInfo, TrackTags};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};

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

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    args: Value,
) -> (bool, Value) {
    let mut p = CallToolRequestParams::new(name.to_owned());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    let r = client.call_tool(p).await.unwrap();
    let text = r.content[0].as_text().unwrap().text.clone();
    let v = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (r.is_error != Some(true), v)
}

#[tokio::test]
async fn tools_round_trip_in_process() {
    let lib: Library = [
        track("aaaa000000000001", 124.0),
        track("bbbb000000000002", 126.0),
    ]
    .into_iter()
    .collect();
    let session = Arc::new(Session::new(lib));
    let io = redj_mcp::serve_in_process(session.clone());
    let client = ().serve(io).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for n in [
        "list_tracks",
        "get_track",
        "compatibility",
        "suggest_plan",
        "validate_plan",
        "submit_plan",
    ] {
        assert!(names.contains(&n), "missing tool {n}: {names:?}");
    }
    let info = client.peer_info().unwrap();
    assert!(info.instructions.as_deref().unwrap().contains("BARS"));

    let (ok, list) = call(&client, "list_tracks", json!({})).await;
    assert!(ok);
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["mix_out"], json!([112.0, 128.0]));

    let (ok, _) = call(&client, "get_track", json!({"id": "zzzz"})).await;
    assert!(!ok, "unknown id is a tool error");

    // A broken plan comes back with a path and a hint.
    let bad = json!({"plan": {"entries": [
        {"track": "aaaa"},
        {"track": "bbbb", "transition": {"at_bar": 120, "length_bars": 16, "style": "bass_swap"}}
    ]}});
    let (_, rep) = call(&client, "submit_plan", bad).await;
    assert_eq!(rep["ok"], json!(false));
    assert_eq!(
        rep["errors"][0]["path"],
        json!("/entries/1/transition/at_bar")
    );
    assert!(session.accepted_plan().is_none());

    // The plan may also arrive as a JSON string.
    let good = json!({"plan": json!({"entries": [
        {"track": "aaaa"},
        {"track": "bbbb", "transition": {"at_bar": 112, "length_bars": 16, "style": "bass_swap"}}
    ]}).to_string()});
    let (_, rep) = call(&client, "submit_plan", good).await;
    assert_eq!(rep["ok"], json!(true), "{rep}");
    assert_eq!(rep["tracklist"].as_array().unwrap().len(), 2);
    assert!(session.accepted_plan().is_some());

    let (ok, plan) = call(&client, "suggest_plan", json!({"max_tracks": 2})).await;
    assert!(ok);
    assert_eq!(plan["entries"].as_array().unwrap().len(), 2);
    client.cancel().await.unwrap();
}
