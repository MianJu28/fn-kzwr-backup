//! 酷族备份 · 外置插件示例（动态库，ADR-013 **方案 B**）
//!
//! 这个 crate 演示「不重新打包主程序与前端」即可新增一个功能区块：
//! 1. 编译为 `cdylib`（`cargo build --release` → `libfn_kzwr_plugin_example.so`）
//! 2. 放进宿主的插件目录（`$TRIM_PKGETC/plugins/`），并在设置页开启「外置插件加载」
//! 3. 重启应用后：设置页出现下面的卡片（前端用**通用 UI Schema 渲染**），
//!    卡片里的按钮调用插件自己的路由 `/api/p/example/hello`
//!
//! 说明：插件与宿主通过 Rust trait 对象交接，**ABI 不稳定**，因此宿主会校验
//! 「插件编译时的宿主版本」；升级主程序后请重新编译插件（`Scripts/build_plugins.sh`）。

use async_trait::async_trait;
use axum::routing::post;
use axum::Router;
use serde_json::json;

use fnos_backup::AppState;
use fnos_backup::infra::config::AppConfig;
use fnos_backup::plugin::api::{
    CheckOutcome, EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta, PluginUi, UiBlock,
};
use fnos_backup::plugin::sdk::PluginHandle;

/// 示例插件：只做三件事 —— 一张设置卡片、一个自检项、一个自己的接口
struct ExamplePlugin;

#[async_trait]
impl EnhancePlugin for ExamplePlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "example".to_string(),
            name: "示例外置插件".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: PluginKind::Enhance,
            // 外置插件：必须为 false，前端/诊断据此标注来源
            builtin: false,
            description: "通过动态库（.so）加载的示例插件，演示外置扩展能力".to_string(),
        }
    }

    fn caps(&self) -> EnhanceCaps {
        // 不声明任何内置能力：本插件只提供自己的 UI 与接口
        EnhanceCaps::default()
    }

    fn available(&self, _cfg: &AppConfig) -> bool {
        // 本示例没有依赖配置，始终可用
        true
    }

    /// 设置页卡片：前端不认识 `component`（这里是 None），会走通用 UI Schema 渲染
    fn ui(&self) -> Option<PluginUi> {
        Some(PluginUi {
            section: "settings".to_string(),
            title: "示例外置插件".to_string(),
            // 排在内置卡片（webdav 10 / kzwr 20）之后
            order: 90,
            component: None,
            blocks: vec![
                UiBlock::Tips {
                    text: "这张卡片来自外置插件（动态库 .so）：宿主启动时扫描插件目录并加载，\
                           无需重新打包主程序与前端即可新增功能区块。"
                        .to_string(),
                },
                UiBlock::Metric {
                    label: "加载方式".to_string(),
                    value: "动态库（ADR-013 方案 B）".to_string(),
                    hint: Some("宿主校验 ABI 版本与编译期宿主版本后才注册".to_string()),
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

    /// 插件自带的 HTTP 子路由（宿主统一挂在 `/api/p/<插件id>` 下 → `/api/p/example/hello`）
    fn routes(&self) -> Router<AppState> {
        Router::new().route(
            "/hello",
            post(|| async {
                axum::Json(json!({
                    "success": true,
                    "message": "你好，来自外置插件的问候（动态库加载成功）",
                }))
            }),
        )
    }

    /// 「一键体检」自检项
    async fn health_check(&self, _state: &AppState, _cfg: &AppConfig) -> Vec<CheckOutcome> {
        vec![CheckOutcome {
            key: "example_plugin".to_string(),
            title: "示例外置插件".to_string(),
            status: "ok".to_string(),
            detail: "动态库已加载，接口 /api/p/example/hello 可用".to_string(),
            hint: None,
        }]
    }
}

/// 创建插件实例（由 [`fnos_backup::export_plugin!`] 导出的 C ABI 符号调用）
fn create() -> PluginHandle {
    PluginHandle::enhance_only(ExamplePlugin)
}

// 导出三个 C ABI 符号：ABI 版本 / 宿主版本 / 创建实例
fnos_backup::export_plugin!(create);
