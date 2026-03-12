// Rust guideline compliant 2026-02-21
//! HTTP client for the Argus supervisor REST API.
//!
//! Wraps `reqwest` and provides typed methods for each API endpoint.
//! All methods return `anyhow::Result` and translate HTTP errors into
//! human-readable messages.

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;

use argus::api::types::{
    ApproveResponse, DenyResponse, HealthResponse, PauseResponse,
    PendingApprovalsResponse, ResumeResponse, StatusResponse,
};

use crate::types::{
    AgentsResponse, ConnectionsResponse, CorrelationResponse,
    FileHistoryResponse, PipelineResponse, ProcessTreeNode, RestoreRequest,
    RestoreResponse, RulesAppliedResponse, RulesResponse,
    StorageStatusResponse, StdioResponse, TreeDiffResponse, TreeResponse,
    UndoRequest,
};

/// HTTP client targeting a single supervisor instance.
#[derive(Debug)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    /// Create a client pointing at the given base URL.
    pub fn new(base_url: String) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_owned(),
            http: reqwest::Client::new(),
        }
    }

    // -- Agent Control -------------------------------------------------------

    pub async fn status(&self) -> Result<StatusResponse> {
        self.get_json("/agent/status").await
    }

    pub async fn pause(&self) -> Result<PauseResponse> {
        self.post_json("/agent/pause").await
    }

    pub async fn resume(&self) -> Result<ResumeResponse> {
        self.post_json("/agent/resume").await
    }

    pub async fn health(&self) -> Result<HealthResponse> {
        self.get_json("/health").await
    }

    // -- Events --------------------------------------------------------------

    /// Fetch events as streaming JSONL text.
    pub async fn events(&self, params: &[(&str, String)]) -> Result<String> {
        let url = self.url("/events");
        let resp = self.http.get(&url).query(params).send().await?;
        check_status(&url, resp).await?.text().await.map_err(Into::into)
    }

    pub async fn file_history(&self, path: &str) -> Result<FileHistoryResponse> {
        let url = self.url("/file_history");
        let resp = self.http.get(&url).query(&[("path", path)]).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    // -- Processes & Stdio ---------------------------------------------------

    pub async fn stdio(&self, pid: u32, stream: Option<&str>) -> Result<StdioResponse> {
        let url = self.url("/stdio");
        let mut params: Vec<(&str, String)> = vec![("pid", pid.to_string())];
        if let Some(s) = stream {
            params.push(("stream", s.to_owned()));
        }
        let resp = self.http.get(&url).query(&params).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    /// Start an SSE stream for stdio follow mode. Returns the raw response.
    pub async fn stdio_follow(
        &self,
        pid: u32,
        stream: &str,
    ) -> Result<reqwest::Response> {
        let url = self.url("/stdio");
        let params = [
            ("pid", pid.to_string()),
            ("stream", stream.to_owned()),
            ("follow", "true".to_owned()),
        ];
        let resp = self.http.get(&url).query(&params).send().await?;
        check_status(&url, resp).await
    }

    pub async fn pipeline(&self, shell_pid: u32) -> Result<PipelineResponse> {
        let url = self.url("/pipeline");
        let resp = self.http.get(&url).query(&[("shell_pid", shell_pid)]).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    pub async fn process_tree(
        &self,
        root: Option<u32>,
        stdio: bool,
        depth: Option<u32>,
    ) -> Result<ProcessTreeNode> {
        let url = self.url("/process_tree");
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(r) = root {
            params.push(("root", r.to_string()));
        }
        if stdio {
            params.push(("stdio", "true".to_owned()));
        }
        if let Some(d) = depth {
            params.push(("depth", d.to_string()));
        }
        let resp = self.http.get(&url).query(&params).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    // -- Content -------------------------------------------------------------

    /// Fetch CAS object as UTF-8 text.
    pub async fn cat(&self, hash: &str) -> Result<String> {
        let url = self.url(&format!("/content/{hash}/text"));
        let resp = self.http.get(&url).send().await?;
        check_status(&url, resp).await?.text().await.map_err(Into::into)
    }

    /// Fetch raw CAS object bytes.
    pub async fn content_raw(&self, hash: &str) -> Result<bytes::Bytes> {
        let url = self.url(&format!("/content/{hash}"));
        let resp = self.http.get(&url).send().await?;
        check_status(&url, resp).await?.bytes().await.map_err(Into::into)
    }

    /// Fetch unified diff between two content hashes.
    pub async fn diff(&self, before: &str, after: &str) -> Result<String> {
        let url = self.url("/diff");
        let params = [
            ("before_hash", before),
            ("after_hash", after),
            ("format", "unified"),
        ];
        let resp = self.http.get(&url).query(&params).send().await?;
        check_status(&url, resp).await?.text().await.map_err(Into::into)
    }

    /// Fetch tree diff between two sequence numbers.
    pub async fn tree_diff(&self, from: u64, to: u64) -> Result<TreeDiffResponse> {
        let url = self.url("/tree/diff");
        let resp = self
            .http
            .get(&url)
            .query(&[("from_seq", from), ("to_seq", to)])
            .send()
            .await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    // -- Snapshots & Restore -------------------------------------------------

    pub async fn tree(
        &self,
        seq: Option<u64>,
        path_prefix: Option<&str>,
    ) -> Result<TreeResponse> {
        let url = self.url("/tree");
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(s) = seq {
            params.push(("seq", s.to_string()));
        }
        if let Some(p) = path_prefix {
            params.push(("path_prefix", p.to_owned()));
        }
        let resp = self.http.get(&url).query(&params).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    pub async fn restore(&self, req: &RestoreRequest) -> Result<RestoreResponse> {
        let url = self.url("/restore");
        let resp = self.http.post(&url).json(req).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    pub async fn restore_undo(&self, req: &UndoRequest) -> Result<RestoreResponse> {
        let url = self.url("/restore/undo");
        let resp = self.http.post(&url).json(req).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    // -- Network & Storage ---------------------------------------------------

    pub async fn connections(
        &self,
        pid: Option<u32>,
        active_only: bool,
    ) -> Result<ConnectionsResponse> {
        let url = self.url("/connections");
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(p) = pid {
            params.push(("pid", p.to_string()));
        }
        if active_only {
            params.push(("active_only", "true".to_owned()));
        }
        let resp = self.http.get(&url).query(&params).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    pub async fn storage_status(&self) -> Result<StorageStatusResponse> {
        self.get_json("/storage/status").await
    }

    // -- Rules ---------------------------------------------------------------

    pub async fn rules(&self) -> Result<RulesResponse> {
        self.get_json("/rules").await
    }

    /// Replace the entire ruleset. Body is a JSON value read from a file.
    pub async fn rules_set(&self, body: serde_json::Value) -> Result<RulesAppliedResponse> {
        let url = self.url("/rules");
        let resp = self.http.post(&url).json(&body).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    pub async fn rules_remove(&self, index: u64) -> Result<RulesAppliedResponse> {
        let url = self.url(&format!("/rules/{index}"));
        let resp = self.http.delete(&url).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    // -- Approvals -----------------------------------------------------------

    pub async fn pending_approvals(&self) -> Result<PendingApprovalsResponse> {
        self.get_json("/approvals/pending").await
    }

    pub async fn approve(&self, action_id: &str) -> Result<ApproveResponse> {
        let url = self.url(&format!("/approvals/{action_id}/approve"));
        let resp = self.http.post(&url).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    pub async fn deny(&self, action_id: &str) -> Result<DenyResponse> {
        let url = self.url(&format!("/approvals/{action_id}/deny"));
        let resp = self.http.post(&url).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    // -- Cross-Agent ---------------------------------------------------------

    pub async fn agents(&self) -> Result<AgentsResponse> {
        self.get_json("/agents").await
    }

    /// Fetch cross-agent timeline as streaming JSONL text.
    pub async fn timeline(&self, params: &[(&str, String)]) -> Result<String> {
        let url = self.url("/timeline");
        let resp = self.http.get(&url).query(params).send().await?;
        check_status(&url, resp).await?.text().await.map_err(Into::into)
    }

    pub async fn correlate(
        &self,
        write_agent: &str,
        read_agent: &str,
        resource: Option<&str>,
    ) -> Result<CorrelationResponse> {
        let url = self.url("/correlation");
        let mut params = vec![
            ("write_agent", write_agent.to_owned()),
            ("read_agent", read_agent.to_owned()),
        ];
        if let Some(r) = resource {
            params.push(("resource", r.to_owned()));
        }
        let resp = self.http.get(&url).query(&params).send().await?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    // -- Helpers -------------------------------------------------------------

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.url(path);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }

    async fn post_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.url(path);
        let resp = self.http.post(&url).send().await
            .with_context(|| format!("POST {url}"))?;
        check_status(&url, resp).await?.json().await.map_err(Into::into)
    }
}

/// Check HTTP status and return a descriptive error on failure.
async fn check_status(url: &str, resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    match status {
        StatusCode::NOT_FOUND => bail!("not found: {url}"),
        StatusCode::CONFLICT => bail!("{body}"),
        _ => bail!("HTTP {status} from {url}: {body}"),
    }
}
