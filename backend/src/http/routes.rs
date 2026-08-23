//! REST 路由定义（axum）

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::domain::backup::BackupJob;
use crate::domain::restore::RestoreJob;
use crate::http::ws;
use crate::AppState;

// ── 响应/请求结构 ──────────────────────────────

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// 用户信息响应（来自 get_member + 配置）
#[derive(Serialize, Default)]
pub struct UserInfoResponse {
    pub email: Option<String>,
    pub name: Option<String>,
    pub avatar: Option<String>,
    pub plan: Option<String>,
    /// 总容量（字节）
    pub total: u64,
    /// 已用容量（字节）
    pub use_bytes: u64,
    /// 已用百分比（如 "8.38%"）
    pub percentage: Option<String>,
    /// 配置中记录的登录用户名
    pub logged_in_username: Option<String>,
    pub error: Option<String>,
}

/// kzwr 登录请求
#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// 登录环境状态（Camoufox 浏览器 + uBlock addon 就绪 + 初始化任务状态）
#[derive(Serialize, Clone)]
pub struct LoginEnvStatus {
    /// Camoufox 浏览器二进制是否就绪（$CACHE_DIR/camoufox/camoufox-bin 存在且可执行）
    pub camoufox_browser: bool,
    /// uBlock Origin addon 是否就绪（$CACHE_DIR/camoufox/addons/UBO/manifest.json 存在）
    pub ubo: bool,
    /// 综合：两者皆就绪
    pub ready: bool,
    /// 缓存目录（TRIM_LOGIN_CACHE_DIR）
    pub cache_dir: String,
    /// 当前请求的 Camoufox 版本（默认）
    pub default_camoufox_version: String,
    /// 可选国内镜像源列表
    pub mirrors: Vec<String>,
    /// 初始化任务状态：idle | running | done | error | cancelled
    pub state: String,
    /// 是否正在初始化
    pub running: bool,
    /// 下载进度 0-100（-1 表示未知）
    pub progress: i32,
    /// 最近一条状态/进度消息
    pub message: String,
    /// 当前使用的镜像
    pub mirror: String,
    /// 最近更新时间（unix 秒）
    pub updated_at: u64,
}

/// 初始化登录环境请求（可选指定国内镜像 + Camoufox 版本）
#[derive(Deserialize, Default)]
pub struct InitLoginEnvRequest {
    #[serde(default)]
    pub camoufox_version: Option<String>,
    #[serde(default)]
    pub mirror: Option<String>,
}

/// 初始化状态持久化文件内容
#[derive(Serialize, Deserialize, Clone, Default)]
struct LoginEnvInitState {
    #[serde(default)]
    pub state: String, // idle|running|done|error|cancelled
    #[serde(default)]
    pub progress: i32,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub mirror: String,
    #[serde(default)]
    pub camoufox_version: String,
    #[serde(default)]
    pub updated_at: u64,
}

/// 当前正在运行的下载任务 curl PID（供取消）
static CURRENT_DL_PID: std::sync::OnceLock<std::sync::Mutex<Option<u32>>> =
    std::sync::OnceLock::new();

fn current_dl_pid() -> &'static std::sync::Mutex<Option<u32>> {
    CURRENT_DL_PID.get_or_init(|| std::sync::Mutex::new(None))
}

/// 初始化取消标志：cancel 时置 true，download 主循环检查后及时退出。
static DL_CANCEL_FLAG: std::sync::OnceLock<std::sync::atomic::AtomicBool> =
    std::sync::OnceLock::new();

fn dl_cancel_flag() -> &'static std::sync::atomic::AtomicBool {
    DL_CANCEL_FLAG.get_or_init(|| std::sync::atomic::AtomicBool::new(false))
}

/// 登录环境初始化状态文件路径
fn login_env_state_path(cache_dir: &str) -> PathBuf {
    PathBuf::from(cache_dir).join(".login_env_init.json")
}

/// 读取初始化状态文件（不存在则返回 idle 默认）
fn read_login_env_state(cache_dir: &str) -> LoginEnvInitState {
    let p = login_env_state_path(cache_dir);
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写入初始化状态文件
fn write_login_env_state(cache_dir: &str, st: &LoginEnvInitState) {
    let p = login_env_state_path(cache_dir);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut st = st.clone();
    st.updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(s) = serde_json::to_string(&st) {
        let _ = std::fs::write(&p, s);
    }
}

/// 登录响应
#[derive(Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub username: Option<String>,
    pub error: Option<String>,
}

/// 配置响应（不回传敏感字段明文）
#[derive(Serialize)]
pub struct ConfigResponse {
    pub backup_paths: Vec<String>,
    pub target_folder: String,
    /// 定时备份 cron 表达式（空 = 未启用）
    pub schedule_cron: String,
    /// cron 表达式是否合法（供前端提示）
    pub schedule_cron_valid: bool,
    pub logged_in: bool,
    pub login_bin_available: bool,
    /// 是否开启登录二进制 debug 日志
    pub login_debug: bool,
    pub error: Option<String>,
}

/// 配置保存请求
#[derive(Deserialize)]
pub struct ConfigSaveRequest {
    pub backup_paths: Vec<String>,
    pub target_folder: Option<String>,
    /// 定时备份 cron 表达式（空 = 关闭定时）
    pub schedule_cron: Option<String>,
    /// 是否开启登录二进制 debug 日志
    #[serde(default)]
    pub login_debug: Option<bool>,
}

/// 备份响应
#[derive(Serialize)]
pub struct BackupResponse {
    pub uploaded: usize,
    pub uploaded_bytes: u64,
    pub deleted: usize,
    pub unchanged: usize,
    /// 保留策略清理的孤儿文件数
    pub orphan_removed: usize,
    pub error: Option<String>,
}

/// 恢复请求体
#[derive(Deserialize)]
pub struct RestoreRequest {
    /// 要恢复的文件相对路径列表；空 = 全量
    pub files: Option<Vec<String>>,
    /// 恢复目标根目录（未传则用配置的备份源路径，恢复到原位置）
    pub source_path: Option<String>,
}

/// 恢复响应
#[derive(Serialize)]
pub struct RestoreResponse {
    pub restored: usize,
    pub restored_bytes: u64,
    pub error: Option<String>,
}

/// 可恢复文件条目
#[derive(Serialize)]
pub struct RestorableFile {
    pub rel_path: String,
    pub size: u64,
    pub is_dir: bool,
}

/// 一个备份文件夹及其可恢复文件
#[derive(Serialize)]
pub struct RestorableFolder {
    /// 备份源路径（本地目录）
    pub path: String,
    /// 是否已有备份数据（SQLite 快照）
    pub has_backup: bool,
    /// 文件列表
    pub files: Vec<RestorableFile>,
}

/// 恢复文件列表响应
#[derive(Serialize)]
pub struct RestoreFilesResponse {
    pub folders: Vec<RestorableFolder>,
    pub error: Option<String>,
}

// ── Handler ────────────────────────────────────

async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// 登录 kzwr：保存凭据（加密）并获取 token
async fn auth_login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Json<LoginResponse> {
    match state
        .auth
        .login_with_credentials(&body.username, &body.password)
    {
        Ok(_) => Json(LoginResponse {
            success: true,
            username: Some(body.username),
            error: None,
        }),
        Err(e) => Json(LoginResponse {
            success: false,
            username: None,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 检测登录环境状态（Camoufox 浏览器 + uBlock + 初始化任务状态）
/// 前端页面打开时调用；结合持久化状态文件，避免初始化完成后重复提示。
async fn login_env_status(State(_state): State<AppState>) -> Json<LoginEnvStatus> {
    let cache_dir = std::env::var(crate::infra::kzwr_auth::CACHE_DIR_ENV)
        .unwrap_or_default();
    let camo_dir = PathBuf::from(&cache_dir).join("camoufox");
    let ubo_manifest = camo_dir
        .join("addons")
        .join("UBO")
        .join("manifest.json");
    // camoufox 判定浏览器已安装：config.json 的 active_version 指向的版本子目录内有 camoufox-bin（可执行）+ version.json
    let browser_ok = active_camoufox_bin(&camo_dir)
        .map(|bin| {
            bin.join("version.json").is_file()
                && bin.join("camoufox-bin").is_file()
                && {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        bin.join("camoufox-bin")
                            .metadata()
                            .map(|m| m.permissions().mode() & 0o111 != 0)
                            .unwrap_or(false)
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                }
        })
        .unwrap_or(false);
    let ubo_ok = ubo_manifest.is_file();

    // 读取持久化初始化状态（进度/消息/镜像）
    let init = read_login_env_state(&cache_dir);
    // running 判定：state==running 且最近 10 分钟内活跃。
    // 不依赖 pid（下载完成进入解压阶段时 pid 已清空，但状态仍是 running，前端需保持进度显示）。
    // 若后端重启导致僵尸 running（updated_at 陈旧）则视为 idle。
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let running = init.state == "running"
        && init.updated_at > 0
        && now.saturating_sub(init.updated_at) < 600;

    // 完成判定：文件就绪（浏览器+uBlock）即 ready，无论状态文件如何
    let ready = browser_ok && ubo_ok;
    // 若已 ready 但状态文件仍标记 running/非 done，纠正为 done
    let state = if ready && init.state != "done" {
        let mut st = init.clone();
        st.state = "done".to_string();
        st.message = "登录环境已就绪".to_string();
        write_login_env_state(&cache_dir, &st);
        "done".to_string()
    } else if running {
        "running".to_string()
    } else {
        init.state.clone()
    };

    Json(LoginEnvStatus {
        camoufox_browser: browser_ok,
        ubo: ubo_ok,
        ready,
        cache_dir: cache_dir.clone(),
        default_camoufox_version: "152.0.4-beta.28".to_string(),
        mirrors: vec![
            "gh-proxy.com".to_string(),
            "ghproxy.net".to_string(),
            "ghfast.top".to_string(),
        ],
        state,
        running,
        progress: init.progress,
        message: init.message,
        mirror: init.mirror,
        updated_at: init.updated_at,
    })
}

/// 确保 Xvfb 可用（登录二进制 headless="virtual" 需要）。
/// 检查 Xvfb 是否存在 + 试运行；缺失则尝试 apt 安装（补 Mesa 软件渲染 + Firefox 依赖）。
/// 同时检测 /tmp 可写性，若不可写（Xvfb 写键盘映射文件会失败）则把 TMPDIR 指向 cache_dir 下可写目录。
/// 返回详细诊断日志（含 apt 输出），供状态文件/日志文件展示。
fn ensure_xvfb(cache_dir: &str) -> String {
    let mut log = String::new();
    let now_line = format!(
        "===== Xvfb 检测 {:?} =====\n",
        chrono::Local::now()
    );
    log.push_str(&now_line);

    let sh = |cmd: &str| -> (bool, String) {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output();
        match out {
            Ok(o) => (
                o.status.success(),
                format!(
                    "stdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                ),
            ),
            Err(e) => (false, format!("执行失败: {}", e)),
        }
    };

    // 0) 检测/修复 /tmp 与 /tmp/.X11-unix 可写性。
    //    Xvfb 需写 /tmp/server-N.xkm（键盘映射，可用 TMPDIR 改）和 /tmp/.X11-unix（unix socket，X 协议固定，无法用 TMPDIR 改）。
    //    若 /tmp 只读（如只读挂载），Xvfb 无法建 unix socket → Failed to find a socket to listen on → 登录失败。
    log.push_str("[*] 检测 /tmp 与 /tmp/.X11-unix ...\n");
    let (tmp_writable, _) = sh("test -w /tmp && echo YES || echo NO");
    log.push_str(&format!("/tmp 可写: {}\n", if tmp_writable { "是" } else { "否" }));

    // 尝试修复 /tmp/.X11-unix（Xvfb unix socket 目录，需 1777）。优先 sudo -n，失败则直接。
    let fix_cmds = [
        "sudo -n mkdir -p /tmp/.X11-unix && sudo -n chmod 1777 /tmp/.X11-unix 2>&1 && test -w /tmp/.X11-unix && echo FIXED",
        "mkdir -p /tmp/.X11-unix && chmod 1777 /tmp/.X11-unix 2>&1 && test -w /tmp/.X11-unix && echo FIXED",
    ];
    let mut x11_fixed = false;
    for c in &fix_cmds {
        let (ok, out) = sh(c);
        if out.contains("FIXED") {
            x11_fixed = true;
            log.push_str(&format!("[+] /tmp/.X11-unix 已修复为可写 ({}):\n{}\n", c, out));
            break;
        }
        let _ = ok;
    }
    if !x11_fixed {
        log.push_str("[!] /tmp/.X11-unix 仍不可写，Xvfb 将无法创建 unix socket。诊断信息如下：\n");
        let (_, ls_out) = sh("ls -ld /tmp/.X11-unix 2>&1; stat -c 'mode=%a owner=%U group=%G' /tmp/.X11-unix 2>&1; mount | grep -E ' /tmp |/tmp/.X11-unix' 2>&1; echo ---; touch /tmp/.X11-unix/.wtest 2>&1 && echo WRITABLE && rm -f /tmp/.X11-unix/.wtest || echo NOT_WRITABLE");
        log.push_str(&ls_out);
        log.push_str("[!] 若 ls 显示非 1777 且 owner 非 root，需 root 执行: chmod 1777 /tmp/.X11-unix；若为只读挂载(ro)，需: mount -o remount,rw /tmp\n");
    }

    // 键盘映射文件用 TMPDIR 指向可写目录（登录二进制继承，其 Xvfb 用此目录写 xkb 文件）
    let xvfb_tmp = std::path::PathBuf::from(cache_dir).join("xvfb-tmp");
    if !tmp_writable {
        let _ = std::fs::create_dir_all(&xvfb_tmp);
        std::env::set_var("TMPDIR", &xvfb_tmp);
        log.push_str(&format!(
            "[+] /tmp 不可写，已设置 TMPDIR={}（解决 xkb 键盘文件）\n",
            xvfb_tmp.display()
        ));
    }

    // 1) Xvfb 是否存在
    let (xvfb_exists, _) = sh("command -v Xvfb");
    if !xvfb_exists {
        log.push_str("[!] 未找到 Xvfb，尝试安装...\n");
        // 优先 sudo -n（飞牛应用进程可能非 root），失败则直接 apt
        let install_cmds = [
            "sudo -n apt-get update && sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y xvfb libgl1 libgl1-mesa-dri libosmesa6 libegl1 libglu1-mesa libnspr4 libnss3 libfontconfig1 2>&1",
            "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y xvfb libgl1 libgl1-mesa-dri libosmesa6 libegl1 libglu1-mesa libnspr4 libnss3 libfontconfig1 2>&1",
        ];
        let mut installed = false;
        for c in &install_cmds {
            log.push_str(&format!("$ {}\n", c));
            let (ok, out) = sh(c);
            log.push_str(&out);
            if ok {
                installed = true;
                break;
            }
        }
        if installed {
            log.push_str("[+] Xvfb 安装完成\n");
        } else {
            log.push_str("[!] Xvfb 安装失败（可能无 root 权限或软件源缺包），登录需 Xvfb\n");
            return log;
        }
    } else {
        log.push_str("[+] Xvfb 已存在\n");
    }

    // 2) 试运行 Xvfb 验证——**用 camoufox 的完整参数**（含 GLX/RENDER 扩展 + displayfd），
    //    以复现登录二进制调用 camoufox 时 Xvfb 的确切失败原因。
    log.push_str("[*] 试运行 Xvfb（camoufox 完整参数，含 GLX 扩展）...\n");
    // 注意：不能用 `pkill -f 'Xvfb -displayfd'`（会匹配并杀掉执行本命令的 shell 自身，导致无输出）。
    // 用 `pkill -x Xvfb` 精确匹配进程名清理残留，不影响自身 shell。
    let (ok, out) = sh(
        "pkill -x Xvfb 2>/dev/null; sleep 0.5; rm -f /tmp/.X11-unix/X* /tmp/.X*-lock 2>/dev/null; D=$(mktemp -d); exec 3>\"$D/fd\"; TMPDIR=\"${TMPDIR:-/tmp}\" Xvfb -displayfd 3 -screen 0 1x1x24 -ac -nolisten tcp -extension RENDER +extension GLX -extension COMPOSITE -extension XVideo -extension XVideo-MotionCompensation -extension XINERAMA -fp built-ins -nocursor -br >/dev/null 2>\"$D/xvfb.err\" & P=$!; sleep 1.5; if kill -0 $P 2>/dev/null; then echo \"RUNNING displayfd=$(cat $D/fd)\"; kill $P 2>/dev/null; else echo FAILED; cat \"$D/xvfb.err\"; fi; rm -rf $D",
    );
    log.push_str(&out);
    if out.contains("RUNNING") {
        log.push_str("[+] Xvfb 可正常启动（含 GLX 扩展，TMPDIR 已生效）\n");
    } else {
        log.push_str("[!] Xvfb 用 camoufox 完整参数启动失败，登录会报 CannotExecuteXvfb\n");
    }
    let _ = ok;

    // 3) 若失败，检查关键扩展库是否缺失
    log.push_str("[*] 检查 Mesa 软件渲染与 GLX 库 ...\n");
    let (_, gl_out) = sh("ls -l /usr/lib/x86_64-linux-gnu/libGL.so.1 /usr/lib/x86_64-linux-gnu/dri/swrast_dri.so /usr/lib/x86_64-linux-gnu/libOSMesa.so* 2>&1; echo ---; command -v glxinfo && glxinfo -B 2>&1 | head -5 || echo 'glxinfo 不可用'");
    log.push_str(&gl_out);

    log
}

/// 用 Rust zip 解压 zip 到目标目录（不依赖系统 unzip，飞牛精简系统也适用）。
/// 自动防护路径穿越（跳过绝对路径/..）。
fn unzip_to_dir(zip_path: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开 zip 失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("解析 zip 失败: {}", e))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取 zip 条目失败: {}", e))?;
        // 防护路径穿越
        let name = entry.name().replace('\\', "/");
        if name.starts_with('/') || name.split('/').any(|seg| seg == "..") {
            continue;
        }
        let out_path = dest.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(p) = out_path.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut f = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 初始化登录环境：解压 uBlock + 国内镜像下载 Camoufox 浏览器。
/// 后台任务更新持久化状态文件（进度/消息），前端轮询 /login-env/status 获取进度。
async fn login_env_init(
    State(_state): State<AppState>,
    body: Option<Json<InitLoginEnvRequest>>,
) -> Json<LoginEnvStatus> {
    let req = body.map(|j| j.0).unwrap_or_default();
    let version = req
        .camoufox_version
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "152.0.4-beta.28".to_string());
    let mirror = req
        .mirror
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "gh-proxy.com".to_string());

    let cache_dir = std::env::var(crate::infra::kzwr_auth::CACHE_DIR_ENV).unwrap_or_default();

    // 若已有下载任务在跑，拒绝重复启动
    if current_dl_pid().lock().unwrap().is_some() {
        return login_env_status(State(_state)).await;
    }

    // 记录初始化状态为 running
    write_login_env_state(
        &cache_dir,
        &LoginEnvInitState {
            state: "running".to_string(),
            progress: 0,
            message: "开始初始化...".to_string(),
            mirror: mirror.clone(),
            camoufox_version: version.clone(),
            updated_at: 0,
        },
    );

    // 后台任务执行初始化（解压 uBlock + 下载浏览器），写状态文件
    tokio::spawn(async move {
        let cache_dir = std::env::var(crate::infra::kzwr_auth::CACHE_DIR_ENV).unwrap_or_default();
        if cache_dir.is_empty() {
            write_login_env_state(
                &cache_dir,
                &LoginEnvInitState {
                    state: "error".to_string(),
                    progress: 0,
                    message: "未设置 TRIM_LOGIN_CACHE_DIR".to_string(),
                    mirror: mirror.clone(),
                    camoufox_version: version.clone(),
                    updated_at: 0,
                },
            );
            return;
        }
        let camo_dir = std::path::PathBuf::from(&cache_dir).join("camoufox");
        let ubo_dir = camo_dir.join("addons").join("UBO");
        let _ = std::fs::create_dir_all(&camo_dir);
        let _ = std::fs::create_dir_all(&ubo_dir);

        // ── 0. 检测/安装 Xvfb（登录二进制 headless="virtual" 需要），记录详细日志 ──
        write_login_env_state(
            &cache_dir,
            &LoginEnvInitState {
                state: "running".to_string(),
                progress: 0,
                message: "检测/安装 Xvfb 虚拟显示...".to_string(),
                mirror: mirror.clone(),
                camoufox_version: version.clone(),
                updated_at: 0,
            },
        );
        let cache_dir_for_xvfb = cache_dir.clone();
        let setup_log = tokio::task::spawn_blocking(move || ensure_xvfb(&cache_dir_for_xvfb))
            .await
            .unwrap_or_default();
        // 详细日志写入 setup 日志文件
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::path::PathBuf::from(&cache_dir).join("login_env_setup.log"))
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(setup_log.as_bytes())
            });

        // ── 1. 解压 uBlock ──
        let ubo_xpi = std::path::PathBuf::from(&cache_dir).join("ubo.xpi");
        if !ubo_dir.join("manifest.json").is_file() {
            if !ubo_xpi.is_file() {
                write_login_env_state(
                    &cache_dir,
                    &LoginEnvInitState {
                        state: "error".to_string(),
                        progress: 0,
                        message: "未找到 fpk 内置 ubo.xpi".to_string(),
                        mirror: mirror.clone(),
                        camoufox_version: version.clone(),
                        updated_at: 0,
                    },
                );
                return;
            }
            write_login_env_state(
                &cache_dir,
                &LoginEnvInitState {
                    state: "running".to_string(),
                    progress: 0,
                    message: "正在解压 uBlock Origin addon...".to_string(),
                    mirror: mirror.clone(),
                    camoufox_version: version.clone(),
                    updated_at: 0,
                },
            );
            let _ = std::fs::remove_dir_all(&ubo_dir);
            let _ = std::fs::create_dir_all(&ubo_dir);
            let ubo_xpi2 = ubo_xpi.clone();
            let ubo_dir2 = ubo_dir.clone();
            let unzip_res = tokio::task::spawn_blocking(move || {
                unzip_to_dir(&ubo_xpi2, &ubo_dir2)
            })
            .await;
            if !matches!(unzip_res, Ok(Ok(()))) {
                write_login_env_state(
                    &cache_dir,
                    &LoginEnvInitState {
                        state: "error".to_string(),
                        progress: 0,
                        message: "uBlock 解压失败".to_string(),
                        mirror: mirror.clone(),
                        camoufox_version: version.clone(),
                        updated_at: 0,
                    },
                );
                return;
            }
        }

        // ── 2. 下载 Camoufox 浏览器（若未就绪；用 config.json 的 active_version 定位）──
        let browser_ready = active_camoufox_bin(&camo_dir)
            .map(|b| b.join("camoufox-bin").is_file())
            .unwrap_or(false);
        let dl_result = if browser_ready {
            write_login_env_state(
                &cache_dir,
                &LoginEnvInitState {
                    state: "running".to_string(),
                    progress: 100,
                    message: "Camoufox 浏览器已就绪".to_string(),
                    mirror: mirror.clone(),
                    camoufox_version: version.clone(),
                    updated_at: 0,
                },
            );
            Ok(())
        } else {
            write_login_env_state(
                &cache_dir,
                &LoginEnvInitState {
                    state: "running".to_string(),
                    progress: 0,
                    message: format!("开始下载 Camoufox {}（镜像 {}）...", version, mirror),
                    mirror: mirror.clone(),
                    camoufox_version: version.clone(),
                    updated_at: 0,
                },
            );
            download_camoufox_browser(&version, &mirror, &cache_dir, &camo_dir).await
        };

        match dl_result {
            Ok(()) => write_login_env_state(
                &cache_dir,
                &LoginEnvInitState {
                    state: "done".to_string(),
                    progress: 100,
                    message: "登录环境已就绪".to_string(),
                    mirror: mirror.clone(),
                    camoufox_version: version.clone(),
                    updated_at: 0,
                },
            ),
            Err(e) => {
                // 若用户已取消（状态文件被 cancel 置为 cancelled），则不覆盖为 error
                let cur = read_login_env_state(&cache_dir);
                if cur.state == "cancelled" {
                    // 保持 cancelled，不覆盖
                } else {
                    write_login_env_state(
                        &cache_dir,
                        &LoginEnvInitState {
                            state: "error".to_string(),
                            progress: 0,
                            message: format!("初始化失败: {}", e),
                            mirror: mirror.clone(),
                            camoufox_version: version.clone(),
                            updated_at: 0,
                        },
                    );
                }
            }
        }
        // 清理下载句柄
        *current_dl_pid().lock().unwrap() = None;
    });

    login_env_status(State(_state)).await
}

/// 取消初始化下载任务
async fn login_env_cancel(State(_state): State<AppState>) -> Json<LoginEnvStatus> {
    let cache_dir = std::env::var(crate::infra::kzwr_auth::CACHE_DIR_ENV).unwrap_or_default();
    // 置取消标志，通知 download 主循环及时退出（防止卡死）
    dl_cancel_flag().store(true, std::sync::atomic::Ordering::Relaxed);
    // kill 当前下载 curl 进程（真实 pid）
    if let Some(pid) = current_dl_pid().lock().unwrap().take() {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
    // 清理临时下载文件
    let camo_dir = PathBuf::from(&cache_dir).join("camoufox");
    let _ = std::fs::remove_file(camo_dir.join("__camoufox_browser.zip"));
    // 重置状态为 cancelled
    write_login_env_state(
        &cache_dir,
        &LoginEnvInitState {
            state: "cancelled".to_string(),
            progress: 0,
            message: "已取消初始化".to_string(),
            mirror: String::new(),
            camoufox_version: String::new(),
            updated_at: 0,
        },
    );
    login_env_status(State(_state)).await
}

/// 国内镜像下载 Camoufox 浏览器并解压到 camo_dir。
/// 由版本构造 URL（已确认资产名）：
///   https://github.com/daijro/camoufox/releases/download/v{version}/camoufox-{version}-lin.x86_64.zip
/// 用指定国内镜像下载，curl 进度条解析百分比写入持久化状态文件；记录 curl PID 供取消。
async fn download_camoufox_browser(
    version: &str,
    mirror: &str,
    cache_dir: &str,
    camo_dir: &std::path::Path,
) -> Result<(), String> {
    let tmp_zip = camo_dir.join("__camoufox_browser.zip");
    let _ = std::fs::remove_file(&tmp_zip);

    let tag = if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{}", version)
    };
    let asset = format!("camoufox-{}-lin.x86_64.zip", version);
    let gh_url = format!(
        "https://github.com/daijro/camoufox/releases/download/{}/{}",
        tag, asset
    );

    // 写状态 helper（闭包捕获 cache_dir）
    let set_state = |st: &str, prog: i32, msg: &str| {
        write_login_env_state(
            cache_dir,
            &LoginEnvInitState {
                state: st.to_string(),
                progress: prog,
                message: msg.to_string(),
                mirror: mirror.to_string(),
                camoufox_version: version.to_string(),
                updated_at: 0,
            },
        );
    };

    // 用用户选择的镜像下载。
    // 注意：curl `-#` 进度条在 stderr 非终端（管道）时不输出，无法实时解析百分比。
    // 改为：先 HEAD 拿 Content-Length，下载时轮询临时文件大小计算进度。
    let url = format!("https://{}/{}", mirror, gh_url);

    // 1) HEAD 获取总大小（经国内镜像）
    let mut total: u64 = 0;
    for head_m in [mirror] {
        let head_url = format!("https://{}/{}", head_m, gh_url);
        let head_out = tokio::process::Command::new("curl")
            .args(["-sIL", "--connect-timeout", "15", "--max-time", "30", &head_url])
            .output()
            .await;
        if let Ok(o) = head_out {
            let text = String::from_utf8_lossy(&o.stdout);
            for line in text.lines() {
                let l = line.to_lowercase();
                if let Some(pos) = l.find("content-length:") {
                    if let Some(v) = l[pos + 15..].trim().parse::<u64>().ok() {
                        if v > 0 {
                            total = v;
                        }
                    }
                }
            }
            if total > 0 {
                break;
            }
        }
    }

    set_state("running", 0, &format!("开始下载（镜像 {}）...", mirror));

    // 2) 用 tokio::process::Command 启动 curl（真实 pid，可被 cancel kill）
    //    主循环轮询文件大小算进度 + 检查取消标志，取消时 kill curl 及时退出。
    dl_cancel_flag().store(false, std::sync::atomic::Ordering::Relaxed);
    let tmp_zip_dl = tmp_zip.clone();
    let url_dl = url.clone();
    let mut child = tokio::process::Command::new("curl")
        .args([
            "-sL",
            "--connect-timeout",
            "20",
            "--max-time",
            "3600",
            "-o",
        ])
        .arg(&tmp_zip_dl)
        .arg(&url_dl)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("curl 启动失败: {}", e))?;
    // 记录真实 PID 供取消
    {
        let pid = child.id();
        *current_dl_pid().lock().unwrap() = pid;
    }

    // 3) 主循环轮询文件大小算进度 + 检查取消
    let mut last_pct: i32 = -1;
    loop {
        // 被取消：kill curl，立即退出
        if dl_cancel_flag().load(std::sync::atomic::Ordering::Relaxed) {
            let _ = child.kill().await;
            break;
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let cur = std::fs::metadata(&tmp_zip).map(|m| m.len()).unwrap_or(0);
        if total > 0 {
            let p = ((cur as f64 / total as f64) * 100.0) as i32;
            let p = p.clamp(0, 99);
            if p != last_pct {
                last_pct = p;
                set_state("running", p, &format!("下载中 {}%（{} MB / {} MB）", p, cur / 1024 / 1024, total / 1024 / 1024));
            }
        }
    }

    let status = child.wait().await.map_err(|e| e.to_string())?;
    *current_dl_pid().lock().unwrap() = None;
    let status_success = status.success();

    let ok_size = std::fs::metadata(&tmp_zip)
        .map(|m| m.len() > 1_000_000)
        .unwrap_or(false);
    if !(status_success && ok_size) {
        let _ = std::fs::remove_file(&tmp_zip);
        return Err(format!("下载失败 (exit {:?})", status.code()));
    }

    // 解压到 camoufox 期望的版本子目录：browsers/official/<版本>-<sha8>/。
    // sha8 不动态计算，直接采用与飞牛一致的值（camoufox 152.0.4-beta.28 的 sha8 = 924f3109）。
    // config.json / version.json 也直接照抄飞牛上的内容，保证一致。
    set_state("running", 100, "下载完成，正在解压...");

    // 固定 sha8（与飞牛 repo_cache 中 152.0.4-beta.28 一致，不计算）
    let sha8 = "924f3109";
    let ver_tag = version.trim_start_matches('v');
    let ver_dir_name = format!("{}-{}", ver_tag, sha8);
    let official_dir = camo_dir.join("browsers").join("official");
    let ver_dir = official_dir.join(&ver_dir_name);

    let camo_dir2 = camo_dir.to_path_buf();
    let tmp_zip2 = tmp_zip.clone();
    let ver_dir2 = ver_dir.clone();
    let unzip = tokio::task::spawn_blocking(move || {
        let r = unzip_to_dir(&tmp_zip2, &ver_dir2);
        let _ = std::fs::remove_file(&tmp_zip2);
        r
    })
    .await
    .map_err(|e| format!("解压任务失败: {}", e))?;
    // chmod +x camoufox-bin（版本子目录内）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = ver_dir.join("camoufox-bin");
        if bin.is_file() {
            let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
        }
    }
    // 写版本子目录内的 version.json（照抄飞牛）
    let vdata = serde_json::json!({
        "version": "152.0.4",
        "build": "beta.28",
        "prerelease": false,
        "asset_id": null,
        "asset_size": null,
        "asset_updated_at": null,
        "sha256": "924f3109ccd6d47cd6a0384d67a345fadf975d48b6319f8dbbd5954c588982bd",
        "created_at": "2026-07-19T07:19:16Z"
    });
    if let Ok(s) = serde_json::to_string(&vdata) {
        let _ = std::fs::write(ver_dir.join("version.json"), s);
    }
    // 写根 config.json（照抄飞牛）
    let active_rel = format!("browsers/official/{}", ver_dir_name);
    let cfg_json = serde_json::json!({ "active_version": active_rel });
    if let Ok(s) = serde_json::to_string(&cfg_json) {
        let _ = std::fs::write(camo_dir.join("config.json"), s);
    }
    // 创建 COMPAT_FLAG（.0.5_FLAG 空文件）：camoufox 检测到 camoufox/ 目录有内容但缺该标志
    // 会判定为"不兼容旧数据"并 rmtree 删除整个目录。飞牛成功目录含 .0.5_FLAG。
    let flag_path = camo_dir.join(".0.5_FLAG");
    if !flag_path.exists() {
        let _ = std::fs::write(&flag_path, b"");
    }
    unzip
}

/// 从 camoufox config.json 的 active_version 读取当前激活的版本子目录路径。
fn active_camoufox_bin(camo_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let cfg_path = camo_dir.join("config.json");
    let txt = std::fs::read_to_string(cfg_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let active = v.get("active_version")?.as_str()?;
    // active_version 形如 "browsers/official/152.0.4-beta.28-924f3109"
    let dir = camo_dir.join(active);
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// 读取配置（不含敏感字段明文）
/// 由配置构造响应（含 cron 校验）
fn config_response(
    cfg: &crate::infra::config::AppConfig,
    state: &AppState,
    logged_in: bool,
    error: Option<String>,
) -> ConfigResponse {
    let schedule = cfg.backup.schedule_cron.clone().unwrap_or_default();
    let valid = crate::domain::scheduler::validate_cron(&schedule).is_ok();
    ConfigResponse {
        backup_paths: cfg.backup.paths.clone(),
        target_folder: cfg.backup.target_folder.clone(),
        schedule_cron: schedule,
        schedule_cron_valid: valid,
        logged_in,
        login_bin_available: state.auth.bin_exists(),
        login_debug: cfg.kzwr.login_debug,
        error,
    }
}

async fn config_get(State(state): State<AppState>) -> Json<ConfigResponse> {
    let cfg_guard = state.config.lock().unwrap();
    let cfg = match cfg_guard.load() {
        Ok(c) => c,
        Err(e) => {
            return Json(ConfigResponse {
                backup_paths: Vec::new(),
                target_folder: state.target_folder.clone(),
                schedule_cron: String::new(),
                schedule_cron_valid: true,
                logged_in: false,
                login_bin_available: state.auth.bin_exists(),
                login_debug: false,
                error: Some(format!("{:#}", e)),
            })
        }
    };
    let logged_in = crate::infra::kzwr_auth::KzwrAuthService::has_credentials(&cfg);
    Json(config_response(&cfg, &state, logged_in, None))
}

/// 保存配置（备份路径、目标文件夹、定时 cron）
async fn config_save(
    State(state): State<AppState>,
    Json(body): Json<ConfigSaveRequest>,
) -> Json<ConfigResponse> {
    let cfg_guard = state.config.lock().unwrap();
    let mut cfg = match cfg_guard.load() {
        Ok(c) => c,
        Err(_) => crate::infra::config::AppConfig::default(),
    };
    cfg.backup.paths = body.backup_paths.clone();
    if let Some(folder) = body.target_folder {
        if !folder.trim().is_empty() {
            cfg.backup.target_folder = folder;
        }
    }
    // 定时 cron：校验合法性；空串视为关闭
    if let Some(cron) = body.schedule_cron {
        let cron = cron.trim().to_string();
        if let Err(e) = crate::domain::scheduler::validate_cron(&cron) {
            let resp = config_response(&cfg, &state, cfg.kzwr.username_enc.is_some(), Some(e.to_string()));
            return Json(resp);
        }
        if cron.is_empty() {
            cfg.backup.schedule_cron = None;
        } else {
            cfg.backup.schedule_cron = Some(cron);
        }
    }
    // 登录二进制 debug 日志开关
    if let Some(ld) = body.login_debug {
        cfg.kzwr.login_debug = ld;
    }
    match cfg_guard.save(&cfg) {
        Ok(_) => {
            let logged_in = cfg.kzwr.username_enc.is_some();
            Json(config_response(&cfg, &state, logged_in, None))
        }
        Err(e) => Json(config_response(&cfg, &state, false, Some(format!("{:#}", e)))),
    }
}

/// 获取当前登录用户信息（含存储容量，来自 get_member）
async fn user_info(State(state): State<AppState>) -> Json<UserInfoResponse> {
    // 1) 配置中记录的登录用户名
    let logged_in_username = state.auth.current_username();

    // 2) 从 kzwr API 获取用户信息与容量
    match state.kzwr_client.get_member().await {
        Ok(v) => {
            let data = v.get("data").cloned().unwrap_or_default();
            let num = |key: &str| {
                data.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
            };
            let opt_str = |key: &str| {
                data.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
            };
            Json(UserInfoResponse {
                email: opt_str("email").or_else(|| logged_in_username.clone()),
                name: opt_str("name"),
                avatar: opt_str("avatar"),
                plan: opt_str("plan"),
                total: num("total").max(num("capacity")),
                use_bytes: num("use"),
                percentage: opt_str("percentage"),
                logged_in_username,
                error: None,
            })
        }
        Err(e) => Json(UserInfoResponse {
            logged_in_username,
            error: Some(format!("获取用户信息失败: {e}")),
            ..Default::default()
        }),
    }
}

/// 触发备份：确保登录 + 遍历配置的多备份路径
async fn backup_run(State(state): State<AppState>) -> Json<BackupResponse> {
    Json(run_backup_now(&state).await)
}

/// 执行一次备份（可被 HTTP handler 与定时调度器复用）
///
/// 返回 BackupResponse（含 uploaded/deleted/orphan_removed/error）。
pub async fn run_backup_now(state: &AppState) -> BackupResponse {
    // 1) 确保已登录（token 过期自动重登）
    if let Err(e) = state.auth.ensure_login() {
        return BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            orphan_removed: 0,
            error: Some(format!("登录失败: {:#}", e)),
        };
    }

    // 2) 读取配置的备份路径（锁操作隔离在同步函数，避免跨 await）
    let (paths, target_folder, retention_cfg) = read_backup_config(state);

    if paths.is_empty() {
        return BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            orphan_removed: 0,
            error: Some("未配置备份路径".to_string()),
        };
    }

    // 3) 执行多路径备份
    //    保留策略：启用时构造 RetentionPolicy，备份完成后清理目标端孤儿文件
    let retention = if retention_cfg.enabled && retention_cfg.cleanup_unmanaged {
        let rt = crate::domain::retention::RetentionPolicy::new(
            state.target.clone(),
            &target_folder,
        );
        let rt = if retention_cfg.min_age_days > 0 {
            rt.with_min_age_secs(retention_cfg.min_age_days * 86400)
        } else {
            rt
        };
        Some(rt)
    } else {
        None
    };

    let job = BackupJob {
        job_id: state.job_id.clone(),
        source: Arc::new(crate::infra::source::local::LocalFsSource::new(&paths[0])),
        target: state.target.clone(),
        crypto: state.crypto.clone(),
        store: state.store.clone(),
        target_prefix: Some(target_folder),
        eventbus: Some(state.eventbus.clone()),
        retention,
    };
    match job.run_multi(&paths).await {
        Ok(summary) => BackupResponse {
            uploaded: summary.uploaded,
            uploaded_bytes: summary.uploaded_bytes,
            deleted: summary.deleted,
            unchanged: summary.unchanged,
            orphan_removed: summary.orphan_removed,
            error: None,
        },
        Err(e) => BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            orphan_removed: 0,
            error: Some(format!("{:#}", e)),
        },
    }
}

/// 读取备份配置（同步，锁在函数内释放）
fn read_backup_config(
    state: &AppState,
) -> (Vec<PathBuf>, String, crate::infra::config::RetentionConfig) {
    let cfg_guard = state.config.lock().unwrap();
    match cfg_guard.load() {
        Ok(cfg) => {
            let paths = cfg.backup.paths.iter().map(PathBuf::from).collect();
            let folder = cfg.backup.target_folder.clone();
            let retention = cfg.backup.retention.clone();
            (paths, folder, retention)
        }
        Err(_) => (
            Vec::new(),
            state.target_folder.clone(),
            crate::infra::config::RetentionConfig::default(),
        ),
    }
}

/// 列出配置的备份文件夹及其可恢复文件（从 SQLite 快照查询）
async fn restore_files(State(state): State<AppState>) -> Json<RestoreFilesResponse> {
    let paths = read_backup_config(&state).0;
    let mut folders = Vec::new();

    for (i, path) in paths.iter().enumerate() {
        // 多路径备份时，每个路径的 job_id = "{base}-{i}"
        let job_id = format!("{}-{}", state.job_id, i);
        let entries = state.store.load_snapshot(&job_id);

        let files = match entries {
            Ok(entries) => entries
                .iter()
                .map(|e| RestorableFile {
                    rel_path: e.rel_path.clone(),
                    size: e.size,
                    is_dir: e.is_dir,
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        folders.push(RestorableFolder {
            path: path.to_string_lossy().into_owned(),
            has_backup: !files.is_empty(),
            files,
        });
    }

    Json(RestoreFilesResponse { folders, error: None })
}

/// 触发恢复
async fn restore_run(
    State(state): State<AppState>,
    body: Option<axum::extract::Json<RestoreRequest>>,
) -> Json<RestoreResponse> {
    // 恢复前确保已登录（token 过期自动重登）
    if let Err(e) = state.auth.ensure_login() {
        return Json(RestoreResponse {
            restored: 0,
            restored_bytes: 0,
            error: Some(format!("登录失败: {:#}", e)),
        });
    }

    // 恢复目标根：优先用前端传入的 source_path（备份源路径，恢复到原位置），否则用默认目录
    let (files, restore_root) = match body {
        Some(Json(req)) => (
            req.files.unwrap_or_default(),
            req.source_path
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| state.default_restore_dir.to_string_lossy().into_owned()),
        ),
        None => (
            Vec::new(),
            state.default_restore_dir.to_string_lossy().into_owned(),
        ),
    };

    let job = RestoreJob {
        target: state.target.clone(),
        crypto: state.crypto.clone(),
        target_prefix: Some(state.target_folder.clone()),
        eventbus: Some(state.eventbus.clone()),
    };
    match job.run(&files, std::path::Path::new(&restore_root)).await {
        Ok(summary) => Json(RestoreResponse {
            restored: summary.restored,
            restored_bytes: summary.restored_bytes,
            error: None,
        }),
        Err(e) => Json(RestoreResponse {
            restored: 0,
            restored_bytes: 0,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 构建应用路由（不含 /api 前缀，由 main.rs nest("/api") 统一加前缀）
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_handler))
        .route("/auth/login", post(auth_login))
        .route("/user/info", get(user_info))
        .route("/config", get(config_get).post(config_save))
        .route("/backup/run", post(backup_run))
        .route("/restore/files", get(restore_files))
        .route("/restore/run", post(restore_run))
        .route("/login-env/status", get(login_env_status))
        .route("/login-env/init", post(login_env_init))
        .route("/login-env/cancel", post(login_env_cancel))
        .with_state(state)
}
