//! Statusline payloads in `tests/fixtures/statusline/`, each parsed and
//! asserted field by field. `full.json` and `partial.json` are captured
//! verbatim from live Claude Code 2.1.202 (the registered statusline
//! command tee'd to a file): `full.json` is a mid-turn payload with every
//! window reported, `partial.json` a session-start payload with `null`
//! percentages and no `rate_limits`. Live payloads carry integer
//! percentages and fields the parser doesn't map (`prompt_id`,
//! `effort.level`, `context_window.current_usage`, …) — the tests prove
//! both are handled. `garbage.txt` stays synthetic: it exists to not be
//! JSON. Malformed input must return `None`, never panic.

use std::path::Path;

use roster_detect::statusline::parse;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/statusline")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()))
}

#[test]
fn full_payload_yields_every_telemetry_field() {
    let payload = parse(&fixture("full.json")).expect("full payload parses");
    assert_eq!(payload.session_name.as_deref(), Some("Acknowledge request"));
    assert_eq!(
        payload.session_id.as_deref(),
        Some("719520a9-eb3f-41ce-9782-62b725c49ab7")
    );
    let t = payload.telemetry.expect("numbers reported");
    assert_eq!(t.model.as_deref(), Some("Fable 5"));
    // Live payloads report integer percentages.
    assert_eq!(t.context_pct, Some(96.0));
    assert_eq!(t.cost_usd, Some(0.765_263));
    let rl = t.rate_limit.expect("rate-limit windows reported");
    let five = rl.five_hour.expect("five-hour window reported");
    assert_eq!(five.used_pct, 26.0);
    let seven = rl.seven_day.expect("seven-day window reported");
    assert_eq!(seven.used_pct, 24.0);
    // The fixture's `resets_at` values are the captured epoch instants; the
    // remaining durations depend on the wall clock, so only presence is
    // asserted — the epoch arithmetic is unit-tested with an injected clock.
    assert!(five.resets_in.is_some());
    assert!(seven.resets_in.is_some());
}

#[test]
fn partial_payload_maps_null_and_absent_fields_to_none() {
    // A session-start payload: `current_usage` and both percentages are
    // `null`, `rate_limits` absent, and the cost a literal integer `0`.
    let payload = parse(&fixture("partial.json")).expect("partial payload parses");
    assert_eq!(
        payload.session_name, None,
        "unnamed session carries no name"
    );
    let t = payload.telemetry.expect("numbers reported");
    assert_eq!(t.model.as_deref(), Some("Fable 5"));
    assert_eq!(t.context_pct, None, "null percentage is absent, not zero");
    assert_eq!(t.cost_usd, Some(0.0));
    assert_eq!(t.rate_limit, None);
}

#[test]
fn garbage_input_returns_none_without_panicking() {
    assert_eq!(parse(&fixture("garbage.txt")), None);
}

#[test]
fn json_that_is_not_an_object_returns_none() {
    assert_eq!(parse("[1, 2, 3]"), None);
    assert_eq!(parse("42"), None);
    assert_eq!(parse("\"just a string\""), None);
    assert_eq!(parse("null"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn empty_object_is_no_telemetry_not_an_empty_report() {
    // An object with none of the mapped fields — `{}`, or a payload whose
    // keys all drifted — is "no telemetry": a `Some(default)` would grow a
    // blank badge line on the pane's sidebar card.
    assert_eq!(parse("{}"), None);
}
