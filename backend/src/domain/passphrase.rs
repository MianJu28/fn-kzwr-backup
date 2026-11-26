//! 管理员口令校验闸门（`P1-6`）
//!
//! 敏感操作（导出私钥、导出/导入配置、查看审计日志）在放行前都要校验管理员
//! 口令。原先四处调用点各自写一行 `body.passphrase != state.passphrase`，有两个
//! 共同的毛病：
//!
//! 1. **非常数时间比较**：`str::eq` 一遇不同字节立即返回，比较耗时随「猜对了多少
//!    前缀」线性变化。口令校验是本地、无网络抖动掩盖的场景，这点差异足够被
//!    统计出来 ⇒ 先各算一次 BLAKE3（定长 32 字节），再用常数时间比较定长摘要。
//! 2. **无失败限速**：`keys.export` 这类接口可以无限次重放。口令是**用户自定义**
//!    的，未必是高熵随机串（很多人就是 NAS 登录口令），必须在第 N 次失败后
//!    进入冷却，否则就是在给离线字典攻击提供在线验证器。
//!
//! 因此收敛成一个 [`PassphraseGate`]：所有校验点走同一处，规则一致、可审计。

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 连续失败多少次后进入冷却
const MAX_ATTEMPTS: u32 = 5;
/// 触发冷却后的锁定时长
const LOCKOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct GateState {
    /// 连续失败次数（成功后清零）
    failures: u32,
    /// 冷却截止时刻（`None` = 未冷却）
    locked_until: Option<Instant>,
}

/// 管理员口令校验闸门（**常数时间比较 + 失败限速**）
pub struct PassphraseGate {
    state: Mutex<GateState>,
}

impl Default for PassphraseGate {
    fn default() -> Self {
        Self::new()
    }
}

impl PassphraseGate {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(GateState::default()),
        }
    }

    /// 是否处于冷却中（供调用方给出「请稍后再试」的提示文案）
    pub fn is_locked(&self) -> bool {
        let st = self.state.lock().unwrap();
        matches!(st.locked_until, Some(until) if Instant::now() < until)
    }

    /// 距冷却结束还剩多少秒（已冷却结束则 `None`）
    pub fn lock_remaining_secs(&self) -> Option<u64> {
        let st = self.state.lock().unwrap();
        st.locked_until
            .map(|until| until.saturating_duration_since(Instant::now()).as_secs())
            .filter(|s| *s > 0)
    }

    /// 校验口令
    ///
    /// - **先查冷却**：冷却期内一律拒绝（含**正确**口令 —— 否则限速形同虚设，
    ///   攻击者可以用正确口令「洗掉」计数后再继续猜）；
    /// - **比较**：双方各取 BLAKE3（定长 32B）后常数时间比较，不泄露前缀信息；
    /// - 成功清零计数；失败累加，达到 [`MAX_ATTEMPTS`] 进入 [`LOCKOUT`] 冷却。
    pub fn verify(&self, input: &str, expected: &str) -> bool {
        // 冷却检查（提前返回，不做比较 ⇒ 冷却期内不泄露任何口令信息）
        {
            let mut st = self.state.lock().unwrap();
            if let Some(until) = st.locked_until {
                if Instant::now() < until {
                    return false;
                }
                // 冷却已过：重置，让用户可以再试
                st.locked_until = None;
                st.failures = 0;
            }
        }

        // 定长摘要 + 常数时间比较（长度不等时 ring 会返回 Err，这里用 blake3 定长后必然等长）
        let a = blake3::hash(input.as_bytes());
        let b = blake3::hash(expected.as_bytes());
        let ok = ring::constant_time::verify_slices_are_equal(a.as_bytes(), b.as_bytes()).is_ok();

        let mut st = self.state.lock().unwrap();
        if ok {
            st.failures = 0;
            st.locked_until = None;
        } else {
            st.failures += 1;
            if st.failures >= MAX_ATTEMPTS {
                st.locked_until = Some(Instant::now() + LOCKOUT);
                tracing::warn!(
                    attempts = st.failures,
                    lockout_secs = LOCKOUT.as_secs(),
                    "管理员口令连续校验失败，已锁定该时长"
                );
            }
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 正确口令通过() {
        let g = PassphraseGate::new();
        assert!(g.verify("s3cret", "s3cret"));
        assert_eq!(g.lock_remaining_secs(), None);
    }

    #[test]
    fn 错误口令被拒且累计失败() {
        let g = PassphraseGate::new();
        for _ in 0..(MAX_ATTEMPTS - 1) {
            assert!(!g.verify("wrong", "s3cret"));
        }
        assert!(!g.is_locked(), "未达上限时不应冷却");
    }

    #[test]
    fn 连续失败后进入冷却_连正确口令也拒() {
        let g = PassphraseGate::new();
        for _ in 0..MAX_ATTEMPTS {
            assert!(!g.verify("wrong", "s3cret"));
        }
        assert!(g.is_locked());
        assert!(g.lock_remaining_secs().unwrap() > 0);
        // 冷却期内的**正确**口令同样拒绝：否则限速可被「先输对一次」绕过
        assert!(!g.verify("s3cret", "s3cret"));
    }

    #[test]
    fn 成功后清零计数() {
        let g = PassphraseGate::new();
        for _ in 0..(MAX_ATTEMPTS - 1) {
            assert!(!g.verify("wrong", "s3cret"));
        }
        assert!(g.verify("s3cret", "s3cret"));
        // 清零后再失败不会立刻冷却
        assert!(!g.verify("wrong", "s3cret"));
        assert!(!g.is_locked());
    }
}
