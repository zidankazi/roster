//! Statusline payloads in `tests/fixtures/statusline/`, each parsed and
//! asserted field by field. `full.json` is the complete example payload
//! from the official statusline docs; `partial.json` is an early-session
//! payload with `null` percentages and no `rate_limits`; `garbage.txt` is
//! not JSON. Malformed input must return `None`, never panic.

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
    let t = parse(&fixture("full.json")).expect("full payload parses");
    assert_eq!(t.model.as_deref(), Some("Opus"));
    assert_eq!(t.context_pct, Some(92.0));
    assert_eq!(t.cost_usd, Some(0.01234));
    let rl = t.rate_limit.expect("five-hour window reported");
    assert_eq!(rl.used_pct, 23.5);
    // The fixture's `resets_at` is a fixed epoch instant; the remaining
    // duration depends on the wall clock, so only its presence is asserted
    // here — the epoch arithmetic is unit-tested with an injected clock.
    assert!(rl.resets_in.is_some());
}

#[test]
fn partial_payload_maps_null_and_absent_fields_to_none() {
    let t = parse(&fixture("partial.json")).expect("partial payload parses");
    assert_eq!(t.model.as_deref(), Some("Opus"));
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
fn empty_object_is_telemetry_with_no_readings() {
    let t = parse("{}").expect("an empty object is a valid payload");
    assert_eq!(t, roster_core::Telemetry::default());
}
