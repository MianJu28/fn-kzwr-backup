//! 插件侧日志（走 **stderr**，不依赖宿主）
//!
//! 为什么不直接用 `tracing`：本 crate 编成独立 cdylib，`tracing` 的 dispatcher 是
//! **每个副本各自一份**的线程局部状态。宿主的订阅器装在宿主那份里，插件这份没有
//! 订阅器 → 事件被静默丢弃，等于日志黑洞（还白白拖进 `tracing` 依赖）。
//!
//! 因此这里用最朴素也最可靠的方式：写 stderr，带 `[kzwr]` 前缀。NAS 上宿主把子进程
//! stderr 收进自己的日志，`grep '\[kzwr\]'` 就能捞到插件的输出。
//!
//! `debug!` 默认关闭，设环境变量 `KZWR_PLUGIN_DEBUG=1` 打开（排障时临时用）。

/// 是否输出 debug 级（启动时读一次）
fn debug_on() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("KZWR_PLUGIN_DEBUG")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    })
}

fn line(level: &str, msg: &str) {
    eprintln!("[kzwr] {level} {msg}");
}

/// 信息级（默认输出）
#[allow(dead_code)]
pub fn info(msg: impl AsRef<str>) {
    line("INFO", msg.as_ref());
}

/// 警告级：不影响功能但用户该知道的问题
pub fn warn(msg: impl AsRef<str>) {
    line("WARN", msg.as_ref());
}

/// 调试级：仅在 `KZWR_PLUGIN_DEBUG=1` 时输出
pub fn debug(msg: impl AsRef<str>) {
    if debug_on() {
        line("DEBUG", msg.as_ref());
    }
}
