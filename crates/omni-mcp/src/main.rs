use anyhow::Result;
use omni_core::OmniExtractionResult;
use omni_vision::OmniVisionEngine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        if let Ok(req) = serde_json::from_str::<JsonRpcRequest>(trimmed) {
            let response = handle_request(&req).await;
            if let Ok(json_resp) = serde_json::to_string(&response) {
                stdout.write_all(json_resp.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }

        line.clear();
    }

    Ok(())
}

async fn handle_request(req: &JsonRpcRequest) -> JsonRpcResponse {
    let jsonrpc = req.jsonrpc.clone();
    let id = req.id.clone();

    match req.method.as_str() {
        "initialize" => JsonRpcResponse {
            jsonrpc,
            id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "firefly-omni-mcp", "version": "0.1.0" }
            })),
            error: None,
        },
        "tools/list" => JsonRpcResponse {
            jsonrpc,
            id,
            result: Some(json!({
                "tools": [
                    {
                        "name": "omni_detect_mime",
                        "description": "Detect precise file MIME type using Google Magika neural network",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Absolute path to target file" }
                            },
                            "required": ["path"]
                        }
                    },
                    {
                        "name": "omni_compute_phash",
                        "description": "Compute 64-bit perceptual hash (pHash) for image duplicate detection",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Absolute path to image file" }
                            },
                            "required": ["path"]
                        }
                    },
                    {
                        "name": "omni_check_corrupted",
                        "description": "Verify whether a media or archive file is corrupted",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "Absolute path to file" }
                            },
                            "required": ["path"]
                        }
                    }
                ]
            })),
            error: None,
        },
        "tools/call" => {
            let params = req.params.clone().unwrap_or(json!({}));
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let result = handle_tool_call(tool_name, &arguments).await;
            match result {
                Ok(val) => JsonRpcResponse {
                    jsonrpc,
                    id,
                    result: Some(json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&val).unwrap_or_default() }] })),
                    error: None,
                },
                Err(err) => JsonRpcResponse {
                    jsonrpc,
                    id,
                    result: None,
                    error: Some(json!({ "code": -32603, "message": err.to_string() })),
                },
            }
        }
        _ => JsonRpcResponse {
            jsonrpc,
            id,
            result: None,
            error: Some(json!({ "code": -32601, "message": "Method not found" })),
        },
    }
}

async fn handle_tool_call(name: &str, args: &Value) -> Result<Value> {
    match name {
        "omni_detect_mime" => {
            let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
            let mime = OmniVisionEngine::detect_mime_type(path_str)?;
            Ok(json!({ "path": path_str, "mime": mime, "source": "OmniVisionEngine" }))
        }
        "omni_compute_phash" => {
            let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
            let phash = OmniExtractionResult::compute_phash(path_str);
            Ok(json!({ "path": path_str, "phash": phash }))
        }
        "omni_check_corrupted" => {
            let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
            let is_corrupted = OmniExtractionResult::check_corrupted(path_str);
            Ok(json!({ "path": path_str, "is_corrupted": is_corrupted }))
        }
        _ => Err(anyhow::anyhow!("Unknown tool name: {}", name)),
    }
}
