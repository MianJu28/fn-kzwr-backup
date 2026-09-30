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
    // 迁移要点：kzwr 的凭据/阈值由**插件自己**存（自己的 `own_data_dir` + 声明式回写），
    // 不再走宿主代管的 `scope:"host"` 表单，也不该有 `cfg.kzwr.*` 残留。
    //
    // 注意：`UiBlock` 的 `scope` 字段已随 ADR-021 一并删除，故这里只能对**原始文本**
    // 断言（而不是像以前那样反序列化后检查字段值）—— 效果相同且更严格：
    // 任何形式出现的 `scope`/`host` 都会被挡下。
    let v: crate::plugin::abi::AbiDescribe =
        serde_json::from_str(&read_describe("kzwr")).unwrap();
    assert!(v.ui.is_some(), "kzwr 必须声明 ui");
    let raw = read_describe("kzwr");
    for banned in ["access_token", "quota_warn_percent", "cfg.kzwr", "\"scope\"", "\"host\""] {
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
// `apply_side_effects` 落告警需要 AppState，故按**可单测的 seam**拆开校验：
// 键名规则、告警前缀消解、以及 health 的两种形状。
//
// 注：声明式 `config` 回写**已移除**（ADR-021：宿主不再代存插件配置），
// 故对应的 `parse_writeback` / `writeback_to_manager` 测试一并删除。

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
    use crate::plugin::cabi::config_key_ok as ok;
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

// ── ABI 布局哨兵 ─────────────────────────────────────────────────────────
//
// 宿主与 SDK **各自独立定义**同一张 `#[repr(C)]` 表（这样插件不依赖宿主 crate）。
// 两边靠字段**顺序与类型**对齐 —— 一旦有人只改一边（或误插字段而非追加），
// 就是**静默的内存错位**：调用会跳到错误地址。编译器发现不了，只有这里能发现。
//
// 做法：从两边源码里抽出 `pub struct ... { ... }` 的字段名序列，逐字段比对。

/// 读取仓库内某个相对路径的源码
fn read_repo_file(rel: &str) -> String {
    let p = std::path::Path::new(PLUGINS_DIR).join("..").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读取 {} 失败：{e}", p.display()))
}

/// 从源码里抽出一个 `struct <name> { ... }` 的**字段名**序列
///
/// 只取「行首是 `pub <ident>:`」的行，忽略注释/属性/泛型，够用且不引入解析依赖。
/// 行内注释与 `///` 文档行会被跳过；`Option<...>` 等类型不影响字段名提取。
fn struct_fields(src: &str, name: &str) -> Vec<String> {
    let marker = format!("pub struct {name} {{");
    let start = src
        .find(&marker)
        .unwrap_or_else(|| panic!("源码里找不到 `{marker}`（结构体被改名或移动了？）"));
    let body = &src[start + marker.len()..];
    let mut out = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with('}') {
            break; // 结构体结束
        }
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        // 形如 `pub log: Option<...>,`
        let Some(rest) = line.strip_prefix("pub ") else {
            continue;
        };
        let Some((field, _)) = rest.split_once(':') else {
            continue;
        };
        out.push(field.trim().to_string());
    }
    assert!(!out.is_empty(), "结构体 {name} 未解析出字段（格式变了？）");
    out
}


/// 读取 `abi-layout.txt` 快照里的某张表字段名
///
/// **为什么需要它**：插件源码迁到独立仓库 `fn-kzwr-backup-plugins` 后，本仓库不再有
/// `plugins/sdk/src/lib.rs` 可读。快照是两仓库的**共同契约**：
/// 本仓库比对「宿主 abi.rs ↔ 快照」，插件仓库 CI 比对「SDK ↔ 快照」，
/// 任一侧漂移都会在自己的 CI 变红 —— 跨 FFI 布局错位依然拦得住。
///
/// 快照不存在时返回 `None`（迁移期兼容）。
fn snapshot_fields(name: &str) -> Option<Vec<String>> {
    let p = std::path::Path::new(PLUGINS_DIR).join("..").join("abi-layout.txt");
    let raw = std::fs::read_to_string(&p).ok()?;
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            cur = line.trim_matches(['[', ']']).to_string();
            continue;
        }
        if cur == name {
            out.push(line.to_string());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// SDK 源码路径（迁移到独立仓库后不存在）
fn sdk_path() -> std::path::PathBuf {
    std::path::Path::new(PLUGINS_DIR).join("sdk/src/lib.rs")
}

#[test]
fn host_plugin_abi_fields_match_sdk_exactly() {
    let host = read_repo_file("backend/src/plugin/abi.rs");
    let h = struct_fields(&host, "KzwrPluginAbi");
    // 插件源码仍在同仓库时直接比对 SDK；已迁出则退回比对 abi-layout.txt 快照
    let s = if sdk_path().is_file() {
        struct_fields(&read_repo_file("plugins/sdk/src/lib.rs"), "KzwrPluginAbi")
    } else {
        snapshot_fields("KzwrPluginAbi").expect(
            "既无 plugins/sdk/src/lib.rs 也无 abi-layout.txt —— \
             无法校验 ABI 布局（跨 FFI 错位会静默崩）",
        )
    };
    assert_eq!(
        h, s,
        "主表 KzwrPluginAbi 字段与 SDK/快照不一致：\n宿主: {h:?}\n对方: {s:?}\n\
         规则：只能**尾部追加**；顺序/名称必须逐字段相同，否则是静默内存错位"
    );
    // 尾部字段的存在性也要断言，避免有人把 host_bind 放到中间
    assert_eq!(h.last().map(String::as_str), Some("host_bind"));
}

#[test]
fn host_capability_table_fields_match_sdk_exactly() {
    let host = read_repo_file("backend/src/plugin/abi.rs");
    let h = struct_fields(&host, "KzwrHostAbi");
    let s = if sdk_path().is_file() {
        struct_fields(&read_repo_file("plugins/sdk/src/lib.rs"), "KzwrHostAbi")
    } else {
        snapshot_fields("KzwrHostAbi").expect("既无 SDK 也无 ABI 快照，无法校验能力表布局")
    };
    assert_eq!(
        h, s,
        "能力表 KzwrHostAbi 字段与 SDK/快照不一致：\n宿主: {h:?}\n对方: {s:?}\n\
         这是宿主**下发给插件**的表：错位会让插件的日志/告警调用跳错地址"
    );
    assert_eq!(h.first().map(String::as_str), Some("abi"));
    // `free_str` 必须在**必需前缀**里（紧随 abi/size）—— 它是唯一非可选入口，
    // 这样老宿主的短表仍能被接受、只逐字段跳过缺失能力。
    assert_eq!(
        h.get(2).map(String::as_str),
        Some("free_str"),
        "free_str 必须是第三个字段（必需前缀），否则老宿主短表会被整表拒绝"
    );
}

#[test]
fn host_table_min_size_covers_all_but_optional_tail() {
    use crate::plugin::abi::{KzwrHostAbi, HOST_ABI_VERSION};
    // 必需前缀与完整长度都必须有意义：后者 ≥ 前者，且版本号非零
    assert!(KzwrHostAbi::MIN_SIZE >= 8, "至少要含 abi + size");
    assert!(
        KzwrHostAbi::TABLE_SIZE > KzwrHostAbi::MIN_SIZE,
        "能力表的可选尾部字段让完整长度大于必需前缀（若相等，说明 free_str 后没有可选字段了）"
    );
    assert_eq!(HOST_ABI_VERSION, 2);

    // 主表：必需前缀只到 free_str 为止，尾部可选字段（destroy/host_bind）不计入
    use crate::plugin::abi::KzwrPluginAbi;
    assert!(KzwrPluginAbi::MIN_SIZE < KzwrPluginAbi::TABLE_SIZE);
}

#[test]
fn older_plugin_table_without_host_bind_is_still_accepted() {    // 「老插件表短一截」必须仍然合法（否则每次给主表加字段都会踢掉所有旧插件）。
    // 这里直接构造一个「只到 free_str」的表头来验证 size 判定。
    use crate::plugin::abi::KzwrPluginAbi;
    let mut short = std::mem::MaybeUninit::<KzwrPluginAbi>::zeroed();
    // 只读 `size` 字段：写进去再验判定逻辑（`has_host_bind` 只看 size）
    unsafe {
        let p = short.as_mut_ptr();
        (*p).abi = crate::plugin::abi::C_ABI_VERSION;
        (*p).size = KzwrPluginAbi::MIN_SIZE;
        let t = &*p;
        assert!(
            !crate::plugin::cabi::has_host_bind(t),
            "size 只到必需前缀时，必须判定为「未实现 host_bind」"
        );
        assert!(
            (t.size as usize) >= KzwrPluginAbi::MIN_SIZE as usize,
            "必需前缀校验必须通过（老插件不能被拒绝）"
        );
        // 完整长度则必须判定为「实现了 host_bind」
        (*p).size = KzwrPluginAbi::TABLE_SIZE;
        assert!(crate::plugin::cabi::has_host_bind(&*p));
    }
}

/// **真实跨边界**冒烟测试：加载已构建（并签名）的 kzwr `.so`，走完整的
/// `host_bind` 握手，再经能力表回调 —— 这是唯一能证明「宿主与 SDK 的
/// `#[repr(C)]` 布局在**运行时**真的对齐」的测试（源码比对只能证明文本一致）。
///
/// `dist/` 未提交，因此没有构建产物时**跳过**（不算失败）：
/// 先跑 `Scripts/build_plugins.sh` 再执行本测试即可获得完整覆盖。
#[test]
fn real_plugin_completes_host_bind_handshake() {
    let so = std::path::Path::new(PLUGINS_DIR)
        .join("../dist/plugins/libfn_kzwr_plugin_kzwr.so");
    if !so.is_file() {
        eprintln!(
            "跳过：{} 不存在（先跑 Scripts/build_plugins.sh 可获得完整跨边界覆盖）",
            so.display()
        );
        return;
    }

    // 直接 dlsym 入口 + 校验主表（与 loader 同一条路径）
    let lib = unsafe { libloading::Library::new(&so) }.expect("dlopen 插件失败");
    let entry = unsafe {
        lib.get::<extern "C" fn() -> *const crate::plugin::abi::KzwrPluginAbi>(
            crate::plugin::abi::SYM_ENTRY_V1,
        )
    }
    .expect("缺少稳定入口 fn_kzwr_plugin_abi_v1");
    let table = unsafe { crate::plugin::cabi::validate_table(entry()) }
        .expect("插件主表未通过校验");
    assert!(
        crate::plugin::cabi::has_host_bind(table),
        "我们自己的插件必须声明 host_bind（SDK 的 export_plugin_v1! 会自动填）"
    );

    // 宿主能力表 + ctx：用真实的 HostEffects（无须 AppState，仅验布局与握手）
    let effects = crate::plugin::host_abi::HostEffects::new(
        std::env::temp_dir().join("kzwr-crossboundary"),
    );
    let ctx = effects.issue("kzwr-so");
    let host = effects.table();
    assert!(!host.is_null());

    // 握手：插件侧会校验 abi/size 并保存绑定，返回 0 表示接受
    let bind = unsafe { (*table).host_bind }.expect("host_bind 槽位");
    let rc = unsafe { bind(host, ctx) };
    assert_eq!(
        rc, 0,
        "插件拒绝了宿主能力表（返回 {rc}）—— 说明两侧 ABI 版本或必需前缀不一致"
    );

    // 经能力表调用 `own_data_dir`：验证「宿主分配串 + 插件用 free_str 释放」
    // 这条**双向**路径在真实进程里能跑通（布局错位会在这里崩或读到垃圾）。
    // （原先用 `config_get` 验证，该槽位已随 ABI v2 移除。）
    let own = unsafe { (*host).own_data_dir }.expect("own_data_dir 槽位");
    let dp = unsafe { own(ctx) };
    assert!(!dp.is_null(), "宿主应给出插件私有目录");
    let dir = unsafe { std::ffi::CStr::from_ptr(dp) }
        .to_string_lossy()
        .into_owned();
    assert!(dir.contains("kzwr-so"), "目录应带插件 id：{dir}");
    unsafe { ((*host).free_str)(dp) };
}

// ── 目标插件「表单能力」契约 ─────────────────────────────────────────────
//
// 有些目标根本不用凭据（如本地目录只认一个路径）。若宿主一律强制要求账号密码，
// 用户在「目标」页就**建不出**这类目标 —— 这正是本次修的问题。
// 因此 `needs_credentials` / `url_label` 等字段必须能从插件 describe 解析出来。

/// `needs_credentials` 缺省为 `true`（老插件不写该字段 ⇒ 行为不变）
#[test]
fn target_caps_default_to_needing_credentials() {
    use crate::plugin::abi::AbiTargetCaps;
    let caps: AbiTargetCaps =
        serde_json::from_str("{}").expect("空对象应可解析（全字段有缺省）");
    assert!(
        caps.needs_credentials,
        "缺省必须是 true —— 否则老插件会突然变成「不用凭据」，凭据校验被静默跳过"
    );
    assert!(caps.url_label.is_none(), "标签缺省为空，由前端按 WebDAV 渲染");
}

/// 显式声明 `needs_credentials: false` 时必须被采纳（本地目录这类目标靠它建出来）
#[test]
fn target_caps_honours_explicit_no_credentials() {
    use crate::plugin::abi::AbiTargetCaps;
    let caps: AbiTargetCaps = serde_json::from_str(
        r#"{"needs_credentials":false,"url_label":"目录路径","url_placeholder":"/vol1/backup"}"#,
    )
    .expect("应可解析");
    assert!(!caps.needs_credentials, "显式 false 必须生效");
    assert_eq!(caps.url_label.as_deref(), Some("目录路径"));
    assert_eq!(caps.url_placeholder.as_deref(), Some("/vol1/backup"));
}

/// example-localfs 的 describe 必须能被宿主解析，且声明「不用凭据」
///
/// 这是**回归测试**：该插件此前没声明 `needs_credentials`，导致它的目标在
/// 「目标」页建不出来（保存时被「新目标必须填写用户名与密码」拒绝）。
#[test]
fn example_localfs_declares_credential_free_target() {
    let src = read_repo_file("plugins/example-localfs/src/lib.rs");
    // 从源码里抽出 describe 的 JSON 字面量不现实，这里断言关键声明存在，
    // 真正的端到端解析由 `AbiTargetCaps` 的反序列化测试覆盖。
    assert!(
        src.contains("\"needs_credentials\": false"),
        "example-localfs 必须声明 needs_credentials=false，否则其目标无法在「目标」页创建"
    );
    assert!(
        src.contains("\"url_label\""),
        "应声明 url_label，让前端把「地址」渲染成「目录路径」（否则用户不知道该填路径）"
    );
}

/// `PluginEntry` 必须把「是否需要凭据」与 `kind` 暴露给前端
///
/// `PluginEntry` 只实现 `Serialize`（它是**出站**视图），故验证序列化输出：
/// 前端靠 `kind === 'target'` 筛选可选的目标类型，靠 `needs_credentials` 决定
/// 是否渲染账号/密码输入框 —— 两者缺一，插件目标就建不出来。
#[test]
fn plugin_entry_exposes_kind_and_credential_flag() {
    use crate::plugin::api::{PluginEntry, PluginKind, PluginMeta};
    let e = PluginEntry {
        meta: PluginMeta {
            id: "x".to_string(),
            name: "X".to_string(),
            version: "1".to_string(),
            kind: PluginKind::Target,
            builtin: false,
            description: String::new(),
        },
        available: true,
        api_base: "/api/p/x".to_string(),
        ui: None,
        source: "external".to_string(),
        path: None,
        supports_plan: false,
        parallel: None,
        disabled: false,
        needs_credentials: true,
        url_label: None,
        url_placeholder: None,
        url_hint: None,
        form: Vec::new(),
    };
    let v = serde_json::to_value(&e).expect("应可序列化");
    assert_eq!(
        v.get("kind").and_then(|x| x.as_str()),
        Some("target"),
        "前端据 kind 筛选「可作为备份目标」的插件"
    );
    assert_eq!(
        v.get("needs_credentials").and_then(|x| x.as_bool()),
        Some(true),
        "前端据 needs_credentials 决定是否显示账号/密码"
    );
}


// ── 目标表单声明（`target.form`）契约 ────────────────────────────────────
//
// 「新建/编辑目标」弹窗的字段完全由插件声明，宿主只渲染与存取。
// 这些测试钉住解析行为与**敏感字段判定**（涉及明文不外泄）。

/// 空 `target` 段必须可解析（老插件不声明 form ⇒ 前端回退内置 WebDAV 表单）
#[test]
fn target_form_is_optional() {
    use crate::plugin::abi::AbiTargetCaps;
    let caps: AbiTargetCaps = serde_json::from_str("{}").expect("空对象应可解析");
    assert!(caps.form.is_empty(), "未声明 form 时为空 ⇒ 前端用内置默认表单");
}

/// 声明 `form` 时必须解析出全部字段与属性
#[test]
fn target_form_parses_declared_fields() {
    use crate::plugin::abi::AbiTargetCaps;
    let caps: AbiTargetCaps = serde_json::from_str(
        r#"{
          "needs_credentials": false,
          "form": [
            {"key":"url","label":"目录路径","kind":"text","required":true,"placeholder":"/data"},
            {"key":"token","label":"令牌","kind":"password"},
            {"key":"mode","label":"模式","kind":"select",
             "options":[{"value":"a"},{"value":"b","label":"B 模式"}]}
          ]
        }"#,
    )
    .expect("应可解析");
    assert_eq!(caps.form.len(), 3);
    assert_eq!(caps.form[0].key, "url");
    assert!(caps.form[0].required);
    assert_eq!(caps.form[0].placeholder.as_deref(), Some("/data"));
    // 未写 kind ⇒ 缺省 text
    assert_eq!(caps.form[0].kind, "text");
    assert_eq!(caps.form[1].kind, "password");
    // select 选项：label 可省略（用 value 兜底）
    assert_eq!(caps.form[2].options.len(), 2);
    assert!(caps.form[2].options[0].label.is_none());
    assert_eq!(caps.form[2].options[1].label.as_deref(), Some("B 模式"));
}

/// **敏感字段判定**：显式 `secret` 优先；缺省时 `password` 类型视为敏感
///
/// 这直接决定该字段是否**加密存储**与**是否回传明文**，判错就是泄漏。
#[test]
fn target_field_secret_detection_is_conservative() {
    use crate::plugin::abi::AbiTargetField;
    let parse = |j: &str| -> AbiTargetField { serde_json::from_str(j).expect("应可解析") };

    // password 类型 ⇒ 缺省敏感
    assert!(parse(r#"{"key":"a","label":"A","kind":"password"}"#).is_secret());
    // 显式 secret:true 即使不是 password 也敏感
    assert!(parse(r#"{"key":"a","label":"A","kind":"text","secret":true}"#).is_secret());
    // 显式 secret:false 可关闭（如「令牌名称」这类非敏感文本）
    assert!(!parse(r#"{"key":"a","label":"A","kind":"password","secret":false}"#).is_secret());
    // 普通 text ⇒ 不敏感
    assert!(!parse(r#"{"key":"a","label":"A","kind":"text"}"#).is_secret());
    assert!(!parse(r#"{"key":"a","label":"A"}"#).is_secret());
}

/// `example-localfs` 必须声明 form，且 `url` 是必填项
///
/// 回归：该插件此前没有任何 form 声明，弹窗只能回退到 WebDAV 默认表单，
/// 于是「目录路径」以外的插件自有字段（如 subdir）在界面上根本无从填写。
#[test]
fn example_localfs_declares_its_own_form() {
    let src = read_repo_file("plugins/example-localfs/src/lib.rs");
    assert!(src.contains("\"form\": ["), "example-localfs 应声明 target.form");
    assert!(
        src.contains("\"key\": \"subdir\""),
        "应声明插件自有字段（验证「非 well-known 键按目标存储」这条路径）"
    );
    assert!(
        src.contains("\"key\": \"url\""),
        "应声明 url 字段（well-known 键，映射到 TargetConfig.url）"
    );
}

/// `abi-layout.txt` 必须与宿主 `abi.rs` 同步
///
/// 这条测试的意义：快照是**两仓库的共同契约**。如果只靠上面两条测试，
/// 当 SDK 恰好还在本仓库时它们走 SDK 分支，**快照漂移不会被发现**；
/// 等将来插件迁出（SDK 消失）才暴露，就太晚了。所以这里无条件校验快照本身。
#[test]
fn abi_layout_snapshot_is_in_sync_with_host() {
    for name in ["KzwrPluginAbi", "KzwrHostAbi", "KzwrTargetAbi"] {
        let host = struct_fields(&read_repo_file("backend/src/plugin/abi.rs"), name);
        let snap = snapshot_fields(name)
            .unwrap_or_else(|| panic!("abi-layout.txt 缺少 [{name}] 段"));
        assert_eq!(
            host, snap,
            "[{name}] 宿主字段与 abi-layout.txt 快照不一致：\n宿主: {host:?}\n快照: {snap:?}\n\
             改动 ABI 表时必须同步更新 abi-layout.txt（两仓库同时提交），\
             否则插件仓库 CI 会与主仓库判定不一致"
        );
    }
}
