// Byte-cap tests for get_ceph_health (card #16, fix round 3). The Codex
// Inspector showed that strings cut by character count can still push the
// fixed fields past 1,024 bytes when a character takes four UTF-8 bytes or six
// bytes as a JSON escape. Every string is now cut by its serialized JSON byte
// length, and a final guard shrinks the answer in a fixed order with a tiny
// fallback. These tests run the Inspector's boundary probe, put Unicode-heavy
// and escape-heavy values in every string field, sweep thousands of random
// inputs, and reach the shrink steps and the fallback through a smaller cap.
use proxmox_mcp_rs::mcp::{
    summarize_ceph_status_from, summarize_ceph_status_within, CEPH_HEALTH_MAX_BYTES,
};
use serde_json::{json, Value};

fn size(v: &Value) -> usize {
    serde_json::to_string(v).unwrap().len()
}

/// Serialized JSON bytes of a string, quotes not counted.
fn content_len(s: &str) -> usize {
    serde_json::to_string(s).unwrap().len() - 2
}

/// Budget per string, in serialized bytes, plus the 3-byte ellipsis.
const NAME_MAX: usize = 64 + 3;
const MESSAGE_MAX: usize = 120 + 3;
const SOURCE_MAX: usize = 96 + 3;

/// Characters that cost more serialized bytes than characters.
const HEAVY: [(&str, &str); 9] = [
    ("four-byte emoji", "🔥"),
    ("four-byte supplementary", "\u{10FFFF}"),
    ("three-byte CJK", "中"),
    ("two-byte accented", "é"),
    ("double quote", "\""),
    ("backslash", "\\"),
    ("control \\u0001", "\u{0001}"),
    ("control \\u001f", "\u{001f}"),
    ("mixed", "a\"🔥\u{0002}\\\n中"),
];

#[test]
fn inspector_fixed_field_probe_stays_under_the_cap() {
    // The Inspector's reproduction: empty lists, six totals at f64::MAX, a
    // 5,000-character health word and a 300-character node in the source.
    // Before the fix: 1,028 bytes with flames, 1,334 with control characters.
    for (label, c) in [
        ("four-byte Unicode", "🔥"),
        ("JSON-escaped control", "\u{0001}"),
    ] {
        let n = f64::MAX;
        let status = json!({"health": {"status": c.repeat(5000)}, "quorum_names": [],
            "osdmap": {"num_osds": n, "num_up_osds": n, "num_in_osds": n},
            "pgmap": {"num_pgs": n, "pgs_by_state": [], "bytes_used": n, "bytes_total": n}});
        let source = format!("/nodes/{}/ceph/status", c.repeat(300));
        let out = summarize_ceph_status_from(&status, Some(&source));
        let len = size(&out);
        assert!(len <= CEPH_HEALTH_MAX_BYTES, "{label}: {len} bytes");
        assert!(
            out.get("truncated").is_none(),
            "{label}: fallback not needed"
        );
        let health = out["health"].as_str().unwrap();
        assert!(health.ends_with('…') && content_len(health) <= NAME_MAX);
        assert!(out["source"].as_str().unwrap().starts_with("/nodes/"));
        assert_eq!(out["osds"]["total"], n);
        assert_eq!(out["usage"]["percent_used"], 100.0);
        assert_eq!(out["checks_omitted"], 0);
        assert_eq!(out["monitors"]["quorum_omitted"], 0);
        assert_eq!(out["placement_groups"]["by_state_omitted"], 0);
    }
}

#[test]
fn every_string_field_is_cut_by_serialized_bytes() {
    for (label, c) in HEAVY {
        let long = |i: usize| format!("{i}{}", c.repeat(2000));
        let checks: serde_json::Map<String, Value> = (0..5)
            .map(|i| {
                (
                    long(i),
                    json!({"severity": long(i), "summary": {"message": long(i)}}),
                )
            })
            .collect();
        let states: Vec<Value> = (0..5)
            .map(|i| json!({"state_name": long(i), "count": 7}))
            .collect();
        let n = f64::MAX;
        let status = json!({
            "health": {"status": long(0), "checks": checks},
            "quorum_names": (0..5).map(long).collect::<Vec<_>>(),
            "osdmap": {"num_osds": n, "num_up_osds": n, "num_in_osds": n},
            "pgmap": {"num_pgs": n, "pgs_by_state": states, "bytes_used": n, "bytes_total": n}
        });
        let v = summarize_ceph_status_from(&status, Some(&long(0)));
        assert!(
            size(&v) <= CEPH_HEALTH_MAX_BYTES,
            "{label}: {} bytes",
            size(&v)
        );
        assert!(v.get("truncated").is_none(), "{label}");
        let health = v["health"].as_str().unwrap();
        assert!(
            health.ends_with('…') && content_len(health) <= NAME_MAX,
            "{label}: health"
        );
        let source = v["source"].as_str().unwrap();
        assert!(
            source.ends_with('…') && content_len(source) <= SOURCE_MAX,
            "{label}: source"
        );
        let checks = v["checks"].as_array().unwrap();
        for ch in checks {
            for (field, max) in [
                ("name", NAME_MAX),
                ("severity", NAME_MAX),
                ("summary", MESSAGE_MAX),
            ] {
                let s = ch[field].as_str().unwrap();
                assert!(
                    s.ends_with('…') && content_len(s) <= max,
                    "{label}: check {field}"
                );
            }
        }
        let names = v["monitors"]["quorum"].as_array().unwrap();
        for name in names {
            let s = name.as_str().unwrap();
            assert!(
                s.ends_with('…') && content_len(s) <= NAME_MAX,
                "{label}: monitor"
            );
        }
        let by_state = v["placement_groups"]["by_state"].as_object().unwrap();
        for (key, count) in by_state {
            assert!(
                key.ends_with('…') && content_len(key) <= NAME_MAX,
                "{label}: PG state"
            );
            assert_eq!(*count, 7);
        }
        assert_eq!(
            checks.len() + v["checks_omitted"].as_u64().unwrap() as usize,
            5
        );
        assert_eq!(
            names.len() + v["monitors"]["quorum_omitted"].as_u64().unwrap() as usize,
            5
        );
        assert_eq!(
            by_state.len() + v["placement_groups"]["by_state_omitted"].as_u64().unwrap() as usize,
            5
        );
        assert_eq!(v["osds"]["in"], n, "{label}: totals are kept");
    }
}

#[test]
fn two_pg_states_cut_to_the_same_name_count_one_as_omitted() {
    let name = |tail: &str| format!("{}{tail}", "s".repeat(64));
    let v = summarize_ceph_status_from(
        &json!({"pgmap": {"pgs_by_state": [
            {"state_name": name("A"), "count": 1},
            {"state_name": name("B"), "count": 2},
            {"state_name": "active+clean", "count": 3}
        ]}}),
        None,
    );
    let by_state = v["placement_groups"]["by_state"].as_object().unwrap();
    assert_eq!(by_state.len(), 2);
    assert_eq!(by_state[&name("…")], 1, "the first of the two wins");
    assert_eq!(by_state["active+clean"], 3);
    assert_eq!(v["placement_groups"]["by_state_omitted"], 1);
}

#[test]
fn valid_health_word_survives_at_every_cap_down_to_the_fallback() {
    let status = json!({"health": {"status": "HEALTH_WARN", "checks": {"X": {"severity": "HEALTH_WARN", "summary": {"message": "m".repeat(200)}}}},
        "quorum_names": ["a", "b", "c"], "osdmap": {"num_osds": 12, "num_up_osds": 11, "num_in_osds": 12},
        "pgmap": {"num_pgs": 32, "pgs_by_state": [{"state_name": "active+clean", "count": 32}], "bytes_used": 1, "bytes_total": 4}});
    for cap in (1..=CEPH_HEALTH_MAX_BYTES).rev() {
        let v = summarize_ceph_status_within(&status, Some("/cluster/ceph/status"), cap);
        assert_eq!(v["health"], "HEALTH_WARN", "cap {cap}");
        assert_eq!(v["osds"]["up"], 11, "cap {cap}");
        assert_eq!(v["usage"]["percent_used"], 25.0, "cap {cap}");
        if v.get("truncated").is_none() {
            assert!(size(&v) <= cap, "cap {cap}: {} bytes", size(&v));
        } else {
            assert_eq!(
                v,
                json!({"health": "HEALTH_WARN", "osds": {"total": 12, "up": 11, "in": 12},
                "placement_groups": {"total": 32}, "usage": {"bytes_used": 1, "bytes_total": 4, "percent_used": 25.0},
                "truncated": true}),
                "cap {cap}"
            );
            assert!(size(&v) <= 327);
        }
    }
}

#[test]
fn fallback_through_the_real_pipeline_with_the_longest_numbers() {
    // Every total at the longest JSON number (24 bytes) and heavy strings
    // everywhere: a 200-byte cap is below the fixed part, so the lists stay
    // empty, source and health are shortened, and the fallback is returned.
    let n = -f64::MIN_POSITIVE;
    let status = json!({"health": {"status": format!("HEALTH_ERR{}", "🔥".repeat(100))},
        "quorum_names": ["🔥".repeat(100)],
        "osdmap": {"num_osds": n, "num_up_osds": n, "num_in_osds": n},
        "pgmap": {"num_pgs": n, "pgs_by_state": [{"state_name": "\u{0001}".repeat(100), "count": 1}],
                  "bytes_used": n, "bytes_total": n}});
    let v = summarize_ceph_status_within(&status, Some(&"\"".repeat(500)), 200);
    assert_eq!(v["truncated"], true);
    assert_eq!(v["health"], "HEALTH_ERR…");
    assert_eq!(v["osds"], json!({"total": n, "up": n, "in": n}));
    assert_eq!(v["placement_groups"], json!({"total": n}));
    assert_eq!(v["usage"]["bytes_used"], n);
    assert!(
        v["usage"].get("percent_used").is_none(),
        "a negative total gives no percent"
    );
    assert!(size(&v) <= 327, "{} bytes", size(&v));
    // The same input at the real cap needs no fallback.
    let full =
        summarize_ceph_status_within(&status, Some(&"\"".repeat(500)), CEPH_HEALTH_MAX_BYTES);
    assert!(full.get("truncated").is_none());
    assert!(size(&full) <= CEPH_HEALTH_MAX_BYTES);
}

// ---------------------------------------------------------------------------
// Property-style sweep: thousands of random inputs with 4-byte UTF-8, JSON
// escapes and control characters in every string, random numbers of every
// shape in every total, and random list lengths.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n
    }
    fn string(&mut self, max_len: usize) -> String {
        const ALPHABET: [char; 16] = [
            'a',
            'Z',
            '_',
            '+',
            ' ',
            '"',
            '\\',
            '\n',
            '\t',
            '\u{0001}',
            '\u{001f}',
            '\u{7f}',
            'é',
            '中',
            '🔥',
            '\u{10FFFF}',
        ];
        let len = self.below(max_len + 1);
        (0..len)
            .map(|_| ALPHABET[self.below(ALPHABET.len())])
            .collect()
    }
    fn number(&mut self) -> Value {
        match self.below(14) {
            0 => json!(0),
            1 => json!(12),
            2 => json!(-1),
            3 => json!(u64::MAX),
            4 => json!(i64::MIN),
            5 => json!(f64::MAX),
            6 => json!(-f64::MAX),
            7 => json!(f64::MIN_POSITIVE),
            8 => json!(-f64::MIN_POSITIVE),
            9 => json!(0.1),
            10 => json!(1e-300),
            11 => json!("12"),
            12 => Value::Null,
            _ => json!({"x": 1}),
        }
    }
}

#[test]
fn sweep_three_thousand_random_inputs_never_exceed_the_cap() {
    let mut rng = Rng(20261013);
    for case in 0..3000 {
        let n_checks = rng.below(31);
        let n_quorum = rng.below(31);
        let n_states = rng.below(31);
        let mut checks = serde_json::Map::new();
        for i in 0..n_checks {
            // Distinct keys: a JSON object cannot hold two checks with one name.
            checks.insert(
                format!("{i}{}", rng.string(300)),
                json!({"severity": rng.string(300), "summary": {"message": rng.string(400)}}),
            );
        }
        let quorum: Vec<String> = (0..n_quorum).map(|_| rng.string(300)).collect();
        let states: Vec<Value> = (0..n_states)
            .map(|_| json!({"state_name": rng.string(300), "count": rng.number()}))
            .collect();
        let totals = [
            rng.number(),
            rng.number(),
            rng.number(),
            rng.number(),
            rng.number(),
            rng.number(),
        ];
        let health = rng.string(400);
        let osdmap =
            json!({"num_osds": totals[0], "num_up_osds": totals[1], "num_in_osds": totals[2]});
        let status = json!({
            "health": {"status": health, "checks": checks},
            "quorum_names": quorum,
            "osdmap": if case % 2 == 0 { osdmap } else { json!({"osdmap": osdmap}) },
            "pgmap": {"num_pgs": totals[3], "pgs_by_state": states,
                      "bytes_used": totals[4], "bytes_total": totals[5]},
            "mgrmap": {"filler": "x".repeat(rng.below(5000))}
        });
        let source = if case % 3 == 0 {
            None
        } else {
            Some(rng.string(600))
        };
        let v = summarize_ceph_status_from(&status, source.as_deref());

        let len = size(&v);
        assert!(
            len <= CEPH_HEALTH_MAX_BYTES,
            "case {case}: {len} bytes: {v}"
        );
        assert!(
            v.get("truncated").is_none(),
            "case {case}: fallback must not be needed"
        );
        let h = v["health"].as_str().unwrap();
        assert!(content_len(h) <= NAME_MAX, "case {case}: health");
        if let Some(src) = &source {
            let s = v["source"].as_str().unwrap();
            assert!(content_len(s) <= SOURCE_MAX, "case {case}: source");
            assert!(
                src.starts_with(s.trim_end_matches('…')),
                "case {case}: source prefix"
            );
        }
        let kept_checks = v["checks"].as_array().unwrap();
        for ch in kept_checks {
            assert!(
                content_len(ch["name"].as_str().unwrap()) <= NAME_MAX,
                "case {case}"
            );
            assert!(
                content_len(ch["severity"].as_str().unwrap()) <= NAME_MAX,
                "case {case}"
            );
            assert!(
                content_len(ch["summary"].as_str().unwrap()) <= MESSAGE_MAX,
                "case {case}"
            );
        }
        assert_eq!(
            kept_checks.len() + v["checks_omitted"].as_u64().unwrap() as usize,
            n_checks,
            "case {case}: checks"
        );
        let kept_quorum = v["monitors"]["quorum"].as_array().unwrap();
        for name in kept_quorum {
            assert!(
                content_len(name.as_str().unwrap()) <= NAME_MAX,
                "case {case}"
            );
        }
        assert_eq!(
            kept_quorum.len() + v["monitors"]["quorum_omitted"].as_u64().unwrap() as usize,
            n_quorum,
            "case {case}: quorum"
        );
        assert_eq!(v["monitors"]["count"], n_quorum, "case {case}");
        let kept_states = v["placement_groups"]["by_state"].as_object().unwrap();
        for key in kept_states.keys() {
            assert!(content_len(key) <= NAME_MAX, "case {case}");
        }
        assert_eq!(
            kept_states.len()
                + v["placement_groups"]["by_state_omitted"].as_u64().unwrap() as usize,
            n_states,
            "case {case}: PG states"
        );
        for (at, want) in [
            ("/osds/total", &totals[0]),
            ("/osds/up", &totals[1]),
            ("/osds/in", &totals[2]),
            ("/placement_groups/total", &totals[3]),
            ("/usage/bytes_used", &totals[4]),
            ("/usage/bytes_total", &totals[5]),
        ] {
            let got = v.pointer(at);
            if want.is_number() {
                assert_eq!(got, Some(want), "case {case}: {at}");
            } else {
                assert!(got.is_none(), "case {case}: {at} must be left out");
            }
        }
    }
}
