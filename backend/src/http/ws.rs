//! WebSocket 状态推送（http/ws.rs）
//!
//! 前端连接 /ws 端点，订阅内部事件总线，实时接收备份/恢复任务状态。

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tokio::sync::broadcast;

use crate::eventbus::{DomainEvent, EventBus};
use crate::AppState;

/// WebSocket 升级处理器
pub async fn ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let eventbus = state.eventbus.clone();
    ws.on_upgrade(move |socket| handle_socket(socket, eventbus))
}

/// 处理 WebSocket 连接：订阅事件总线并实时推送
async fn handle_socket(mut socket: WebSocket, eventbus: Arc<EventBus>) {
    let mut rx: broadcast::Receiver<Arc<DomainEvent>> = eventbus.subscribe();

    // 立即推送一条 hello/订阅确认
    let _ = socket
        .send(Message::Text(
            json!({"type": "connected", "message": "已连接事件流"}).to_string(),
        ))
        .await;

    loop {
        // 1) 读取客户端消息（心跳/关闭），并处理事件
        tokio::select! {
            // 客户端消息
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Text(t))) if t == "ping" => {
                        let _ = socket.send(Message::Text("pong".to_string())).await;
                    }
                    _ => {}
                }
            }
            // 事件总线推送
            evt = rx.recv() => {
                match evt {
                    Ok(event) => {
                        let payload = json!({
                            "type": "event",
                            "kind": event.kind,
                            "status": event.status,
                            "job_id": event.job_id,
                            "current_file": event.current_file,
                            "done": event.done,
                            "total": event.total,
                            "bytes_done": event.bytes_done,
                            "bytes_total": event.bytes_total,
                            "elapsed_ms": event.elapsed_ms,
                            "message": event.message,
                            "ts": event.ts,
                        });
                        if socket.send(Message::Text(payload.to_string())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // 跳过被跳过的历史事件
                        continue;
                    }
                    Err(_) => break,
                }
            }
        }
    }
}
