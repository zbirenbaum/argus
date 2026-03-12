//! Network event payloads.

use serde::{Deserialize, Serialize};

/// A socket was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Socket {
    pub pid: u32,
    pub domain: String,
    pub sock_type: String,
    pub fd: i32,
}

/// A connection was initiated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connect {
    pub pid: u32,
    pub fd: i32,
    pub remote_addr: String,
    pub remote_port: u16,
}

/// An incoming connection was accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accept {
    pub pid: u32,
    pub fd: i32,
    pub peer_addr: String,
    pub peer_port: u16,
}

/// TLS key material was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsKeys {
    pub pid: u32,
    pub fd: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keylog_line_hash: Option<String>,
}

/// An HTTP request was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub pid: u32,
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
}

/// An HTTP response was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub pid: u32,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_round_trip() {
        let s = Socket {
            pid: 1,
            domain: "AF_INET".into(),
            sock_type: "SOCK_STREAM".into(),
            fd: 5,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Socket = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn connect_round_trip() {
        let c = Connect {
            pid: 1,
            fd: 5,
            remote_addr: "10.0.0.1".into(),
            remote_port: 443,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Connect = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn accept_round_trip() {
        let a = Accept {
            pid: 1,
            fd: 6,
            peer_addr: "192.168.1.100".into(),
            peer_port: 52301,
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: Accept = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn tls_keys_round_trip() {
        let t = TlsKeys {
            pid: 1,
            fd: 5,
            sni: Some("api.example.com".into()),
            keylog_line_hash: Some("deadbeef".into()),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: TlsKeys = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn http_request_round_trip() {
        let r = HttpRequest {
            pid: 1,
            method: "POST".into(),
            url: "https://api.example.com/data".into(),
            headers_hash: Some("h1".into()),
            body_hash: Some("b1".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: HttpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn http_response_round_trip() {
        let r = HttpResponse {
            pid: 1,
            status: 404,
            headers_hash: None,
            body_hash: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("headers_hash"));
        let back: HttpResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn tls_keys_none_fields_omitted() {
        let t = TlsKeys {
            pid: 1,
            fd: 3,
            sni: None,
            keylog_line_hash: None,
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.contains("sni"));
        assert!(!json.contains("keylog_line_hash"));
    }

    #[test]
    fn http_request_none_fields_omitted() {
        let r = HttpRequest {
            pid: 1,
            method: "GET".into(),
            url: "https://example.com".into(),
            headers_hash: None,
            body_hash: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("headers_hash"));
        assert!(!json.contains("body_hash"));
    }
}
