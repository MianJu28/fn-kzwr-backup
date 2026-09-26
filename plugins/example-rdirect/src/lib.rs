//! 酷族备份 · 外置插件示例 —— **Rust 直连（进阶）**
//!
//! 直接实现宿主的 `EnhancePlugin` / `TargetPlugin` trait，能力最全
//! （可以写自定义备份目标、用 axum 路由、读 `AppState`），代价是：
//!
//! ⚠️ **必须与宿主同版本编译**：跨边界传的是 Rust trait 对象，Rust 没有稳定 ABI，
//!    宿主会校验「插件编译时链接的宿主版本 == 当前运行版本」，不一致直接拒绝加载。
//!    想让插件在宿主升级后免重编 → 用 `plugins/example-hello`（稳定 C ABI）。
//!
//! 本示例演示增强插件（一张设置卡片 + 自己的接口 + 体检项）。

use async_trait::async_trait;
use axum::routing::post;
use axum::Router;
use serde_json::json;

use fnos_backup::infra::config::AppConfig;
use fnos_backup::plugin::api::{
    CheckOutcome, EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta, PluginUi, UiBlock,
};
use fnos_backup::plugin::sdk::PluginHandle;
use fnos_backup::AppState;

struct RustDirectPlugin;

#[async_trait]
impl EnhancePlugin for RustDirectPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "example-rust".to_string(),
            name: "示例插件（Rust 直连）".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: PluginKind::Enhance,
            builtin: false,
            description: "Rust 直连示例：能力最全，但需与宿主同版本编译".to_string(),
        }
    }

    fn caps(&self) -> EnhanceCaps {
        EnhanceCaps::default()
    }

    fn available(&self, _cfg: &AppConfig) -> bool {
        true
    }

    fn ui(&self) -> Option<PluginUi> {
        Some(PluginUi {
            section: "settings".to_string(),
            title: "示例插件（Rust 直连）".to_string(),
            order: 91,
            component: None,
            blocks: vec![
                UiBlock::Tips {
                    text: "这张卡片来自 Rust 直连插件：直接实现宿主 trait，能力最全，\
                           但升级主程序后必须重新编译本插件（Rust 无稳定 ABI）。"
                        .to_string(),
                },
                UiBlock::Metric {
                    label: "加载机制".to_string(),
                    value: "Rust 直连（trait 对象）".to_string(),
                    hint: Some("宿主校验编译期宿主版本，不一致拒绝加载".to_string()),
                },
                UiBlock::Button {
                    label: "调用插件接口".to_string(),
                    action: "/hello".to_string(),
                    danger: false,
                    confirm: None,
                },
            ],
        })
    }

    fn routes(&self) -> Router<AppState> {
        Router::new().route(
            "/hello",
            post(|| async {
                axum::Json(json!({
                    "success": true,
                    "message": "你好，来自 Rust 直连插件的问候（同版本编译校验通过）",
                }))
            }),
        )
    }

    async fn health_check(&self, _state: &AppState, _cfg: &AppConfig) -> Vec<CheckOutcome> {
        vec![CheckOutcome {
            key: "example_rust".to_string(),
            title: "示例插件（Rust 直连）".to_string(),
            status: "ok".to_string(),
            detail: "Rust 直连插件已加载，接口 /api/p/example-rust/hello 可用".to_string(),
            hint: None,
        }]
    }
}

fn create() -> PluginHandle {
    PluginHandle::enhance_only(RustDirectPlugin)
}

fnos_backup::export_plugin!(create);
