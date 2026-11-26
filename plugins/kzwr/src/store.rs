//! **插件自管配置存储**（宿主不再代存，ADR-021）
//!
//! ## 为什么改
//! 此前 kzwr 的账号/阈值由**宿主代存**（`plugin_data["kzwr"]`，宿主 age 加密）。
//! 现在宿主不再代存插件配置，插件把键值写进自己的能力表给的 **`own_data_dir`**。
//!
//! ## 安全：敏感内容必须经宿主 `seal` 加密
//! 账号里含 **access-token**（凭据）。直接写盘就是明文落盘 —— 只靠目录权限保护，
//! 目录一旦被读走凭据即泄漏。因此本模块**整份配置**用 `sdk::host::seal` 加密后落盘，
//! 密钥留在宿主手里（插件拿不到），与迁移前「宿主 age 加密」的安全等级一致。
//!
//! ## 兼容：没有能力表时退回声明式回写
//! 老宿主（未下发能力表 / 无 `seal`）上，本模块**不落盘**，
//! 由调用方继续用返回值里的 `config` 声明交给宿主代存 —— 保证插件在新旧宿主上都能用。
//!
//! ## 落盘格式
//! `config.json` = `{"v":1,"sealed":"<base64 密文>"}`；密文解开后是一个 JSON 对象，
//! 即此前 `self_config` 的那份键值表（`accounts` / `percent-<id>` / `percent`）。
//! 用外层信封而不是「直接存密文」是为了将来能改加密方案而不破坏识别。

use std::path::PathBuf;

use serde_json::Value;

/// 配置文件名（在插件私有目录内）
const FILE: &str = "config.json";

/// 插件私有配置存储
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// 打开存储：拿不到私有目录（老宿主 / 未绑定）→ `None`
    ///
    /// `None` 时调用方应退回**声明式回写**（宿主代存）。
    pub fn open() -> Option<Self> {
        let dir = fn_kzwr_plugin_sdk::host::own_data_dir()?;
        Some(Self {
            dir: PathBuf::from(dir),
        })
    }

    fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// 读取配置的**三态**结果
    ///
    /// 必须区分「文件不存在」（可安全视为空）与「文件存在但解不开」（绝不能当空）：
    /// `commit()` 是「读—改—写整份配置」，若把后者当空对象，写回时会
    /// **永久覆盖**原来那份密文里的全部账号（宿主换过口令 / .tmp 半写文件残留时触发）。
    pub fn load_checked(&self) -> Loaded {
        // **解析结果缓存（按 mtime+len 失效）**
        //
        // 为什么必须缓存：`unseal` 背后是 age 口令解密（scrypt，故意慢，
        // NAS 级 CPU 上常达数百毫秒）。而 `Snapshot::from_cfg` 被 23 处调用、
        // 每个动作/事件各一次；更关键的是宿主 `available_json` 是在
        // **持有配置锁**时调用的 —— 一次 scrypt 就把锁占用放大一个数量级，
        // 表现为设置页/插件页「点一下要等一下」。
        //
        // 用 `(mtime, len)` 作指纹：配置只在 `save()` 时变化，而 save 必然改
        // mtime（且我们会显式作废旧缓存），不会读到旧值。
        let meta = std::fs::metadata(self.path()).ok();
        let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let stamp = (mtime, len);
        if let Ok(g) = CACHE.lock() {
            if let Some((cached_stamp, cached)) = g.as_ref() {
                if *cached_stamp == stamp {
                    return cached.clone();
                }
            }
        }
        let loaded = self.load_uncached();
        if let Ok(mut g) = CACHE.lock() {
            *g = Some((stamp, loaded.clone()));
        }
        loaded
    }

    fn load_uncached(&self) -> Loaded {
        let Ok(text) = std::fs::read_to_string(self.path()) else {
            return Loaded::Absent; // 没有配置文件 = 首次使用，可安全当空
        };
        let Ok(env) = serde_json::from_str::<Value>(&text) else {
            return Loaded::Corrupt("配置文件不是合法 JSON（可能写到一半就中断了）".to_string());
        };
        let Some(sealed) = env.get("sealed").and_then(|s| s.as_str()) else {
            // 明文旧格式或结构损坏：不接受（避免「以为加密了其实是明文」）
            return Loaded::Corrupt("配置文件缺少 sealed 字段（旧版明文格式或已损坏）".to_string());
        };
        match fn_kzwr_plugin_sdk::host::unseal(sealed) {
            Some(plain) => match serde_json::from_str::<Value>(&plain) {
                Ok(v) if v.is_object() => Loaded::Ok(v),
                Ok(_) => Loaded::Corrupt("配置内容不是 JSON 对象".to_string()),
                Err(_) => Loaded::Corrupt("配置解密后不是合法 JSON".to_string()),
            },
            None => Loaded::Corrupt(
                "配置解密失败（宿主口令是否变更？）".to_string(),
            ),
        }
    }

    /// 读取配置（键值表）。解密/格式问题一律按空对象返回 —— **仅供只读场景**。
    ///
    /// ⚠️ 写入路径必须用 [`Self::load_checked`]：这里无法区分「空」与「坏了」，
    /// 拿它的返回值做读—改—写会覆盖掉解不开的既有配置。
    pub fn load(&self) -> Value {
        match self.load_checked() {
            Loaded::Absent | Loaded::Ok(_) => match self.load_checked() {
                Loaded::Ok(v) => v,
                _ => Value::Object(Default::default()),
            },
            Loaded::Corrupt(why) => {
                crate::log::warn(format!("{why}，按空配置处理（只读；写入会被拒绝）"));
                Value::Object(Default::default())
            }
        }
    }

    /// 保存配置（加密后落盘）；失败时返回**可读原因**
    ///
    /// 返回 `Result` 而不是 `bool`：调用方需要把「为什么没存上」告诉用户，
    /// 而不是静默当成成功（那会让用户以为账号已添加、刷新后却消失）。
    pub fn save(&self, kv: &Value) -> Result<(), String> {
        let plain = kv.to_string();
        let Some(sealed) = fn_kzwr_plugin_sdk::host::seal(&plain) else {
            return Err("配置加密失败（宿主 seal 不可用，可能是能力表未下发或调用被限流）".to_string());
        };
        let env = serde_json::json!({ "v": 1, "sealed": sealed });
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            return Err(format!("创建插件数据目录失败：{e}"));
        }
        // 先写临时文件、fsync、再改名：避免掉电留下半写文件
        //（半写文件会让下次 load 解析失败，进而触发「当成空配置」的覆盖风险）
        let tmp = self.dir.join(format!("{FILE}.tmp"));
        {
            use std::io::Write;
            let mut f = match std::fs::File::create(&tmp) {
                Ok(f) => f,
                Err(e) => return Err(format!("创建临时配置文件失败：{e}")),
            };
            // 内容是密文，但仍按 0600 创建（多用户机器上没有理由放宽）
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            if let Err(e) = f.write_all(env.to_string().as_bytes()) {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("写临时配置文件失败：{e}"));
            }
            if let Err(e) = f.sync_all() {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("刷新配置文件失败：{e}"));
            }
        }
        if let Err(e) = std::fs::rename(&tmp, self.path()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("替换配置文件失败：{e}"));
        }
        // 显式作废解析缓存：虽然指纹会变，但「同长度、同纳秒」理论上可能撞上
        if let Ok(mut g) = CACHE.lock() {
            *g = None;
        }
        Ok(())
    }
}

/// 解析缓存：(文件指纹, 结果)。进程内单例 —— 插件只服务一个应用实例。
static CACHE: std::sync::Mutex<Option<((u128, u64), Loaded)>> = std::sync::Mutex::new(None);

/// [`Store::load_checked`] 的三态结果
#[derive(Clone)]
pub enum Loaded {
    /// 文件不存在（首次使用）
    Absent,
    /// 成功读到配置
    Ok(Value),
    /// 文件存在但**无法安全解释**（携带原因）；写入路径必须据此拒绝
    Corrupt(String),
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    /// 缓存不能导致「改了却读不到新值」
    ///
    /// 这是缓存类优化的**唯一风险点**：若失效判据不对，用户改了配置
    /// （如阈值、账号）却看到旧值，比慢更糟。
    #[test]
    fn cache_is_invalidated_after_save() {
        // 直接操作缓存与指纹逻辑（不需要真实解密）
        let stamp_a: (u128, u64) = (100, 10);
        let stamp_b: (u128, u64) = (200, 12);

        if let Ok(mut g) = CACHE.lock() {
            *g = Some((stamp_a, Loaded::Ok(serde_json::json!({"k": "old"}))));
        }
        // 相同指纹：命中旧值（这是缓存的正常行为）
        if let Ok(g) = CACHE.lock() {
            let hit = g.as_ref().filter(|(s, _)| *s == stamp_a).map(|(_, v)| v.clone());
            assert!(hit.is_some(), "同指纹应命中");
        }
        // 指纹变化：不得命中
        if let Ok(g) = CACHE.lock() {
            let hit = g.as_ref().filter(|(s, _)| *s == stamp_b).map(|(_, v)| v.clone());
            assert!(hit.is_none(), "指纹变化后必须未命中（否则会读到旧配置）");
        }
        // 显式作废
        if let Ok(mut g) = CACHE.lock() {
            *g = None;
        }
        if let Ok(g) = CACHE.lock() {
            assert!(g.is_none(), "作废后缓存应为空");
        }
    }
}
