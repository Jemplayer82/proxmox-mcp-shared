// Targeted edge cases for the get_ceph_health byte cap (card #16, Shield round 3).
// The cap counts each string by its serialized JSON bytes, so a JSON escape
// (\" \\ \n is 2 bytes, \u0001 is 6) can straddle a budget. These tests put an
// escape exactly at, just under and just over each budget, and check that the
// answer near the 1,024-byte cap is packed with mixed escapes. Pure functions,
// no HTTP mocks.
use proxmox_mcp_rs::mcp::{
    summarize_ceph_status, summarize_ceph_status_from, CEPH_HEALTH_MAX_BYTES,
};
use serde_json::{json, Value};

fn size(v: &Value) -> usize {
    serde_json::to_string(v).unwrap().len()
}

/// The JSON form of a string, without its two quotes.
fn json_bytes(s: &str) -> usize {
    serde_json::to_string(s).unwrap().len() - 2
}

fn one_check(name: &str, message: &str) -> Value {
    json!({"health": {"status": "HEALTH_WARN", "checks": {
        name: {"severity": "HEALTH_WARN", "summary": {"message": message}}
    }}})
}

// ---------------------------------------------------------------------------
// Escapes at the message budget (120 bytes)
// ---------------------------------------------------------------------------

#[test]
fn two_byte_escape_that_would_cross_the_message_budget_is_not_split() {
    // 119 plain bytes, then "\n" (2 bytes): 121 > 120, so the cut falls before "\n".
    let msg = format!("{}\n{}", "a".repeat(119), "b".repeat(200));
    let v = summarize_ceph_status(&one_check("CHK", &msg));
    let s = v["checks"][0]["summary"].as_str().unwrap();
    assert_eq!(s, format!("{}…", "a".repeat(119)));
    assert_eq!(json_bytes(s), 122);
}

#[test]
fn six_byte_escape_that_would_cross_the_message_budget_is_cut_before_it() {
    // 115 plain bytes, then "\u0001" (6 bytes): 121 > 120.
    let msg = format!("{}\u{1}{}", "a".repeat(115), "b".repeat(200));
    let v = summarize_ceph_status(&one_check("CHK", &msg));
    let s = v["checks"][0]["summary"].as_str().unwrap();
    assert_eq!(s, format!("{}…", "a".repeat(115)));
    assert!(!s.contains('\u{1}'));
}

#[test]
fn escape_that_ends_exactly_on_the_message_budget_is_kept_whole() {
    // 114 plain bytes, then "\u0001" (6 bytes): exactly 120, so no cut and no ellipsis.
    let msg = format!("{}\u{1}", "a".repeat(114));
    let v = summarize_ceph_status(&one_check("CHK", &msg));
    assert_eq!(v["checks"][0]["summary"].as_str().unwrap(), msg);
}

#[test]
fn quote_and_backslash_mix_at_the_message_budget_stays_valid() {
    // Alternating quote and backslash (2 bytes each) past the budget: the cut is
    // on a character boundary and the answer is still valid JSON that re-parses.
    let msg = "\"\\".repeat(200);
    let v = summarize_ceph_status(&one_check("CHK", &msg));
    let s = v["checks"][0]["summary"].as_str().unwrap();
    assert!(s.ends_with('…'));
    assert_eq!(
        json_bytes(s) - 3,
        120,
        "120 budget bytes, then the ellipsis"
    );
    let round: Value = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
    assert_eq!(round, v);
}

// ---------------------------------------------------------------------------
// Escapes at the name budget (64 bytes): check name, PG state name, health word
// ---------------------------------------------------------------------------

#[test]
fn escape_that_would_cross_the_name_budget_is_cut_before_it() {
    // 63 plain bytes, then '"' (2 bytes): 65 > 64.
    let name = format!("{}\"{}", "a".repeat(63), "b".repeat(10));
    let v = summarize_ceph_status(&one_check(&name, "m"));
    assert_eq!(
        v["checks"][0]["name"].as_str().unwrap(),
        format!("{}…", "a".repeat(63))
    );
}

#[test]
fn newline_in_a_pg_state_name_at_the_budget_is_cut_before_it() {
    // 63 plain bytes, then "\n" (2 bytes): 65 > 64. The state name is the map key.
    let state = format!("{}\n{}", "a".repeat(63), "b".repeat(10));
    let s = json!({"pgmap": {"num_pgs": 1, "pgs_by_state": [{"state_name": state, "count": 1}]}});
    let v = summarize_ceph_status(&s);
    let by_state = v["placement_groups"]["by_state"].as_object().unwrap();
    assert_eq!(by_state.len(), 1);
    assert!(by_state.contains_key(&format!("{}…", "a".repeat(63))));
}

#[test]
fn control_escape_at_the_health_word_budget_keeps_a_valid_word() {
    // The health word is cut at 64 bytes, so a valid word is never touched by it.
    // A word with a control escape past the budget is cut before the escape.
    let word = format!("{}\u{1}", "H".repeat(63));
    let v = summarize_ceph_status(&json!({"health": {"status": word}}));
    assert_eq!(
        v["health"].as_str().unwrap(),
        format!("{}…", "H".repeat(63))
    );
    let valid = summarize_ceph_status(&json!({"health": {"status": "HEALTH_ERR"}}));
    assert_eq!(valid["health"], "HEALTH_ERR");
}

// ---------------------------------------------------------------------------
// Escapes at the source budget (96 bytes)
// ---------------------------------------------------------------------------

#[test]
fn backslash_that_would_cross_the_source_budget_is_cut_before_it() {
    // 95 plain bytes, then "\\" (2 bytes): 97 > 96.
    let src = format!("{}\\{}", "a".repeat(95), "b".repeat(50));
    let v = summarize_ceph_status_from(&json!({}), Some(src.as_str()));
    assert_eq!(
        v["source"].as_str().unwrap(),
        format!("{}…", "a".repeat(95))
    );
}

// ---------------------------------------------------------------------------
// Near the 1,024-byte cap with mixed escapes
// ---------------------------------------------------------------------------

#[test]
fn mixed_escapes_near_the_cap_are_packed_and_counts_add_up() {
    // Forty checks, each with a different mix of plain text, emoji, quotes,
    // backslashes, a newline and a control character, sized around the budgets.
    let mut checks = serde_json::Map::new();
    for i in 0..40 {
        let msg = match i % 5 {
            0 => "🔥".repeat(60 + i),
            1 => "\"q\\".repeat(40 + i),
            2 => "line\n".repeat(30 + i),
            3 => "\u{1}x".repeat(25 + i),
            _ => "plain text ".repeat(10 + i),
        };
        checks.insert(
            format!("CHECK_{i:02}_{}", "n\"".repeat(10)),
            json!({"severity": if i % 7 == 0 { "HEALTH_ERR" } else { "HEALTH_WARN" },
                   "summary": {"message": msg}}),
        );
    }
    let s = json!({"health": {"status": "HEALTH_ERR", "checks": Value::Object(checks)}});
    let v = summarize_ceph_status(&s);
    assert!(
        size(&v) <= CEPH_HEALTH_MAX_BYTES,
        "answer is {} bytes",
        size(&v)
    );
    let kept = v["checks"].as_array().unwrap().len();
    let omitted = v["checks_omitted"].as_u64().unwrap() as usize;
    assert_eq!(
        kept + omitted,
        40,
        "kept + omitted must equal the checks sent"
    );
    assert!(omitted > 0, "forty long checks must not all fit");
    // The fill stops only when the next check would not fit, so the space left
    // over is less than one check (at most about 260 bytes with these budgets).
    let slack = CEPH_HEALTH_MAX_BYTES - size(&v);
    assert!(
        slack < 300,
        "{slack} bytes left unused; the fill stopped too early"
    );
}

#[test]
fn every_kept_string_is_within_its_budget_with_mixed_escapes() {
    let mut checks = serde_json::Map::new();
    for i in 0..12 {
        checks.insert(
            format!("N{i}{}", "\\\"\n".repeat(40)),
            json!({"severity": "HEALTH_WARN", "summary": {"message": "\u{1}🔥\"".repeat(60)}}),
        );
    }
    let s = json!({"health": {"status": "HEALTH_WARN", "checks": Value::Object(checks)}});
    let v = summarize_ceph_status_from(&s, Some("\\\"\u{2}🔥".repeat(60).as_str()));
    assert!(size(&v) <= CEPH_HEALTH_MAX_BYTES);
    for c in v["checks"].as_array().unwrap() {
        assert!(json_bytes(c["name"].as_str().unwrap()) <= 64 + 3);
        assert!(json_bytes(c["summary"].as_str().unwrap()) <= 120 + 3);
    }
    assert!(json_bytes(v["source"].as_str().unwrap()) <= 96 + 3);
}
