use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 request sent to the Python sidecar.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: &str, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 2.0 response from the Python sidecar.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Returns Ok(result_value) or Err(error_message).
    pub fn into_result(self) -> Result<serde_json::Value, String> {
        if let Some(err) = self.error {
            Err(format!("RPC error {}: {}", err.code, err.message))
        } else {
            Ok(self.result.unwrap_or(serde_json::Value::Null))
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}
