//! 内部事件总线（ADR-007：进程内事件总线）
//!
//! 用 tokio::broadcast 实现领域事件异步分发。
//! 订阅者：UI 状态广播（WebSocket）、审计日志写入、fnos 通知。
//! 主流程不依赖事件确认——事件丢失不影响数据正确性，仅影响通知。

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

/// 任务类型
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Backup,
    Restore,
}

/// 任务状态
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Started,
    Progress,
    Completed,
    Failed,
}

/// 领域事件（备份/恢复任务状态推送）
#[derive(Debug, Clone, Serialize)]
pub struct DomainEvent {
    /// 任务类型（backup / restore）
    pub kind: TaskKind,
    /// 任务状态（started / progress / completed / failed）
    pub status: TaskStatus,
    /// 任务 id（多路径时含索引）
    pub job_id: String,
    /// 当前处理文件（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
    /// 已完成文件数
    pub done: u64,
    /// 总文件数
    pub total: u64,
    /// 已处理字节数（明文）
    pub bytes_done: u64,
    /// 总字节数（明文；未知为 0）
    pub bytes_total: u64,
    /// 任务已耗时（毫秒）
    pub elapsed_ms: u64,
    /// 实时传输速度（字节/秒；仅统计实际传输时段，空闲不变化）
    pub speed: u64,
    /// 消息（错误信息等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 时间戳（毫秒）
    pub ts: i64,
}

/// 内部事件总线（进程内广播）
pub struct EventBus {
    tx: broadcast::Sender<Arc<DomainEvent>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// 创建事件总线（广播容量 128）
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(128);
        Self { tx }
    }

    /// 发布事件（异步分发，不阻塞主流程）
    pub fn publish(&self, event: DomainEvent) {
        let _ = self.tx.send(Arc::new(event));
    }

    /// 订阅事件流
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<DomainEvent>> {
        self.tx.subscribe()
    }

    /// 便捷发布：备份/恢复任务状态
    #[allow(clippy::too_many_arguments)]
    pub fn task_event(
        &self,
        kind: TaskKind,
        status: TaskStatus,
        job_id: String,
        current_file: Option<String>,
        done: u64,
        total: u64,
        bytes_done: u64,
        bytes_total: u64,
        elapsed_ms: u64,
        speed: u64,
        message: Option<String>,
    ) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.publish(DomainEvent {
            kind,
            status,
            job_id,
            current_file,
            done,
            total,
            bytes_done,
            bytes_total,
            elapsed_ms,
            speed,
            message,
            ts,
        });
    }
}
