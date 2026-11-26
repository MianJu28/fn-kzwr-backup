//! WebSocket 状态推送（http/ws.rs）
//!
//! 前端连接 /ws 端点，订阅内部事件总线，实时接收备份/恢复任务状态。

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tokio::sync::broadcast;

use crate::eventbus::{DomainEvent, EventBus, TaskKind};
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

    // **补一帧当前状态**（关键）：广播只推增量事件，客户端连上之前的进度全丢。
    // 用户刷新页面 / 退出应用再进来时 WebView 会重建、WS 重连，若不补这一帧，
    // 界面就显示「没有任务在跑」，而后台其实还在跑（这正是「任务没了」的成因）。
    for kind in [TaskKind::Backup, TaskKind::Restore] {
        if let Some(ev) = eventbus.last_snapshot(kind) {
            let _ = socket.send(Message::Text(event_payload(&ev).to_string())).await;
        }
    }

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
                        let payload = event_payload(&event);
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

/// 把领域事件转成前端消费的 WS 消息（`{"type":"event", ...}`）
///
/// 实时推送与「连接时补一帧快照」**必须共用同一形状**：两处若各写一份，
/// 补发的那一帧就会缺字段（前端读出 `undefined`），表现为重连后进度条错乱。
fn event_payload(event: &DomainEvent) -> serde_json::Value {
    json!({
        "type": "event",
        "kind": event.kind,
        "status": event.status,
        "phase": event.phase,
        "job_id": event.job_id,
        "current_file": event.current_file,
        "done": event.done,
        "total": event.total,
        "bytes_done": event.bytes_done,
        "bytes_total": event.bytes_total,
        "elapsed_ms": event.elapsed_ms,
        "speed": event.speed,
        "message": event.message,
        "ts": event.ts,
    })
}
