//! 插件自管配置（多账号 + 按账号阈值）
//!
//! ## 为什么插件能写自己的配置，却不回调宿主
//!
//! ABI v1 主表是**冻结**的：没有宿主 vtable，插件不能反过来调宿主的方法。
//! 迁移方案因此采用**声明式回写**：插件在返回值里带
//!
//! ```json
//! {"config": {"set": {"k":"v"}, "remove": ["k"]}}
//! ```
//!
//! 宿主解析后写入本插件的 `plugin_data` 命名空间（age 加密落盘，审计只记键名），
//! **下一次调用**起 `cfg.self_config` 就能读回明文。本模块只负责生成这个回写声明。
//!
//! ## 存储布局（全部在 `cfg.plugin_data["kzwr"]` 命名空间内）
//!
//! | 键 | 含义 |
//! |----|------|
//! | `accounts` | JSON 数组 `[{"id","name","token"}]`（整串存一个键：token 随命名空间一起加密） |
//! | `percent-<id>` | 该账号的空间用量预警阈值（%），缺省 90 |
//! | `percent` | **遗留**全局阈值（迁移前只有单 token 时代）；作为未单独设置账号的兜底 |
//! | `token` | **遗留**单 token；首次读到即迁移成一个名为「默认账号」的账号后删除 |
//!
//! 阈值默认 90%：这是本次迁移确立的行为（原内置固定 85%，多账号后按账号可各自设置）。

use serde_json::Value;

/// 一个酷族账号
#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub token: String,
}

/// 空间预警阈值默认值（%）
pub const DEFAULT_PERCENT: u64 = 90;

/// 遗留账号（单 token 迁移而来）的固定 id
pub const LEGACY_ACCOUNT_ID: &str = "legacy";

/// 从 `cfg.self_config` 读出的插件视角状态
#[derive(Debug, Default)]
pub struct Snapshot {
    /// 已配置的账号（含明文 token，**绝不写日志**）
    pub accounts: Vec<Account>,
    /// 按账号阈值 `percent-<id>` → 百分比（宿主键名规则不允许点号）
    ///
    /// **`Some(0)` 与「没有这个键」语义不同**：0 = 用户显式关闭该账号的预警；
    /// 缺键 = 未单独设置，回落到 `default_percent`。因此写入侧绝不能把 0 存成删键。
    pub percents: std::collections::BTreeMap<String, u64>,
    /// 全局兜底阈值（遗留 `percent` 键；`None` = 未设置 → 用 [`DEFAULT_PERCENT`]，
    /// `Some(0)` = 显式关闭所有未单独设置账号的预警）
    pub default_percent: Option<u64>,
    /// 遗留单 token（尚未迁移）
    pub legacy_token: Option<String>,
}

impl Snapshot {
    /// 从快照 JSON 的 `self_config` 构造
    ///
    /// 解析容错：`accounts` 里的非法条目（缺 id / 缺 token）逐条跳过而不是整体失败
    /// —— 手工编辑过配置文件的场景下，一个坏条目不该让全部账号消失。
    pub fn from_cfg(cfg: &Value) -> Self {
        let sc = cfg.get("self_config");
        let get = |k: &str| -> Option<String> {
            sc?.get(k).and_then(|v| v.as_str()).map(str::to_string)
        };
        let mut out = Snapshot::default();
        if let Some(s) = get("accounts") {
            if let Ok(arr) = serde_json::from_str::<Value>(&s) {
                if let Some(list) = arr.as_array() {
                    for it in list {
                        let Some(id) = it.get("id").and_then(|x| x.as_str()).map(str::to_string)
                        else {
                            continue;
                        };
                        let Some(token) =
                            it.get("token").and_then(|x| x.as_str()).map(str::to_string)
                        else {
                            continue;
                        };
                        if token.trim().is_empty() {
                            continue;
                        }
                        let name = it
                            .get("name")
                            .and_then(|x| x.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("账号 {}", short_id(&id)));
                        out.accounts.push(Account { id, name, token });
                    }
                }
            }
        }
        if let Some(p) = get("percent") {
            if let Ok(n) = p.trim().parse::<u64>() {
                out.default_percent = Some(n);
            }
        }
        // 按账号阈值：`percent-<id>`（宿主键名规则不允许点号）
        if let Some(sc) = sc.and_then(|x| x.as_object()) {
            for (k, v) in sc {
                if let Some(id) = k.strip_prefix("percent-") {
                    if id.is_empty() {
                        continue;
                    }
                    if let Some(s) = v.as_str() {
                        if let Ok(n) = s.trim().parse::<u64>() {
                            out.percents.insert(id.to_string(), clamp_percent(n));
                        }
                    }
                }
            }
        }
        out.legacy_token = get("token").filter(|s| !s.trim().is_empty());
        // 遗留单 token：迁移成第一个账号（回写在需要落盘的动作里带上）
        if out.accounts.is_empty() {
            if let Some(t) = out.legacy_token.clone() {
                out.accounts.push(Account {
                    id: LEGACY_ACCOUNT_ID.to_string(),
                    name: "默认账号".to_string(),
                    token: t,
                });
            }
        }
        out
    }

    /// 已配置账号数
    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    /// 按 id 取账号
    pub fn find(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// 某账号的阈值：`percent-<id>` → 全局兜底 → [`DEFAULT_PERCENT`]
    pub fn percent_for(&self, id: &str) -> u64 {
        if let Some(n) = self.percents.get(id) {
            return clamp_percent(*n);
        }
        clamp_percent(self.default_percent.unwrap_or(DEFAULT_PERCENT))
    }

    /// 生效的兜底阈值：全局 `percent` → [`DEFAULT_PERCENT`]
    ///
    /// 供 `/accounts` 回显「未单独设置阈值的账号实际用哪个值」。必须用**实际值**
    /// 而不是编译期常量：用户改过全局阈值后返回常量会误导调用方。
    pub fn effective_default_percent(&self) -> u64 {
        clamp_percent(self.default_percent.unwrap_or(DEFAULT_PERCENT))
    }

    /// 账号列表序列化成 `accounts` 键的值
    pub fn accounts_json(list: &[Account]) -> String {
        let arr: Vec<Value> = list
            .iter()
            .map(|a| serde_json::json!({"id": a.id, "name": a.name, "token": a.token}))
            .collect();
        Value::Array(arr).to_string()
    }
}

/// 某账号的阈值键名（宿主键名规则：仅 `[A-Za-z0-9_-]`，**不能有点号**）
pub fn percent_key(id: &str) -> String {
    format!("percent-{id}")
}

/// 阈值合法区间（0 或 >100 = 关闭预警，与迁移前一致）
pub fn clamp_percent(n: u64) -> u64 {
    if n > 100 {
        100
    } else {
        n
    }
}

/// 生成新账号 id（无 rand 依赖：纳秒时间 + 现有数量 + 名称哈希）
pub fn new_id(name: &str, existing: usize) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let h = name
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    let mut id = format!("a{:x}{:x}", now ^ h, existing);
    // 键名规则（宿主校验）：ASCII 字母/数字/_/-，长度 ≤64
    id.retain(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if id.len() > 52 {
        id.truncate(52); // `percent-` 前缀 8 字符 + id ≤ 56 < 64（宿主键名上限）
    }
    id
}

/// id 的短显示形式（用作缺省账号名）
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// 声明式配置回写：`{"set":{…},"remove":[…]}`
#[derive(Debug, Default)]
pub struct Writeback {
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
}

impl Writeback {
    pub fn set_kv(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.set.push((k.into(), v.into()));
    }
    pub fn remove_key(&mut self, k: impl Into<String>) {
        self.remove.push(k.into());
    }
    /// 转成返回值里的 `config` 字段（无内容时 `None`）
    pub fn to_json(&self) -> Option<Value> {
        if self.set.is_empty() && self.remove.is_empty() {
            return None;
        }
        let mut map = serde_json::Map::new();
        for (k, v) in &self.set {
            map.insert(k.clone(), Value::String(v.clone()));
        }
        let mut j = serde_json::Map::new();
        if !map.is_empty() {
            j.insert("set".to_string(), Value::Object(map));
        }
        if !self.remove.is_empty() {
            j.insert(
                "remove".to_string(),
                Value::Array(self.remove.iter().map(|k| Value::String(k.clone())).collect()),
            );
        }
        Some(Value::Object(j))
    }
}
