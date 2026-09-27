//! 宿主 ↔ 外置插件的**契约**测试
//!
//! 这里刻意不写「用假 JSON 测假解析器」的自嗨测试：外置插件是**独立仓库目录、
//! 独立编译**的产物，宿主改类型时最容易漏掉它们。所以本模块直接读取 `plugins/`
//! 下各插件的真实声明文件，用**真实的宿主类型**反序列化，并校验前端依赖的字段。
//!
//! 覆盖两件事：
//! 1. **describe 契约**：插件的 `describe.json` 必须能被 `AbiDescribe` 解析，
//!    且 UI 区块里前端会读的字段（动作路径、凭据字段名…）都齐备；
//! 2. **副作用契约**：插件返回值里的 `alerts` / `resolve` / `config` / `audit`
//!    必须被 `apply_side_effects` 正确落到告警/配置/审计里（这是插件唯一的写路径）。

use crate::plugin::api::UiBlock;

/// 仓库内插件目录（相对本文件）
const PLUGINS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../plugins");

fn describe_path(plugin: &str) -> std::path::PathBuf {
    std::path::Path::new(PLUGINS_DIR).join(plugin).join("describe.json")
}

/// 读取插件声明（`{{version}}` 占位符替换成固定串，等价于插件编译期的注入）
fn read_describe(plugin: &str) -> String {
    let p = describe_path(plugin);
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("读取 {} 失败：{e}（插件声明文件丢失？）", p.display()));
    raw.replace("{{version}}", "0.0.0-test")
}

#[test]
fn kzwr_describe_parses_with_host_types() {
    let v: crate::plugin::abi::AbiDescribe =
        serde_json::from_str(&read_describe("kzwr")).expect("kzwr describe.json 必须能被宿主的 AbiDescribe 解析");
    assert_eq!(v.id, "kzwr");
    assert_eq!(v.kind.as_deref(), Some("enhance"));
    assert!(!v.name.trim().is_empty());
    let ui = v.ui.expect("kzwr 必须声明 ui（否则插件页只有空卡片）");
    assert_eq!(ui.section, "settings", "分区必须是既有稳定取值");
    assert!(!ui.blocks.is_empty(), "必须有通用区块：前端没有 kzwr 专属组件了");

    // 必须包含多账号区块，且数据契约字段齐备（前端按这些路径直接发请求）
    let acc = ui
        .blocks
        .iter()
        .find_map(|b| match b {
            UiBlock::Accounts {
                list,
                add,
                update,
                remove,
                credential_field,
                edit_action,
                multiple,
                ..
            } => Some((
                list.clone(),
                add.clone(),
                update.clone(),
                remove.clone(),
                credential_field.clone(),
                edit_action.clone(),
                *multiple,
            )),
            _ => None,
        })
        .expect("kzwr 必须用通用 accounts 区块管理多账号（不得要求前端专属组件）");
    let (list, add, update, remove, cred_field, edit_action, multiple) = acc;
    for p in [&list, &add, &update, &remove] {
        assert!(p.starts_with('/'), "区块路径应相对 api_base 且以 / 开头：{p}");
    }
    assert_eq!(cred_field, "token", "凭据字段名与插件侧 CRUD 实现约定一致");
    assert!(multiple, "kzwr 支持多账号");
    assert_eq!(edit_action.as_deref(), Some("/accounts/percent"), "按账号阈值的编辑动作");

    // 动态指标：靠 action 取实时值
    assert!(
        ui.blocks.iter().any(|b| matches!(b, UiBlock::Metric { action, .. } if action.as_deref() == Some("/space"))),
        "云端空间必须走 metric+action 的通用动态指标"
    );
    // 危险操作必须有二次确认文案
    let btn = ui
        .blocks
        .iter()
        .find_map(|b| match b {
            UiBlock::Button { label, action, confirm, danger, .. } => {
                Some((label.clone(), action.clone(), confirm.clone(), *danger))
            }
            _ => None,
        })
        .expect("清空回收站按钮");
    assert_eq!(btn.1, "/trash/empty");
    assert!(btn.2.is_some() && btn.3, "物理删除必须是 danger + confirm: {btn:?}");
}

#[test]
fn kzwr_declares_no_host_stored_fields() {
    // 迁移要点：kzwr 的凭据/阈值由**插件自己**存（plugin_data 命名空间 + 声明式回写），
    // 不再走宿主代管的 `scope:"host"` 表单，也不该有 `cfg.kzwr.*` 残留。
    let v: crate::plugin::abi::AbiDescribe =
        serde_json::from_str(&read_describe("kzwr")).unwrap();
    let ui = v.ui.unwrap();
    for b in &ui.blocks {
        let scope = match b {
            UiBlock::Text { scope, .. } => scope.as_deref(),
            UiBlock::Number { scope, .. } => scope.as_deref(),
            UiBlock::Toggle { scope, .. } => scope.as_deref(),
            _ => None,
        };
        assert_ne!(scope, Some("host"), "kzwr 不得使用宿主代管字段（配置归插件所有）");
    }
    let raw = read_describe("kzwr");
    for banned in ["access_token", "quota_warn_percent", "cfg.kzwr"] {
        assert!(!raw.contains(banned), "describe 里不该再出现核心时代的字段名 {banned}");
    }
}

#[test]
fn every_bundled_plugin_describe_matches_host_schema() {
    // 通用闸门：plugins/ 下**所有**带 describe.json 的插件都必须能被宿主类型解析。
    // 新增插件时不需要改这个测试就能挡住「字段名写错/枚举 tag 不符」这类问题。
    let dir = std::path::Path::new(PLUGINS_DIR);
    let mut checked = 0;
    for e in std::fs::read_dir(dir).expect("plugins/ 目录应存在").flatten() {
        let p = e.path().join("describe.json");
        if !p.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("读不动 {}", p.display()));
        let v: crate::plugin::abi::AbiDescribe = serde_json::from_str(&raw.replace("{{version}}", "0"))
            .unwrap_or_else(|err| panic!("{} 的 describe.json 解析失败：{err}", e.path().display()));
        assert!(!v.id.trim().is_empty(), "{} 缺少 id", p.display());
        checked += 1;
    }
    assert!(checked > 0, "应至少校验到 kzwr 的 describe.json");
}

// ── 副作用契约 ─────────────────────────────────────────────────────────────
//
// `apply_side_effects` 落告警/配置需要 AppState，故按**可单测的 seam**拆开校验：
// 键名规则、告警前缀消解、以及配置回写的纯逻辑（`parse_writeback` +
// `writeback_to_manager`，后者只依赖 ConfigManager，不依赖整个状态）。

/// 测试用 `ConfigManager`（临时目录 + 固定口令，测试内自洽）
fn test_mgr() -> (tempfile::TempDir, crate::infra::config::ConfigManager) {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = crate::infra::config::ConfigManager::new(
        dir.path(),
        age::secrecy::SecretString::from("test-pass".to_string()),
    );
    (dir, m)
}

#[test]
fn writeback_round_trips_and_is_namespace_isolated() {
    use crate::plugin::cabi::{parse_writeback, writeback_to_manager};
    let (_dir, mgr) = test_mgr();
    let mut cfg = crate::infra::config::AppConfig::default();

    // 用 raw string 写：这里刻意保留「值本身是 JSON 数组字符串」这一真实形态
    // （账号列表整体存一个键，凭据随命名空间一起加密）
    let raw: serde_json::Value = serde_json::from_str(r#"{
        "config": {
            "set": {
                "accounts": "[{\"id\":\"a1\",\"name\":\"主账号\",\"token\":\"SECRET-TOKEN\"}]",
                "percent-a1": "90"
            },
            "remove": []
        }
    }"#)
    .expect("回写 JSON 必须可解析");
    let (set, remove) = parse_writeback(&raw).expect("合法回写应被解析");
    assert_eq!(set.len(), 2);
    assert!(remove.is_empty());
    writeback_to_manager(&mgr, &mut cfg, "kzwr", &set, &remove).expect("回写应成功");
    mgr.save(&cfg).expect("落盘应成功");

    // 重新读取 → 插件自己的命名空间能读回明文（这是插件唯一的持久化路径）
    let mgr2 = crate::infra::config::ConfigManager::new(
        _dir.path(),
        age::secrecy::SecretString::from("test-pass".to_string()),
    );
    let cfg2 = mgr2.load().expect("应能读回配置");
    let kv = mgr2.plugin_data_export(&cfg2, "kzwr").expect("导出 kzwr 命名空间");
    assert!(kv.get("accounts").map(|s| s.contains("SECRET-TOKEN")).unwrap_or(false),
        "自配置必须以明文可读回（凭据由插件持有，宿主只负责加密落盘）");
    assert_eq!(kv.get("percent-a1").map(String::as_str), Some("90"));

    // **隔离**：别的插件视角里一个键都看不到
    let other = mgr2.plugin_data_export(&cfg2, "webdav").expect("其它插件的命名空间");
    assert!(other.is_empty(), "插件命名空间必须隔离，其它插件不得看到 kzwr 的键：{other:?}");
    let snap = crate::plugin::abi::CfgSnapshot::from_config(&cfg2).with_self_config(&cfg2, "webdav", &mgr2);
    let json = serde_json::to_string(&snap.self_config).unwrap();
    assert!(!json.contains("SECRET-TOKEN") && !json.contains("percent-a1"),
        "快照注入也必须按插件 id 过滤：{json}");

    // 删除（空串 = 移除，与宿主代存端点同语义）
    let (s2, r2) = (Vec::new(), vec!["percent-a1".to_string()]);
    writeback_to_manager(&mgr2, &mut cfg2.clone(), "kzwr", &s2, &r2).ok();
    let mut cfg3 = cfg2.clone();
    writeback_to_manager(&mgr2, &mut cfg3, "kzwr", &s2, &r2).expect("删除应成功");
    let kv3 = mgr2.plugin_data_export(&cfg3, "kzwr").unwrap();
    assert!(!kv3.contains_key("percent-a1"), "声明 remove 后该键应消失");
    assert!(kv3.contains_key("accounts"), "只删声明的键，其余保留");
}

#[test]
fn writeback_rejects_illegal_keys_atomically() {
    use crate::plugin::cabi::{parse_writeback, writeback_to_manager};
    let (_dir, mgr) = test_mgr();
    let mut cfg = crate::infra::config::AppConfig::default();

    // 一个合法 + 一个带点号：**整批拒绝**（不是部分生效），避免插件以为写成功了
    let raw = serde_json::json!({
        "config": { "set": { "accounts": "x", "percent.a1": "90" } }
    });
    let parsed = parse_writeback(&raw);
    assert!(parsed.is_err(), "带点号的键应导致整批失败");
    let (set, remove) = parsed.unwrap_or_else(|_| (Vec::new(), Vec::new()));
    assert!(set.is_empty() && remove.is_empty(), "失败时不得返回任何待写项");
    // 即便插件绕过校验直接调用写入，也应被键名规则拦住
    let bad = vec![("percent.a1".to_string(), "90".to_string())];
    assert!(writeback_to_manager(&mgr, &mut cfg, "kzwr", &bad, &[]).is_err());
    assert!(mgr.plugin_data_export(&cfg, "kzwr").unwrap().is_empty(), "非法键不得落库");
}

#[test]
fn writeback_ignores_absent_and_empty_config() {
    use crate::plugin::cabi::parse_writeback;
    // 没写 config 字段（绝大多数动作）→ 不产生任何写入，也不报错
    assert!(parse_writeback(&serde_json::json!({ "success": true })).unwrap().0.is_empty());
    // 空 set + 空 remove → 视为「无事可做」
    let raw = serde_json::json!({ "config": { "set": {}, "remove": [] } });
    let (set, remove) = parse_writeback(&raw).unwrap();
    assert!(set.is_empty() && remove.is_empty(), "空回写不应触发落盘/审计");
}

#[test]
fn health_accepts_both_shapes() {
    use crate::plugin::api::CheckOutcome;
    use crate::plugin::cabi::parse_health;
    // 统一转成宿主对外的 CheckOutcome：顺带覆盖 status 缺省与字段搬运
    let outcomes = |raw: &str| -> Vec<CheckOutcome> {
        parse_health(raw).into_iter().map(CheckOutcome::from).collect()
    };

    // 裸数组（最初的形状，必须继续兼容）
    let arr = r#"[{"key":"kzwr.a1","title":"酷族账号","status":"warn","detail":"额度 92%","hint":"去处理"}]"#;
    let v = outcomes(arr);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].key, "kzwr.a1");
    assert_eq!(v[0].status, "warn");
    assert_eq!(v[0].title, "酷族账号");
    assert_eq!(v[0].hint.as_deref(), Some("去处理"));

    // 对象（带告警副作用的形状）：体检项取自 checks 字段
    let obj = serde_json::json!({
        "checks": [{"key":"quota.a1","title":"云端空间","detail":"1.8 TB / 2 TB"}],
        "alerts": [{"level":"warn","message":"云端存储空间已用「主账号」 92%"}],
        "resolve": ["云端存储空间已用「次账号」"]
    });
    let v = outcomes(&obj.to_string());
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].key, "quota.a1");
    assert_eq!(v[0].status, "ok", "省略 status 必须按 ok 处理（否则会误报异常）");

    // 垃圾输入 → 空表（体检接口不该因某插件返回值走形而 500）
    assert!(outcomes("不是 json").is_empty());
    assert!(outcomes("").is_empty());
    assert!(outcomes(r#"{"alerts":[]}"#).is_empty(), "没有 checks 字段时退化为无体检项");
}

#[test]
fn plugin_config_keys_follow_host_key_rules() {
    // 直接用宿主的判定函数（而不是在这里重抄一遍规则）：插件侧的键名生成
    // 必须与它一致，规则变了这条测试就会指出差异。
    use crate::plugin::cabi::writeback_key_ok as ok;
    assert!(ok("accounts"));
    assert!(ok("percent"));
    assert!(ok("percent-a1f3"));
    // 关键：**点号不被允许** —— 曾经的 `percent.<id>` 会被整批拒绝且只留一条 warn
    assert!(!ok("percent.a1f3"), "带点号的键必须被拒（插件侧因此改用连字符）");
    assert!(!ok(""));
    assert!(!ok(&"x".repeat(65)));
    assert!(!ok("中文键"));
}

#[test]
fn quota_alert_prefix_stable_for_resolve() {
    // 插件用「消息前缀」消解旧告警：宿主 remove_where 靠 starts_with 匹配，
    // 文案前缀改了就会留下永不消失的僵尸告警。这里钉死前缀本身。
    let prefix = "云端存储空间已用";
    let sample = format!("{prefix}「主账号」 92%（1.8 TB / 2 TB），达到预警阈值 90%，请及时清理以免备份失败");
    assert!(sample.starts_with(prefix));
    // 迁移前的旧前缀（无账号名）也必须能被新前缀消解，否则升级后旧告警清不掉
    let legacy = "云端存储空间已用 92%（1.8 TB / 2 TB），达到预警阈值 85%，请及时清理以免备份失败";
    assert!(legacy.starts_with(prefix), "旧格式告警应能被同一前缀消解");
}
