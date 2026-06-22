//! DashScope WebSocket protocol client for TTS and ASR.
//!
//! DashScope's CosyVoice (TTS) and Paraformer/Fun-ASR (ASR) models require
//! WebSocket connections — HTTP endpoints return "url error" with 百炼 platform
//! API keys. This module implements the raw WS protocol so aginxbrain can proxy
//! TTS and ASR requests without requiring the DashScope SDK.

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const DEFAULT_WS_URL: &str = "wss://dashscope.aliyuncs.com/api-ws/v1/inference";
const WS_TIMEOUT_SECS: u64 = 60;

// ─── TTS ────────────────────────────────────────────────────────────────────

/// Parameters for a TTS (text-to-speech) request.
pub struct TtsParams {
    pub text: String,
    pub model: String,
    pub voice: String,
    pub format: String,
    pub sample_rate: u32,
}

/// Call DashScope CosyVoice TTS via WebSocket. Returns concatenated audio bytes.
pub async fn tts_via_websocket(
    ws_url: &str,
    api_key: &str,
    params: &TtsParams,
) -> Result<Vec<u8>> {
    let task_id = generate_task_id();
    let (mut tx, mut rx) = connect_dashscope_ws(ws_url, api_key).await?;

    // 1. Send run-task
    let run_task = json!({
        "header": {
            "action": "run-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "task_group": "audio",
            "task": "tts",
            "function": "SpeechSynthesizer",
            "model": params.model,
            "parameters": {
                "voice": params.voice,
                "format": params.format,
                "sample_rate": params.sample_rate
            },
            "input": {}
        }
    });
    send_json(&mut tx, &run_task).await?;

    // 2. Wait for task-started
    wait_event(&mut rx, "task-started", &task_id).await?;

    // 3. Send continue-task with text
    let continue_task = json!({
        "header": {
            "action": "continue-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "input": {
                "text": params.text
            }
        }
    });
    send_json(&mut tx, &continue_task).await?;

    // 4. Send finish-task
    let finish_task = json!({
        "header": {
            "action": "finish-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "input": {}
        }
    });
    send_json(&mut tx, &finish_task).await?;

    // 5. Collect audio bytes and wait for task-finished
    let mut audio_bytes = Vec::new();
    let timeout = std::time::Duration::from_secs(WS_TIMEOUT_SECS);

    loop {
        let msg = tokio::time::timeout(timeout, rx.next())
            .await
            .context("TTS WebSocket read timeout")?
            .context("TTS WebSocket closed unexpectedly")?;

        match msg {
            Ok(Message::Text(text)) => {
                let evt: Value = serde_json::from_str(&text)
                    .context("TTS: invalid JSON from server")?;
                let event_name = evt.pointer("/header/event")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match event_name {
                    "task-finished" => break,
                    "task-failed" => {
                        let code = evt.pointer("/header/error_code")
                            .and_then(|v| v.as_str()).unwrap_or("Unknown");
                        let msg = evt.pointer("/header/error_message")
                            .and_then(|v| v.as_str()).unwrap_or("");
                        anyhow::bail!("TTS task-failed: {} - {}", code, msg);
                    }
                    "result-generated" => {
                        log::debug!("[TTS-WS] result-generated: {}",
                            evt.pointer("/payload/output/type")
                                .and_then(|v| v.as_str()).unwrap_or("?"));
                    }
                    _ => {
                        log::debug!("[TTS-WS] unexpected event: {}", event_name);
                    }
                }
            }
            Ok(Message::Binary(data)) => {
                audio_bytes.extend_from_slice(&data);
            }
            Ok(Message::Close(frame)) => {
                log::warn!("[TTS-WS] connection closed: {:?}", frame);
                break;
            }
            Ok(_) => {} // Ping/Pong — ignore
            Err(e) => anyhow::bail!("TTS WebSocket error: {}", e),
        }
    }

    let _ = tx.close().await;
    Ok(audio_bytes)
}

// ─── ASR ────────────────────────────────────────────────────────────────────

/// Parameters for an ASR (speech recognition) request.
pub struct AsrParams {
    pub audio_bytes: Vec<u8>,
    pub model: String,
    pub format: String,
    pub sample_rate: u32,
}

/// Call DashScope Paraformer/Fun-ASR via WebSocket. Returns recognized text.
pub async fn asr_via_websocket(
    ws_url: &str,
    api_key: &str,
    params: &AsrParams,
) -> Result<String> {
    let task_id = generate_task_id();
    let (mut tx, mut rx) = connect_dashscope_ws(ws_url, api_key).await?;

    // 1. Send run-task
    let run_task = json!({
        "header": {
            "action": "run-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "task_group": "audio",
            "task": "asr",
            "function": "recognition",
            "model": params.model,
            "parameters": {
                "format": params.format,
                "sample_rate": params.sample_rate
            },
            "input": {}
        }
    });
    send_json(&mut tx, &run_task).await?;

    // 2. Wait for task-started
    wait_event(&mut rx, "task-started", &task_id).await?;

    // 3. Send audio as binary frames (~8KB each)
    let chunk_size = 8192;
    for chunk in params.audio_bytes.chunks(chunk_size) {
        tx.send(Message::Binary(chunk.to_vec().into()))
            .await
            .context("ASR: failed to send audio chunk")?;
    }

    // 4. Send finish-task
    let finish_task = json!({
        "header": {
            "action": "finish-task",
            "task_id": task_id,
            "streaming": "duplex"
        },
        "payload": {
            "input": {}
        }
    });
    send_json(&mut tx, &finish_task).await?;

    // 5. Collect recognized text
    let mut result_text = String::new();
    let timeout = std::time::Duration::from_secs(WS_TIMEOUT_SECS);

    loop {
        let msg = tokio::time::timeout(timeout, rx.next())
            .await
            .context("ASR WebSocket read timeout")?
            .context("ASR WebSocket closed unexpectedly")?;

        match msg {
            Ok(Message::Text(text)) => {
                let evt: Value = serde_json::from_str(&text)
                    .context("ASR: invalid JSON from server")?;
                let event_name = evt.pointer("/header/event")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match event_name {
                    "task-finished" => break,
                    "task-failed" => {
                        let code = evt.pointer("/header/error_code")
                            .and_then(|v| v.as_str()).unwrap_or("Unknown");
                        let msg = evt.pointer("/header/error_message")
                            .and_then(|v| v.as_str()).unwrap_or("");
                        anyhow::bail!("ASR task-failed: {} - {}", code, msg);
                    }
                    "result-generated" => {
                        // Extract text from result
                        let sentence = evt.pointer("/payload/output/sentence");
                        if let Some(s) = sentence {
                            // Skip heartbeat sentences
                            let is_heartbeat = s.get("heartbeat")
                                .and_then(|v| v.as_bool()).unwrap_or(false);
                            if is_heartbeat {
                                continue;
                            }
                            if let Some(text) = s.get("text").and_then(|v| v.as_str()) {
                                // Prefer final results (sentence_end=true) but also
                                // accept intermediate results for responsiveness
                                let is_final = s.get("sentence_end")
                                    .and_then(|v| v.as_bool()).unwrap_or(false);
                                if is_final {
                                    // Replace with final result (may correct intermediate)
                                    if !result_text.is_empty() {
                                        result_text = text.to_string();
                                    } else {
                                        result_text = text.to_string();
                                    }
                                } else if result_text.is_empty() {
                                    // First intermediate result
                                    result_text = text.to_string();
                                }
                                // For intermediate results that aren't the first,
                                // we wait for the final sentence_end version
                            }
                        }
                    }
                    _ => {
                        log::debug!("[ASR-WS] unexpected event: {}", event_name);
                    }
                }
            }
            Ok(Message::Binary(_)) => {
                // ASR doesn't send binary to client — ignore
            }
            Ok(Message::Close(frame)) => {
                log::warn!("[ASR-WS] connection closed: {:?}", frame);
                break;
            }
            Ok(_) => {}
            Err(e) => anyhow::bail!("ASR WebSocket error: {}", e),
        }
    }

    let _ = tx.close().await;
    Ok(result_text)
}

// ─── Shared helpers ────────────────────────────────────────────────────────

fn generate_task_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

type WsSplit = (
    futures::stream::SplitSink<WsStream, Message>,
    futures::stream::SplitStream<WsStream>,
);

/// Connect to DashScope WS endpoint with Authorization header.
async fn connect_dashscope_ws(ws_url: &str, api_key: &str) -> Result<WsSplit> {
    let url = if ws_url.is_empty() {
        DEFAULT_WS_URL
    } else {
        ws_url
    };
    let mut request = url
        .into_client_request()
        .context("invalid WebSocket URL")?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", api_key)
            .parse()
            .context("invalid API key for Authorization header")?,
    );
    let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .context("WebSocket connection failed")?;
    log::info!("[WS] connected to {}", url);
    Ok(ws_stream.split())
}

/// Send a JSON message on the WS sink.
async fn send_json(
    tx: &mut futures::stream::SplitSink<WsStream, Message>,
    value: &Value,
) -> Result<()> {
    let text = serde_json::to_string(value)?;
    tx.send(Message::Text(text.into()))
        .await
        .context("WebSocket send failed")
}

/// Wait for a specific event from the WS stream. Returns the full event JSON.
async fn wait_event(
    rx: &mut futures::stream::SplitStream<WsStream>,
    expected_event: &str,
    expected_task_id: &str,
) -> Result<Value> {
    let timeout = std::time::Duration::from_secs(WS_TIMEOUT_SECS);
    let msg = tokio::time::timeout(timeout, rx.next())
        .await
        .context(format!("WS timeout waiting for {}", expected_event))?
        .context("WS closed while waiting for event")?;

    match msg {
        Ok(Message::Text(text)) => {
            let evt: Value = serde_json::from_str(&text)?;
            let event = evt.pointer("/header/event")
                .and_then(|v| v.as_str()).unwrap_or("");
            let action = evt.pointer("/header/action")
                .and_then(|v| v.as_str()).unwrap_or("");
            let tid = evt.pointer("/header/task_id")
                .and_then(|v| v.as_str()).unwrap_or("");

            // DashScope uses "event" for server->client and "action" for some responses
            if (event == expected_event || action == expected_event)
                && (tid == expected_task_id || tid.is_empty())
            {
                Ok(evt)
            } else if event == "task-failed" {
                let code = evt.pointer("/header/error_code")
                    .and_then(|v| v.as_str()).unwrap_or("Unknown");
                let msg = evt.pointer("/header/error_message")
                    .and_then(|v| v.as_str()).unwrap_or("");
                anyhow::bail!("task-failed while waiting for {}: {} - {}",
                    expected_event, code, msg)
            } else {
                log::debug!("[WS] unexpected event: event={} action={} task_id={}", event, action, tid);
                Ok(evt)
            }
        }
        other => {
            anyhow::bail!("expected JSON event '{}', got: {:?}", expected_event, other)
        }
    }
}
