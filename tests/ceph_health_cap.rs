// Size-cap tests for get_ceph_health (card #16, fix round 2). The Inspector's
// scratch probe (examples/inspector_cap.rs) showed that fields other than the
// checks (PG states, monitor names, source) could push the answer past the
// 1 KB cap. These tests run the real handler through wiremock with oversized
// non-check fields, with the long-check fixture, and with everything at once.
mod common;
use common::*;
use proxmox_mcp_rs::mcp::{
    summarize_ceph_status, summarize_ceph_status_from, CEPH_HEALTH_MAX_BYTES,
};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn text_of(res: &Value) -> String {
    res["content"][0]["text"].as_str().unwrap().to_string()
}

/// 32 placement-group state combinations, as in the Inspector's probe.
fn many_pg_states() -> Vec<Value> {
    let names = [
        "undersized",
        "degraded",
        "remapped",
        "backfill_wait",
        "recovering",
    ];
    (0..32)
        .map(|i| {
            let suffix: String = names
                .iter()
                .enumerate()
                .filter(|(bit, _)| i & (1 << bit) != 0)
                .map(|(_, s)| format!("+{}", s))
                .collect();
            json!({"state_name": format!("active{}", suffix), "count": 1})
        })
        .collect()
}

fn long_checks(n: usize) -> serde_json::Map<String, Value> {
    (0..n)
        .map(|i| {
            (
                format!("CHECK_{:02}", i),
                json!({"severity": if i == n - 1 { "HEALTH_ERR" } else { "HEALTH_WARN" },
                       "summary": {"message": "⚠".repeat(300)}}),
            )
        })
        .collect()
}

async fn health_via_handler(node: Option<&str>, status: Value) -> Value {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;
    let route = match node {
        Some(n) => format!("/api2/json/nodes/{}/ceph/status", n),
        None => "/api2/json/cluster/ceph/status".to_string(),
    };
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": status})))
        .expect(1)
        .mount(&mock_server)
        .await;
    let args = match node {
        Some(n) => json!({"node": n}),
        None => json!({}),
    };
    let res = server.call_tool("get_ceph_health", &args).await.unwrap();
    let text = text_of(&res);
    assert!(
        text.len() <= CEPH_HEALTH_MAX_BYTES,
        "answer is {} bytes, cap is {}",
        text.len(),
        CEPH_HEALTH_MAX_BYTES
    );
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn cap_holds_with_many_pg_states_and_no_checks() {
    // The Inspector's failing case: 1,505 bytes before the fix, checks_omitted 0.
    let v = health_via_handler(
        None,
        json!({
            "health": {"status": "HEALTH_WARN"},
            "quorum_names": ["pve1", "pve2", "pve3"],
            "osdmap": {"num_osds": 12, "num_up_osds": 12, "num_in_osds": 12},
            "pgmap": {"num_pgs": 32, "pgs_by_state": many_pg_states(),
                      "bytes_used": 250, "bytes_total": 1000}
        }),
    )
    .await;
    assert_eq!(v["health"], "HEALTH_WARN");
    assert_eq!(v["checks_omitted"], 0);
    assert_eq!(v["monitors"]["count"], 3);
    assert_eq!(v["monitors"]["quorum_omitted"], 0);
    assert_eq!(v["osds"]["total"], 12);
    assert_eq!(v["placement_groups"]["total"], 32);
    assert_eq!(v["usage"]["percent_used"], 25.0);
    assert_eq!(v["source"], "/cluster/ceph/status");
    let kept = v["placement_groups"]["by_state"].as_object().unwrap().len();
    let omitted = v["placement_groups"]["by_state_omitted"].as_u64().unwrap() as usize;
    assert!(kept > 0, "some PG states must still be listed");
    assert_eq!(kept + omitted, 32);
    assert!(omitted > 0, "32 states cannot all fit in 1 KB");
}

#[tokio::test]
async fn cap_holds_with_long_quorum_names_and_long_source() {
    // A 200-character node name makes the source path long, and each monitor
    // name is 100 characters.
    let node = "n".repeat(200);
    let names: Vec<String> = (0..5).map(|i| format!("{}{}", "m".repeat(99), i)).collect();
    let v = health_via_handler(
        Some(&node),
        json!({
            "health": {"status": "HEALTH_OK"},
            "quorum_names": names,
            "osdmap": {"num_osds": 3, "num_up_osds": 3, "num_in_osds": 3},
            "pgmap": {"num_pgs": 10, "pgs_by_state": [{"state_name": "active+clean", "count": 10}]}
        }),
    )
    .await;
    assert_eq!(v["health"], "HEALTH_OK");
    assert_eq!(v["monitors"]["count"], 5);
    let kept = v["monitors"]["quorum"].as_array().unwrap();
    let omitted = v["monitors"]["quorum_omitted"].as_u64().unwrap() as usize;
    assert_eq!(kept.len() + omitted, 5);
    for name in kept {
        let n = name.as_str().unwrap();
        assert!(
            n.chars().count() <= 65,
            "monitor name not shortened: {} chars",
            n.len()
        );
        assert!(n.ends_with('…'));
    }
    let source = v["source"].as_str().unwrap();
    assert!(source.starts_with("/nodes/nnnn"));
    assert!(source.chars().count() <= 97, "source not shortened");
    assert_eq!(v["osds"]["total"], 3);
    assert_eq!(v["placement_groups"]["total"], 10);
}

#[tokio::test]
async fn cap_holds_with_sixty_long_checks_through_handler() {
    let v = health_via_handler(
        Some("pve1"),
        json!({"health": {"status": "HEALTH_ERR", "checks": long_checks(60)}}),
    )
    .await;
    assert_eq!(v["health"], "HEALTH_ERR");
    assert_eq!(v["checks"][0]["severity"], "HEALTH_ERR");
    assert_eq!(v["checks"][0]["name"], "CHECK_59");
    assert!(v["checks"][0]["summary"].as_str().unwrap().ends_with('…'));
    let kept = v["checks"].as_array().unwrap().len();
    assert!(kept > 0);
    assert_eq!(v["checks_omitted"].as_u64().unwrap() as usize, 60 - kept);
    assert_eq!(v["source"], "/nodes/pve1/ceph/status");
}

#[tokio::test]
async fn cap_holds_with_everything_oversized_at_once() {
    // Long checks, many PG states, long monitor names and a long source
    // together; the health word and every numeric total must survive.
    let node = "x".repeat(300);
    let v = health_via_handler(
        Some(&node),
        json!({
            "health": {"status": "HEALTH_ERR", "checks": long_checks(60)},
            "quorum_names": (0..50).map(|i| format!("{}{}", "q".repeat(200), i)).collect::<Vec<_>>(),
            "osdmap": {"num_osds": 12, "num_up_osds": 11, "num_in_osds": 12},
            "pgmap": {"num_pgs": 32, "pgs_by_state": many_pg_states(),
                      "bytes_used": 333, "bytes_total": 1000}
        }),
    )
    .await;
    assert_eq!(v["health"], "HEALTH_ERR");
    assert_eq!(v["monitors"]["count"], 50);
    assert_eq!(v["osds"]["total"], 12);
    assert_eq!(v["osds"]["up"], 11);
    assert_eq!(v["osds"]["in"], 12);
    assert_eq!(v["placement_groups"]["total"], 32);
    assert_eq!(v["usage"]["bytes_used"], 333);
    assert_eq!(v["usage"]["bytes_total"], 1000);
    assert_eq!(v["usage"]["percent_used"], 33.3);
    let checks = v["checks"].as_array().unwrap().len();
    let names = v["monitors"]["quorum"].as_array().unwrap().len();
    let states = v["placement_groups"]["by_state"].as_object().unwrap().len();
    assert_eq!(checks + v["checks_omitted"].as_u64().unwrap() as usize, 60);
    assert_eq!(
        names + v["monitors"]["quorum_omitted"].as_u64().unwrap() as usize,
        50
    );
    assert_eq!(
        states + v["placement_groups"]["by_state_omitted"].as_u64().unwrap() as usize,
        32
    );
    assert!(v["checks_omitted"].as_u64().unwrap() > 0);
}

#[test]
fn absent_fields_are_left_out_not_null() {
    let bare = summarize_ceph_status_from(&json!({"health": {"status": "HEALTH_OK"}}), None);
    assert_eq!(bare["checks_omitted"], 0);
    assert_eq!(bare["checks"], json!([]));
    assert!(bare.get("source").is_none());
    assert!(bare["osds"].get("total").is_none());
    assert!(bare["monitors"].get("quorum").is_none());
    assert!(bare["monitors"].get("quorum_omitted").is_none());
    assert!(bare["placement_groups"].get("by_state").is_none());
    assert!(bare["placement_groups"].get("by_state_omitted").is_none());
}

#[test]
fn fixed_part_keeps_only_bounded_strings_and_numbers() {
    // A 5,000-character health word and non-numeric totals cannot blow the cap.
    let v = summarize_ceph_status_from(
        &json!({
            "health": {"status": "H".repeat(5000)},
            "osdmap": {"num_osds": "x".repeat(5000), "num_up_osds": 4, "num_in_osds": {"a": 1}},
            "pgmap": {"num_pgs": 1e300, "bytes_used": 5, "bytes_total": 10}
        }),
        Some(&"/".repeat(5000)),
    );
    let text = serde_json::to_string(&v).unwrap();
    assert!(text.len() <= CEPH_HEALTH_MAX_BYTES, "{} bytes", text.len());
    assert_eq!(v["health"].as_str().unwrap().chars().count(), 65);
    assert!(v["osds"].get("total").is_none());
    assert_eq!(v["osds"]["up"], 4);
    assert!(v["osds"].get("in").is_none());
    assert_eq!(v["usage"]["percent_used"], 50.0);
    assert_eq!(v["source"].as_str().unwrap().chars().count(), 97);
}

#[test]
fn checks_win_over_pg_states_when_both_are_large() {
    // Warnings matter more to an agent than the PG state breakdown, so with
    // both oversized some checks are kept and PG states are the ones cut.
    let v = summarize_ceph_status(&json!({
        "health": {"status": "HEALTH_WARN", "checks": long_checks(8)},
        "pgmap": {"num_pgs": 32, "pgs_by_state": many_pg_states()}
    }));
    assert!(serde_json::to_string(&v).unwrap().len() <= CEPH_HEALTH_MAX_BYTES);
    assert!(!v["checks"].as_array().unwrap().is_empty());
    assert!(v["placement_groups"]["by_state_omitted"].as_u64().unwrap() > 0);
}
