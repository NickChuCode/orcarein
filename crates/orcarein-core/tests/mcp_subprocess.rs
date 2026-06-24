#![cfg(feature = "mcp")]

use orcarein_core::config::McpServerConfig;
use orcarein_core::mcp::setup_servers;
use orcarein_core::ToolRegistry;

#[tokio::test]
async fn registers_and_calls_a_real_subprocess_mcp_server() {
    let cfg = McpServerConfig {
        name: "mock".into(),
        command: env!("CARGO_BIN_EXE_mock_mcp_server").into(),
        args: vec![],
        env: Default::default(),
    };
    let mut registry = ToolRegistry::new();
    let clients = setup_servers(std::slice::from_ref(&cfg), &mut registry).await;
    assert_eq!(clients.len(), 1, "server should connect");

    assert!(registry.names().contains(&"mcp__mock__echo"));

    let tool = registry.get("mcp__mock__echo").expect("tool registered");
    let out = tool.execute(serde_json::json!({"x": 1})).await.unwrap();
    assert_eq!(out.content, "mock-ok");
}
