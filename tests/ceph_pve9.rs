mod common;
use common::*;
use proxmox_mcp_rs::mcp::JsonRpcRequest;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn text_of(res: &Value) -> String {
    res["content"][0]["text"].as_str().unwrap().to_string()
}

fn osd(id: u32, host: &str, status: &str, in_: u32) -> Value {
    json!({
        "id": id.to_string(), "name": format!("osd.{}", id), "host": host,
        "type": "osd", "leaf": 1, "status": status, "in": in_,
        "device_class": "nvme", "crush_weight": 0.2, "percent_used": 50.5,
        "pgs": 64, "ceph_version": "ceph version 19.2.6", "blfsdev": "/dev/dm-0"
    })
}

#[tokio::test]
async fn test_list_ceph_pools_uses_pool_path() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/ceph/pools"))
        .respond_with(ResponseTemplate::new(501))
        .expect(0)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/ceph/pool"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            {"pool": 1, "pool_name": ".mgr", "size": 2, "min_size": 2},
            {"pool": 2, "pool_name": "rbd", "size": 2, "min_size": 2}
        ]})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool("list_ceph_pools", &json!({"node": "pve1"}))
        .await
        .unwrap();
    let pools: Value = serde_json::from_str(&text_of(&res)).unwrap();
    assert_eq!(pools.as_array().unwrap().len(), 2);
    assert_eq!(pools[1]["pool_name"], "rbd");
    assert_eq!(pools[1]["min_size"], 2);
}

#[tokio::test]
async fn test_list_ceph_osds_flattens_tree() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    let tree = json!({
        "flags": "sortbitwise,recovery_deletes",
        "root": {
            "id": "-1", "name": "default", "type": "root", "leaf": 0,
            "children": [
                {"id": "-2", "name": "hostA", "type": "host", "leaf": 0,
                 "children": [osd(0, "hostA", "up", 1), osd(1, "hostA", "down", 1)]},
                {"id": "-3", "name": "hostB", "type": "host", "leaf": 0,
                 "children": [osd(2, "hostB", "up", 0), osd(3, "hostB", "up", 1)]}
            ]
        }
    });
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/ceph/osd"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": tree})))
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool("list_ceph_osds", &json!({"node": "pve1"}))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text_of(&res)).unwrap();
    assert_eq!(v["flags"], "sortbitwise,recovery_deletes");
    let osds = v["osds"].as_array().unwrap();
    assert_eq!(osds.len(), 4);
    assert_eq!(osds[1]["name"], "osd.1");
    assert_eq!(osds[1]["status"], "down");
    assert_eq!(osds[2]["host"], "hostB");
    assert_eq!(osds[2]["in"], 0);
    // Only the compact fields are kept.
    assert!(osds[0].get("ceph_version").is_none());
    assert_eq!(osds[0]["pgs"], 64);
}

#[tokio::test]
async fn test_list_ceph_osds_accepts_list() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/ceph/osd"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": 0, "name": "osd.0"}]})),
        )
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool("list_ceph_osds", &json!({"node": "pve1"}))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text_of(&res)).unwrap();
    assert_eq!(v["osds"].as_array().unwrap().len(), 1);
    assert_eq!(v["osds"][0]["name"], "osd.0");
}

#[tokio::test]
async fn test_get_ceph_health_cluster() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    let status = json!({
        "health": {
            "status": "HEALTH_WARN",
            "checks": {
                "OSD_NEARFULL": {"severity": "HEALTH_WARN", "muted": false,
                    "summary": {"count": 1, "message": "1 nearfull osd(s)"},
                    "detail": [{"message": "osd.3 is near full"}]},
                "POOL_NO_REDUNDANCY": {"severity": "HEALTH_WARN", "muted": false,
                    "summary": {"count": 1, "message": "1 pool(s) have no replicas configured"}}
            }
        },
        "quorum_names": ["pve1", "pve2", "pve3"],
        "osdmap": {"num_osds": 4, "num_up_osds": 3, "num_in_osds": 4},
        "pgmap": {
            "num_pgs": 129,
            "pgs_by_state": [
                {"state_name": "active+clean", "count": 128},
                {"state_name": "active+clean+scrubbing", "count": 1}
            ],
            "bytes_used": 250, "bytes_total": 1000
        },
        "mgrmap": {"filler": "x".repeat(100_000)}
    });
    Mock::given(method("GET"))
        .and(path("/api2/json/cluster/ceph/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": status})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool("get_ceph_health", &json!({}))
        .await
        .unwrap();
    let text = text_of(&res);
    assert!(text.len() < 4000, "summary is {} bytes", text.len());
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["health"], "HEALTH_WARN");
    assert_eq!(v["checks"].as_array().unwrap().len(), 2);
    assert_eq!(v["checks"][0]["name"], "OSD_NEARFULL");
    assert_eq!(v["checks"][0]["severity"], "HEALTH_WARN");
    assert_eq!(v["checks"][0]["summary"], "1 nearfull osd(s)");
    assert_eq!(v["monitors"]["count"], 3);
    assert_eq!(v["monitors"]["quorum"][2], "pve3");
    assert_eq!(v["osds"]["total"], 4);
    assert_eq!(v["osds"]["up"], 3);
    assert_eq!(v["osds"]["in"], 4);
    assert_eq!(v["placement_groups"]["total"], 129);
    assert_eq!(v["placement_groups"]["by_state"]["active+clean"], 128);
    assert_eq!(v["usage"]["percent_used"], 25.0);
    assert_eq!(v["source"], "/cluster/ceph/status");
}

#[tokio::test]
async fn test_get_ceph_health_node() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/ceph/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": {"health": {"status": "HEALTH_OK"}}})),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool("get_ceph_health", &json!({"node": "pve1"}))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text_of(&res)).unwrap();
    assert_eq!(v["health"], "HEALTH_OK");
    assert_eq!(v["checks"].as_array().unwrap().len(), 0);
    assert!(v["osds"]["total"].is_null());
    assert!(v["monitors"]["count"].is_null());
    assert!(v["placement_groups"]["total"].is_null());
    assert!(v["usage"]["percent_used"].is_null());
    assert_eq!(v["source"], "/nodes/pve1/ceph/status");
}

#[tokio::test]
async fn test_get_ceph_health_is_listed() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    let req: JsonRpcRequest = serde_json::from_value(json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list"
    }))
    .unwrap();
    let res = server.handle_request(req).await.unwrap();
    let tools = res["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|t| t["name"] == "get_ceph_health")
        .expect("get_ceph_health is advertised");
    assert_eq!(tool["inputSchema"]["required"], json!([]));
    assert!(tool["inputSchema"]["properties"]["node"].is_object());
}

#[tokio::test]
async fn test_ceph_pool_write_paths() {
    let mock_server = MockServer::start().await;
    let server = setup_mcp_server(&mock_server).await;

    Mock::given(method("POST"))
        .and(path("/api2/json/nodes/pve1/ceph/pool"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": "UPID:create"})))
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api2/json/nodes/pve1/ceph/pool/test-pool"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": "UPID:delete"})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let res = server
        .call_tool(
            "manage_cluster_config",
            &json!({"type": "ceph", "action": "create", "node": "pve1", "name": "test-pool"}),
        )
        .await
        .unwrap();
    assert!(text_of(&res).contains("UPID:create"));
    let res = server
        .call_tool(
            "manage_cluster_config",
            &json!({"type": "ceph", "action": "delete", "node": "pve1", "name": "test-pool"}),
        )
        .await
        .unwrap();
    assert!(text_of(&res).contains("UPID:delete"));
}
