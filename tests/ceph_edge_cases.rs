// Edge-case tests for the Proxmox 9 Ceph fixes (card #16, written by Shield).
// Pure-function tests run on the helpers directly; the wiremock tests check
// the error paths through the real MCP call_tool entry point.
mod common;
use common::*;
use proxmox_mcp_rs::mcp::{flatten_ceph_osds, summarize_ceph_status};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn text_of(res: &Value) -> String {
    res["content"][0]["text"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// flatten_ceph_osds
// ---------------------------------------------------------------------------

#[test]
fn flatten_empty_tree_gives_no_osds_and_keeps_flags() {
    let v = flatten_ceph_osds(&json!({
        "flags": "sortbitwise",
        "root": {"id": "-1", "name": "default", "type": "root", "children": []}
    }))
    .unwrap();
    assert_eq!(v["flags"], "sortbitwise");
    assert_eq!(v["osds"], json!([]));
}

#[test]
fn flatten_tree_without_flags_gives_null_flags() {
    let v = flatten_ceph_osds(&json!({"root": {"type": "root", "children": []}})).unwrap();
    assert!(v["flags"].is_null());
    assert_eq!(v["osds"], json!([]));
}

#[test]
fn flatten_walks_three_levels_deep() {
    // root -> row -> host -> osd: the walk must not stop at the first level.
    let v = flatten_ceph_osds(&json!({
        "flags": "",
        "root": {"type": "root", "children": [
            {"type": "row", "children": [
                {"type": "host", "name": "hostC", "children": [
                    {"type": "osd", "id": "7", "name": "osd.7", "host": "hostC",
                     "status": "up", "in": 1}
                ]}
            ]}
        ]}
    }))
    .unwrap();
    let osds = v["osds"].as_array().unwrap();
    assert_eq!(osds.len(), 1);
    assert_eq!(osds[0]["name"], "osd.7");
    assert_eq!(osds[0]["host"], "hostC");
}

#[test]
fn flatten_skips_host_and_root_nodes_as_osds() {
    // Only type == "osd" nodes may appear in the output.
    let v = flatten_ceph_osds(&json!({
        "root": {"type": "root", "name": "default", "children": [
            {"type": "host", "name": "hostA", "children": []}
        ]}
    }))
    .unwrap();
    assert_eq!(v["osds"], json!([]));
}

#[test]
fn flatten_osd_with_missing_fields_gives_nulls_without_panic() {
    let v = flatten_ceph_osds(&json!({
        "root": {"type": "root", "children": [
            {"type": "osd"}
        ]}
    }))
    .unwrap();
    let osd = &v["osds"][0];
    for key in [
        "id",
        "name",
        "host",
        "status",
        "in",
        "device_class",
        "crush_weight",
        "percent_used",
        "pgs",
    ] {
        assert!(osd[key].is_null(), "{} should be null", key);
    }
}

#[test]
fn flatten_keeps_osd_id_as_sent_string() {
    // PVE 9 sends the id as a string ("0"); it must not be converted.
    let v = flatten_ceph_osds(&json!({
        "root": {"type": "root", "children": [
            {"type": "osd", "id": "12", "name": "osd.12"}
        ]}
    }))
    .unwrap();
    assert_eq!(v["osds"][0]["id"], "12");
}

#[test]
fn flatten_empty_plain_list_gives_no_osds() {
    let v = flatten_ceph_osds(&json!([])).unwrap();
    assert_eq!(v["osds"], json!([]));
    assert!(v["flags"].is_null());
}

#[test]
fn flatten_rejects_object_without_root_and_names_the_shape() {
    let err = flatten_ceph_osds(&json!({"flags": "x"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Unexpected Ceph OSD answer"), "{}", err);
    assert!(err.contains("without \"root\""), "{}", err);
}

#[test]
fn flatten_rejects_null_string_number_and_bool() {
    let cases = [
        (json!(null), "null"),
        (json!("osd"), "a string"),
        (json!(3), "a number"),
        (json!(true), "a boolean"),
    ];
    for (input, named) in cases {
        let err = flatten_ceph_osds(&input).unwrap_err().to_string();
        assert!(err.contains(named), "for {} got: {}", input, err);
    }
}

// ---------------------------------------------------------------------------
// summarize_ceph_status
// ---------------------------------------------------------------------------

#[test]
fn summarize_empty_object_gives_nulls_and_empty_checks() {
    let v = summarize_ceph_status(&json!({}));
    assert!(v["health"].is_null());
    assert_eq!(v["checks"], json!([]));
    assert_eq!(v["monitors"]["count"], Value::Null);
    assert!(v["monitors"]["quorum"].is_null());
    assert!(v["osds"]["total"].is_null());
    assert!(v["placement_groups"]["by_state"].is_null());
    assert!(v["usage"]["percent_used"].is_null());
}

#[test]
fn summarize_health_error_keeps_every_check_and_severity() {
    let checks: serde_json::Map<String, Value> = (0..12)
        .map(|i| {
            let sev = if i % 2 == 0 {
                "HEALTH_ERR"
            } else {
                "HEALTH_WARN"
            };
            (
                format!("CHECK_{:02}", i),
                json!({"severity": sev, "summary": {"message": format!("problem {}", i)}}),
            )
        })
        .collect();
    let v = summarize_ceph_status(&json!({"health": {"status": "HEALTH_ERR", "checks": checks}}));
    assert_eq!(v["health"], "HEALTH_ERR");
    let out = v["checks"].as_array().unwrap();
    assert_eq!(out.len(), 12);
    let errs = out.iter().filter(|c| c["severity"] == "HEALTH_ERR").count();
    assert_eq!(errs, 6);
}

#[test]
fn summarize_check_without_summary_message_gives_null_summary() {
    let v = summarize_ceph_status(&json!({"health": {"status": "HEALTH_WARN", "checks": {
        "BARE": {"severity": "HEALTH_WARN"}
    }}}));
    assert_eq!(v["checks"][0]["name"], "BARE");
    assert_eq!(v["checks"][0]["severity"], "HEALTH_WARN");
    assert!(v["checks"][0]["summary"].is_null());
}

#[test]
fn summarize_checks_that_are_not_an_object_give_empty_list() {
    let v = summarize_ceph_status(&json!({"health": {"status": "HEALTH_WARN", "checks": []}}));
    assert_eq!(v["checks"], json!([]));
}

#[test]
fn summarize_percent_is_null_when_total_is_zero() {
    let v = summarize_ceph_status(&json!({"pgmap": {"bytes_used": 0, "bytes_total": 0}}));
    assert!(v["usage"]["percent_used"].is_null());
}

#[test]
fn summarize_percent_is_null_when_bytes_missing() {
    let v = summarize_ceph_status(&json!({"pgmap": {"bytes_total": 1000}}));
    assert!(v["usage"]["percent_used"].is_null());
    assert_eq!(v["usage"]["bytes_total"], 1000);
}

#[test]
fn summarize_percent_rounds_to_one_decimal() {
    // 1 / 3 = 33.33...% must come out as 33.3
    let v = summarize_ceph_status(&json!({"pgmap": {"bytes_used": 1, "bytes_total": 3}}));
    assert_eq!(v["usage"]["percent_used"], 33.3);
}

#[test]
fn summarize_reads_nested_osdmap_from_older_ceph() {
    let v = summarize_ceph_status(&json!({"osdmap": {"osdmap": {
        "num_osds": 6, "num_up_osds": 5, "num_in_osds": 6
    }}}));
    assert_eq!(v["osds"]["total"], 6);
    assert_eq!(v["osds"]["up"], 5);
    assert_eq!(v["osds"]["in"], 6);
}

#[test]
fn summarize_skips_pg_state_entries_without_a_name() {
    let v = summarize_ceph_status(&json!({"pgmap": {"pgs_by_state": [
        {"count": 5},
        {"state_name": "active+clean", "count": 10}
    ]}}));
    let by_state = v["placement_groups"]["by_state"].as_object().unwrap();
    assert_eq!(by_state.len(), 1);
    assert_eq!(by_state["active+clean"], 10);
}

#[test]
fn summarize_pgs_by_state_that_is_not_a_list_gives_null() {
    let v = summarize_ceph_status(&json!({"pgmap": {"pgs_by_state": "oops"}}));
    assert!(v["placement_groups"]["by_state"].is_null());
}

#[test]
fn summarize_empty_quorum_gives_zero_monitors() {
    let v = summarize_ceph_status(&json!({"quorum_names": []}));
    assert_eq!(v["monitors"]["count"], 0);
    assert_eq!(v["monitors"]["quorum"], json!([]));
}

#[test]
fn summarize_huge_mgrmap_is_dropped() {
    // The raw answer is about 120 KB in production; only the summary is returned.
    let big = json!({
        "health": {"status": "HEALTH_OK"},
        "mgrmap": {"modules": vec!["x".repeat(1000); 100]}
    });
    let text = serde_json::to_string(&summarize_ceph_status(&big)).unwrap();
    assert!(text.len() < 1024, "summary is {} bytes", text.len());
}

// ---------------------------------------------------------------------------
// Size limit from the card: oversize answers are trimmed to about 1 KB.
// This is expected to FAIL until the summary bounds its check list.
// ---------------------------------------------------------------------------

#[test]
fn summarize_many_long_checks_stay_near_one_kb() {
    let checks: serde_json::Map<String, Value> = (0..60)
        .map(|i| {
            (
                format!("MODULE_CHECK_{:03}", i),
                json!({"severity": "HEALTH_WARN",
                       "summary": {"message": "y".repeat(300)}}),
            )
        })
        .collect();
    let v = summarize_ceph_status(&json!({"health": {"status": "HEALTH_WARN", "checks": checks}}));
    let text = serde_json::to_string(&v).unwrap();
    assert!(
        text.len() <= 1024,
        "summary is {} bytes for 60 checks",
        text.len()
    );
}

// ---------------------------------------------------------------------------
// Through the MCP entry point (wiremock): error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_ceph_osds_bad_shape_is_an_error_not_a_panic() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/ceph/osd"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"flags": "x"}})))
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool("list_ceph_osds", &json!({"node": "pve1"}))
        .await;
    assert!(res.is_err(), "bad OSD shape must be an error");
}

#[tokio::test]
async fn get_ceph_health_upstream_error_is_propagated() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    Mock::given(method("GET"))
        .and(path("/api2/json/cluster/ceph/status"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&mock_server)
        .await;

    let res = server.call_tool("get_ceph_health", &json!({})).await;
    assert!(
        res.is_err(),
        "a 500 from Proxmox must not look like HEALTH_OK"
    );
}

#[tokio::test]
async fn get_ceph_health_never_hits_old_501_style_path() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/ceph/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": {"health": {"status": "HEALTH_ERR"}}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api2/json/cluster/ceph/status"))
        .respond_with(ResponseTemplate::new(501))
        .expect(0)
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool("get_ceph_health", &json!({"node": "pve1"}))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text_of(&res)).unwrap();
    assert_eq!(v["health"], "HEALTH_ERR");
}
