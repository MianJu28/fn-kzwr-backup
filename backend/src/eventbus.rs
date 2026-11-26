//! 内部事件总线（ADR-007：进程内事件总线）
//!
//! 用 tokio::broadcast 实现领域事件异步分发。
//! 订阅者：UI 状态广播（WebSocket）、审计日志写入、fnos 通知。
//! 主流程不依赖事件确认——事件丢失不影响数据正确性，仅影响通知。

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

/// 任务类型
///
/// `Plugin` 是**唯一**不参与宿主任务卡的事件类型：插件经能力表 `progress` 上报的进度
/// 用它发布，前端据此**忽略**它对顶部「任务卡」的覆盖（否则插件进度会顶掉备份状态）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Backup,
    Restore,
    /// 插件自报进度（非宿主任务）
    Plugin,
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

/// 任务阶段（UI 用「准备 → 传输 → 收尾」展示任务全流程，而不只是上传下载）
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskPhase {
    /// 准备：扫描源目录、比对快照差分、列取云端文件
    Prepare,
    /// 传输：上传 / 下载文件
    Transfer,
    /// 收尾：删除云端多余文件、保留策略清理孤儿、落盘快照
    Cleanup,
}

/// 领域事件（备份/恢复任务状态推送）
#[derive(Debug, Clone, Serialize)]
pub struct DomainEvent {
    /// 任务类型（backup / restore）
    pub kind: TaskKind,
    /// 任务状态（started / progress / completed / failed）
    pub status: TaskStatus,
    /// 当前阶段（prepare / transfer / cleanup；为空表示未上报阶段）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<TaskPhase>,
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
    /// **最后一次任务状态快照**（备份 / 恢复各一份）
    ///
    /// ## 为什么需要它
    /// 广播只推**增量事件**：客户端连上 WS 之后才知道后续发生了什么。
    /// 而「任务卡在 87%」这类状态必须能**随时读取**——否则用户刷新页面、
    /// 退出应用再进来（WebView 重建、WS 重连）时，之前收到的事件全丢，
    /// 界面就变成「没有任务在跑」，而后台其实还在跑。
    ///
    /// 只存最后一帧（而非历史）就够了：进度面板是「当前状态」的投影，
    /// 不需要回放整个序列。存储开销固定为一个结构体。
    last: std::sync::RwLock<[Option<Arc<DomainEvent>>; 2]>,
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
        Self {
            tx,
            last: std::sync::RwLock::new([None, None]),
        }
    }

    /// 任务类型在快照数组里的下标（`None` = 该类事件不入快照）
    ///
    /// 用 `Option` 而不是「返回 2 当哨兵」：哨兵一旦被误用就会**覆盖备份快照**，
    /// 而新增 `TaskKind` 变体时编译器会强制这里补分支 —— 哨兵则不会。
    fn slot(kind: &TaskKind) -> Option<usize> {
        match kind {
            TaskKind::Backup => Some(0),
            TaskKind::Restore => Some(1),
            // 插件自报进度不占任务卡（前端同样忽略）
            TaskKind::Plugin => None,
        }
    }

    /// 发布事件（异步分发，不阻塞主流程）
    ///
    /// 同时更新「最后一帧」快照，供新连上的客户端立即取回当前状态。
    pub fn publish(&self, event: DomainEvent) {
        if let Some(idx) = Self::slot(&event.kind) {
            // 终态也要存：这样重连后仍能看到「上次备份成功/失败」的结论，
            // 而不是一片空白（是否算「进行中」由前端按 status 判断）。
            if let Ok(mut guard) = self.last.write() {
                guard[idx] = Some(Arc::new(event.clone()));
            }
        }
        let _ = self.tx.send(Arc::new(event));
    }

    /// 取回最后一次任务状态快照（`None` = 本次启动以来还没跑过该类任务）
    ///
    /// 供 WS 连接建立时**先补一帧**，以及 `GET /api/tasks/live` 使用。
    pub fn last_snapshot(&self, kind: TaskKind) -> Option<Arc<DomainEvent>> {
        let idx = Self::slot(&kind)?;
        self.last.read().ok().and_then(|g| g[idx].clone())
    }

    /// 若「最后一帧」属于某个已被删除的任务，就把它丢掉
    ///
    /// ## 为什么需要
    /// 「概览 → 实时任务」渲染的是 `last_snapshot` 的最后一帧，**与任务是否
    /// 还存在无关**。于是删掉任务之后，卡片会继续挂着它的上次结果（实测：
    /// 删除探针任务后，卡片仍显示「备份失败 · 路径不存在: /vol1/1000/no-such-…」），
    /// 用户会以为故障还在，也会误判「清空消息提醒没生效」。
    ///
    /// 备份任务的多路径作业 `job_id` 形如 `{task_id}-{序号}`，单路径则可能
    /// 直接等于 `task_id`，所以两种都要匹配；`{task_id}-` 的尾随连字符用来
    /// 避免把一个 `abc` 任务误判成 `abc-1` 那个任务。
    pub fn clear_if_task(&self, kind: TaskKind, task_id: &str) -> bool {
        let Some(idx) = Self::slot(&kind) else {
            return false;
        };
        let prefix = format!("{task_id}-");
        let belongs = self
            .last
            .read()
            .ok()
            .and_then(|g| g[idx].clone())
            .map(|e| e.job_id == task_id || e.job_id.starts_with(&prefix))
            .unwrap_or(false);
        if !belongs {
            return false;
        }
        if let Ok(mut guard) = self.last.write() {
            guard[idx] = None;
        }
        true
    }

    /// **兜底收尾**：若该类型的最后一帧仍处于「进行中」，补发一条终态事件
    ///
    /// ## 为什么必须有
    /// `last_snapshot` 取的是广播缓存里的最后一帧。任务若因以下原因结束，
    /// 都**没有机会**走到正常的「发布 Completed/Failed」代码路径：
    /// - 协作式取消：在文件边界 `return Err`，但外层若已返回则不再发布；
    /// - 重试耗尽（如云端反复超时）：插件层判失败后任务静默返回；
    /// - panic 被 `catch_unwind` 吞掉。
    ///
    /// 结果就是前端「实时任务」卡片永久停在「进行中」——即使后端 `busy` 已是 false
    /// （实测：恢复卡在 9/12，面板一直转圈，既没完成也没报错）。
    ///
    /// 返回 `true` 表示补发了终态。
    pub fn finalize_if_unfinished(&self, kind: TaskKind, reason: &str) -> bool {
        let Some(last) = self.last_snapshot(kind.clone()) else {
            return false;
        };
        if !matches!(last.status, TaskStatus::Started | TaskStatus::Progress) {
            return false;
        }
        self.task_event(
            kind,
            TaskStatus::Failed,
            last.phase,
            last.job_id.clone(),
            last.current_file.clone(),
            last.done,
            last.total,
            last.bytes_done,
            last.bytes_total,
            last.elapsed_ms,
            last.speed,
            Some(reason.to_string()),
        );
        true
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
        phase: Option<TaskPhase>,
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
            phase,
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

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    fn ev(kind: TaskKind, done: u64) -> DomainEvent {
        DomainEvent {
            kind,
            status: TaskStatus::Progress,
            phase: Some(TaskPhase::Transfer),
            job_id: "j".to_string(),
            current_file: Some("a.age".to_string()),
            done,
            total: 10,
            bytes_done: done * 100,
            bytes_total: 1000,
            elapsed_ms: 1234,
            speed: 555,
            message: None,
            ts: 1,
        }
    }

    /// **回归**：新连上的客户端必须能取回「最后一帧」
    ///
    /// 线上症状：退出应用再进来，实时任务就没了（后台其实还在跑）。
    /// 根因是广播只推增量事件，重连后的客户端拿不到任何历史状态。
    #[test]
    fn last_snapshot_survives_for_late_subscribers() {
        let bus = EventBus::new();
        // 订阅之前就发布（模拟「任务在页面关闭期间推进」）
        bus.publish(ev(TaskKind::Backup, 3));
        bus.publish(ev(TaskKind::Backup, 7));

        let snap = bus
            .last_snapshot(TaskKind::Backup)
            .expect("必须有最后一帧");
        assert_eq!(snap.done, 7, "快照应是最后一帧，而不是第一帧");
        // 恢复类未跑过 → 无快照（前端据此不显示恢复卡片）
        assert!(bus.last_snapshot(TaskKind::Restore).is_none());
    }

    /// **回归：任务异常结束时必须补发终态，否则前端永久「进行中」**
    ///
    /// 实测症状：恢复卡在 9/12，后端 `busy` 已是 false，面板却一直转圈，
    /// 既不显示完成也不显示失败。根因是任务走完了但**没有发终态事件**
    /// （取消在文件边界 return、重试耗尽后静默返回等），
    /// `last_snapshot` 于是永远返回那一帧 `status=progress`。
    #[test]
    fn finalize_if_unfinished_publishes_terminal_state() {
        let bus = EventBus::new();
        bus.publish(ev(TaskKind::Restore, 9)); // 停在 progress

        let changed = bus.finalize_if_unfinished(TaskKind::Restore, "异常结束");
        assert!(changed, "处于进行中的任务应被补发终态");

        let snap = bus.last_snapshot(TaskKind::Restore).expect("应有快照");
        assert_eq!(
            snap.status,
            TaskStatus::Failed,
            "补发后必须离开 progress 态，前端才会停止转圈"
        );
        assert_eq!(snap.done, 9, "补发不能丢掉已完成的进度");
        assert_eq!(snap.message.as_deref(), Some("异常结束"));
    }

    /// 已经是终态的任务不能被重复补发（否则会覆盖真实的 completed）
    #[test]
    fn finalize_does_not_override_terminal_state() {
        let bus = EventBus::new();
        bus.task_event(
            TaskKind::Backup,
            TaskStatus::Completed,
            None,
            "j".into(),
            None,
            10,
            10,
            1000,
            1000,
            100,
            10,
            None,
        );

        let changed = bus.finalize_if_unfinished(TaskKind::Backup, "不该覆盖");
        assert!(!changed, "已完成的任务不应被改成 Failed");

        let snap = bus.last_snapshot(TaskKind::Backup).expect("应有快照");
        assert_eq!(snap.status, TaskStatus::Completed, "真实终态必须保留");
    }

    /// 备份与恢复的快照互不覆盖
    #[test]
    fn backup_and_restore_snapshots_are_independent() {
        let bus = EventBus::new();
        bus.publish(ev(TaskKind::Backup, 2));
        bus.publish(ev(TaskKind::Restore, 9));
        assert_eq!(bus.last_snapshot(TaskKind::Backup).unwrap().done, 2);
        assert_eq!(bus.last_snapshot(TaskKind::Restore).unwrap().done, 9);
    }

    /// 插件自报进度不占任务卡快照
    #[test]
    fn plugin_events_do_not_overwrite_task_snapshots() {
        let bus = EventBus::new();
        bus.publish(ev(TaskKind::Backup, 5));
        bus.publish(ev(TaskKind::Plugin, 99));
        assert_eq!(
            bus.last_snapshot(TaskKind::Backup).unwrap().done,
            5,
            "插件事件不该顶掉备份的最后一帧"
        );
        assert!(bus.last_snapshot(TaskKind::Plugin).is_none());
    }

    /// 删除任务后，「实时任务」卡片不能再挂着它的上次结果
    #[test]
    fn clear_if_task_drops_snapshot_of_deleted_task() {
        let bus = EventBus::new();
        let mut e = ev(TaskKind::Backup, 10);
        e.job_id = "kabc-0".to_string();
        bus.publish(e);
        assert!(bus.last_snapshot(TaskKind::Backup).is_some());

        assert!(!bus.clear_if_task(TaskKind::Backup, "kother"), "别的任务不应清掉");
        assert!(bus.last_snapshot(TaskKind::Backup).is_some());

        assert!(bus.clear_if_task(TaskKind::Backup, "kabc"));
        assert!(
            bus.last_snapshot(TaskKind::Backup).is_none(),
            "已删除任务的结果不该继续占据卡片"
        );
    }

    /// `abc` 不能把 `abc-1` 那个任务的帧当成自己的（反之亦然）
    #[test]
    fn clear_if_task_matches_only_exact_or_indexed_job_id() {
        let bus = EventBus::new();
        let mut e = ev(TaskKind::Backup, 3);
        e.job_id = "abc-1".to_string();
        bus.publish(e);
        assert!(!bus.clear_if_task(TaskKind::Backup, "ab"));
        assert!(!bus.clear_if_task(TaskKind::Backup, "abc-1x"));
        assert!(bus.clear_if_task(TaskKind::Backup, "abc"));
    }
}
