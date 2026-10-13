// Edge-case tests for the get_ceph_health size cap (card #16, Shield round 2).
// The fix bounds every variable part of the answer (monitor names, checks,
// PG states, source, health word). These tests check the bound on inputs the
// builder's tests did not cover, and that the omission counts add up to the
// number of entries Proxmox sent. They call the pure summary functions, so no
// HTTP mocks are needed.
use proxmox_mcp_rs::mcp::{
    summarize_ceph_status, summarize_ceph_status_from, CEPH_HEALTH_MAX_BYTES,
};
use serde_json::{json, Value};

fn size(v: &Value) -> usize {
    serde_json::to_string(v).unwrap().len()
}

fn checks_obj(n: usize, severity: &str, msg_len: usize) -> Value {
    let mut m = serde_json::Map::new();
    for i in 0..n {
        m.insert(
            format!("CHECK_{i:03}"),
            json!({"severity": severity, "summary": {"message": "m".repeat(msg_len)}}),
        );
    }
    Value::Object(m)
}

fn pg_states(n: usize, name_len: usize) -> Vec<Value> {
    (0..n)
        .map(
            |i| json!({"state_name": format!("{i:03}{}", "s".repeat(name_len)), "count": 1000 + i}),
        )
        .collect()
}

// ---------------------------------------------------------------------------
// Bounded variable parts, each on its own
// ---------------------------------------------------------------------------

#[test]
fn quorum_names_are_bounded_and_counted() {
    // 300 monitor names of 500 characters each: far more than fits in 1 KB.
    let names: Vec<String> = (0..300)
        .map(|i| format!("{i:03}{}", "q".repeat(500)))
        .collect();
    let v = summarize_ceph_status(&json!({"quorum_names": names}));
    assert!(
        size(&v) <= CEPH_HEALTH_MAX_BYTES,
        "answer is {} bytes",
        size(&v)
    );
    let kept = v["monitors"]["quorum"].as_array().unwrap();
    let omitted = v["monitors"]["quorum_omitted"].as_u64().unwrap() as usize;
    assert!(!kept.is_empty(), "some monitor names must still be listed");
    assert_eq!(kept.len() + omitted, 300);
    assert_eq!(
        v["monitors"]["count"], 300,
        "count stays the real quorum size"
    );
    for n in kept {
        assert!(n.as_str().unwrap().chars().count() <= 65);
    }
}

#[test]
fn pg_states_are_bounded_and_counted() {
    // 400 states with 300-character names: the state list must be cut.
    let v = summarize_ceph_status(
        &json!({"pgmap": {"num_pgs": 400, "pgs_by_state": pg_states(400, 300)}}),
    );
    assert!(
        size(&v) <= CEPH_HEALTH_MAX_BYTES,
        "answer is {} bytes",
        size(&v)
    );
    let kept = v["placement_groups"]["by_state"].as_object().unwrap().len();
    let omitted = v["placement_groups"]["by_state_omitted"].as_u64().unwrap() as usize;
    assert!(kept > 0);
    assert_eq!(kept + omitted, 400);
    assert_eq!(v["placement_groups"]["total"], 400);
}

#[test]
fn long_source_is_shortened_and_answer_stays_small() {
    let src = "/nodes/".to_string() + &"n".repeat(10_000) + "/ceph/status";
    let v = summarize_ceph_status_from(&json!({"health": {"status": "HEALTH_OK"}}), Some(&src));
    assert!(size(&v) <= CEPH_HEALTH_MAX_BYTES);
    let s = v["source"].as_str().unwrap();
    assert_eq!(s.chars().count(), 97, "96 characters plus the ellipsis");
    assert!(s.ends_with('…'));
}

#[test]
fn huge_health_word_is_shortened() {
    let v = summarize_ceph_status(&json!({"health": {"status": "H".repeat(5000)}}));
    assert!(size(&v) <= CEPH_HEALTH_MAX_BYTES);
    let h = v["health"].as_str().unwrap();
    assert!(h.ends_with('…'));
    assert!(h.chars().count() <= 65);
}

#[test]
fn check_messages_of_four_byte_characters_are_cut_by_characters() {
    // Each message is 300 flames (4 bytes each). Cut at 120 characters, not bytes.
    let mut m = serde_json::Map::new();
    for i in 0..30 {
        m.insert(
            format!("FIRE_{i:03}"),
            json!({"severity": "HEALTH_WARN", "summary": {"message": "🔥".repeat(300)}}),
        );
    }
    let v = summarize_ceph_status(
        &json!({"health": {"status": "HEALTH_WARN", "checks": Value::Object(m)}}),
    );
    assert!(
        size(&v) <= CEPH_HEALTH_MAX_BYTES,
        "answer is {} bytes",
        size(&v)
    );
    for c in v["checks"].as_array().unwrap() {
        let s = c["summary"].as_str().unwrap();
        assert_eq!(s.chars().count(), 121, "120 flames plus the ellipsis");
    }
}

// ---------------------------------------------------------------------------
// Omission counts are accurate
// ---------------------------------------------------------------------------

#[test]
fn nothing_omitted_when_everything_fits() {
    let v = summarize_ceph_status(&json!({
        "health": {"status": "HEALTH_WARN", "checks": checks_obj(2, "HEALTH_WARN", 20)},
        "quorum_names": ["pve1", "pve2", "pve3"],
        "pgmap": {"num_pgs": 3, "pgs_by_state": pg_states(2, 5)}
    }));
    assert_eq!(v["checks"].as_array().unwrap().len(), 2);
    assert_eq!(v["checks_omitted"], 0);
    assert_eq!(v["monitors"]["quorum"].as_array().unwrap().len(), 3);
    assert_eq!(v["monitors"]["quorum_omitted"], 0);
    assert_eq!(
        v["placement_groups"]["by_state"].as_object().unwrap().len(),
        2
    );
    assert_eq!(v["placement_groups"]["by_state_omitted"], 0);
}

#[test]
fn check_counts_add_up_for_many_sizes() {
    for n in [0usize, 1, 5, 20, 60, 200] {
        let v = summarize_ceph_status(
            &json!({"health": {"status": "HEALTH_ERR", "checks": checks_obj(n, "HEALTH_WARN", 300)}}),
        );
        assert!(
            size(&v) <= CEPH_HEALTH_MAX_BYTES,
            "n={n}: {} bytes",
            size(&v)
        );
        let kept = v["checks"].as_array().unwrap().len();
        assert_eq!(
            kept as u64 + v["checks_omitted"].as_u64().unwrap(),
            n as u64,
            "n={n}"
        );
    }
}

#[test]
fn severe_checks_are_kept_before_warnings_when_cut() {
    // 40 long warnings sort first alphabetically; one error sorts last. The error must survive the cut.
    let mut checks = checks_obj(40, "HEALTH_WARN", 200);
    checks.as_object_mut().unwrap().insert(
        "ZZZ_ERROR".into(),
        json!({"severity": "HEALTH_ERR", "summary": {"message": "down"}}),
    );
    let v = summarize_ceph_status(&json!({"health": {"status": "HEALTH_ERR", "checks": checks}}));
    assert!(
        v["checks_omitted"].as_u64().unwrap() > 0,
        "the cut must actually happen"
    );
    assert_eq!(v["checks"][0]["name"], "ZZZ_ERROR");
    assert_eq!(v["checks"][0]["severity"], "HEALTH_ERR");
}

// ---------------------------------------------------------------------------
// Missing or wrongly typed fields
// ---------------------------------------------------------------------------

#[test]
fn non_numeric_totals_are_left_out() {
    let v = summarize_ceph_status(&json!({
        "osdmap": {"num_osds": "12", "num_up_osds": {"x": 1}},
        "pgmap": {"bytes_used": {"x": 1}, "bytes_total": "9"}
    }));
    assert!(v["osds"]["total"].is_null());
    assert!(v["osds"]["up"].is_null());
    assert!(v["usage"]["bytes_used"].is_null());
    assert!(v["usage"]["bytes_total"].is_null());
    assert!(v["usage"]["percent_used"].is_null());
    assert!(size(&v) <= CEPH_HEALTH_MAX_BYTES);
}

#[test]
fn missing_health_key_gives_no_health_word_and_no_checks() {
    let v = summarize_ceph_status(&json!({"quorum_names": ["pve1"]}));
    assert!(
        v.get("health").is_none(),
        "absent health word must be left out"
    );
    assert_eq!(v["checks"], json!([]));
    assert_eq!(v["checks_omitted"], 0);
}

#[test]
fn pg_state_with_non_numeric_count_keeps_the_name_only() {
    let v = summarize_ceph_status(
        &json!({"pgmap": {"pgs_by_state": [{"state_name": "active+clean", "count": "many"}]}}),
    );
    let by_state = v["placement_groups"]["by_state"].as_object().unwrap();
    assert!(by_state.contains_key("active+clean"));
    assert!(
        by_state["active+clean"].is_null(),
        "a non-numeric count must not be copied"
    );
}

// ---------------------------------------------------------------------------
// Deterministic sweep: the cap holds for mixed inputs of many sizes
// ---------------------------------------------------------------------------

fn lcg(seed: &mut u64) -> usize {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*seed >> 33) as usize
}

#[test]
fn cap_holds_and_counts_add_up_for_500_mixed_inputs() {
    let mut seed = 20261013u64;
    for case in 0..500 {
        let n_checks = lcg(&mut seed) % 80;
        let n_quorum = lcg(&mut seed) % 60;
        let n_states = lcg(&mut seed) % 60;
        let msg_len = lcg(&mut seed) % 400;
        let name_len = lcg(&mut seed) % 300;
        let src_len = lcg(&mut seed) % 3000;
        let health_len = lcg(&mut seed) % 2000;

        let names: Vec<String> = (0..n_quorum)
            .map(|i| format!("{i:03}{}", "q".repeat(name_len)))
            .collect();
        let s = json!({
            "health": {"status": "H".repeat(health_len), "checks": checks_obj(n_checks, "HEALTH_WARN", msg_len)},
            "quorum_names": names,
            "osdmap": {"num_osds": n_checks, "num_up_osds": n_quorum, "num_in_osds": n_states},
            "pgmap": {"num_pgs": n_states, "pgs_by_state": pg_states(n_states, name_len),
                      "bytes_used": 1, "bytes_total": 3}
        });
        let src = "p".repeat(src_len);
        let v = summarize_ceph_status_from(&s, Some(&src));

        assert!(
            size(&v) <= CEPH_HEALTH_MAX_BYTES,
            "case {case}: {} bytes",
            size(&v)
        );
        let kept_checks = v["checks"].as_array().unwrap().len() as u64;
        assert_eq!(
            kept_checks + v["checks_omitted"].as_u64().unwrap(),
            n_checks as u64,
            "case {case}: checks"
        );
        let kept_quorum = v["monitors"]["quorum"].as_array().unwrap().len() as u64;
        assert_eq!(
            kept_quorum + v["monitors"]["quorum_omitted"].as_u64().unwrap(),
            n_quorum as u64,
            "case {case}: quorum"
        );
        let kept_states = v["placement_groups"]["by_state"].as_object().unwrap().len() as u64;
        assert_eq!(
            kept_states + v["placement_groups"]["by_state_omitted"].as_u64().unwrap(),
            n_states as u64,
            "case {case}: states"
        );
    }
}
