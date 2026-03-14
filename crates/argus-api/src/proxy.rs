// Rust guideline compliant 2026-02-21
//! Proxy requests to the supervisor's control API.

use anyhow::Result;
use reqwest::Client;
use serde_json::Value;

/// Forwards requests to the supervisor and returns the response body.
pub struct SupervisorProxy {
    client: Client,
    base_url: String,
}

impl SupervisorProxy {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
        }
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let body = resp.json().await?;
        Ok(body)
    }

    pub async fn post(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let mut req = self.client.post(&url);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await?;
        let result = resp.json().await?;
        Ok(result)
    }
}
