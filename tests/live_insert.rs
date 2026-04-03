/// Integration test: Insert a test bullet into Workflowy inbox
/// This test uses the REAL Workflowy API - not mocked

use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

#[tokio::main]
async fn main() {
    // Load .env
    dotenv::dotenv().ok();

    let api_key = std::env::var("WORKFLOWY_API_KEY")
        .expect("WORKFLOWY_API_KEY must be set in .env");

    let base_url = "https://workflowy.com/api/v1";

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client");

    println!("=== Workflowy Rust API Integration Test ===");
    println!("Using API key: {}...", &api_key[..8]);

    // Step 1: Create a test node at root (inbox)
    println!("\n1. Creating test bullet in inbox...");

    let create_body = json!({
        "name": "🦀 Test from Rust MCP Server — delete me",
        "description": "Created by Rust integration test on 2026-04-03"
    });

    let response = client
        .post(&format!("{}/nodes", base_url))
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&create_body)
        .send()
        .await;

    match response {
        Ok(resp) => {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();

            println!("   Status: {}", status);
            println!("   Response: {}", body_text);

            if status.is_success() {
                // Parse response to get node ID
                if let Ok(parsed) = serde_json::from_str::<Value>(&body_text) {
                    let node_id = parsed.get("id")
                        .or_else(|| parsed.get("item_id"))
                        .or_else(|| parsed.get("nodeId"));

                    if let Some(id) = node_id {
                        println!("\n   ✅ SUCCESS! Created node with ID: {}", id);
                        println!("   Check your Workflowy inbox for: '🦀 Test from Rust MCP Server'");
                    } else {
                        println!("\n   ✅ SUCCESS! Node created (response: {})", body_text);
                    }
                } else {
                    println!("\n   ✅ SUCCESS! (status {}, raw: {})", status, body_text);
                }
            } else {
                println!("\n   ❌ FAILED with status {}", status);
                println!("   Error: {}", body_text);
                std::process::exit(1);
            }
        }
        Err(e) => {
            println!("\n   ❌ REQUEST FAILED: {}", e);
            std::process::exit(1);
        }
    }

    println!("\n=== Test Complete ===");
}
