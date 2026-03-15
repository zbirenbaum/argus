// Rust guideline compliant 2026-02-21
//! Proxy requests to the supervisor's control API.

use axum::http::StatusCode;
use reqwest::Client;
use serde_json::Value;

/// Result of a proxied request, preserving the upstream status code.
pub struct ProxyResponse {
    pub status: StatusCode,
    pub body: Value,
}

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

    pub async fn get(&self, path: &str) -> Result<ProxyResponse, String> {
        let url = format!("{}{path}", self.base_url);
        let resp = self.client.get(&url).send().await.map_err(|e| e.to_string())?;
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let body = resp.json().await.unwrap_or(serde_json::json!({ "error": "invalid upstream response" }));
        Ok(ProxyResponse { status, body })
    }

    pub async fn post(&self, path: &str, body: Option<Value>) -> Result<ProxyResponse, String> {
        let url = format!("{}{path}", self.base_url);
        let mut req = self.client.post(&url);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let body = resp.json().await.unwrap_or(serde_json::json!({ "error": "invalid upstream response" }));
        Ok(ProxyResponse { status, body })
    }
}
