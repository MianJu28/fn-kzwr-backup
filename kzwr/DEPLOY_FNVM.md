# Kzwr 自动登录 —— 飞牛（fnOS / Linux）部署全流程

> 适用目标：在 **飞牛 fnOS**（Linux x86_64 服务器）上部署并运行 kzwr 自动登录，替代在 Linux 上不可用的 CloakBrowser。
>
> 已验证环境：`192.168.100.10`，系统 Python 3.11.2，项目位于 `/vol1/1000/programs/kzwr`，登录账号 `mianju028@163.com`。

---

## 1. 为什么需要这篇文档

**CloakBrowser 0.5.8 自带的那份 Linux Chromium 146 二进制有打包缺陷**，在飞牛上启动即 `SIGTRAP`（退出码 133），且无解：

- 主进程 spawn `chrome_crashpad_handler` 时**参数缺失**（`--database is required` / `--initial-client-fd is required`），子进程立刻退出。
- 主进程把 crashpad 失败当 **FATAL** → `abort()`；改名 crashpad_handler 又会让主进程 `posix_spawn` 直接 FATAL（硬依赖）。
- `--disable-crash-reporter` / `--no-crashpad` / `--disable-features=Crashpad` 全部无效。

因此改用 **Camoufox**（源码级反检测 Firefox，Playwright API，Linux 可用，反检测更强）。

---

## 2. 文件结构（飞牛项目目录 `/vol1/1000/programs/kzwr`）

```
/vol1/1000/programs/kzwr/
├── kzwr_login_camoufox.py   # ✅ Camoufox 版自动登录脚本（本方案核心）
├── kzwr_login_turnstile.py  # CloakBrowser 版（Linux 不可用，仅参考）
├── kzwr_api.py              # 文件管理 API 客户端
├── session.json             # 登录成功后生成的 token
├── requirements.txt
└── .cache/                  # Camoufox 浏览器与 addon 缓存（XDG_CACHE_HOME 指向这里）
    └── camoufox/
        ├── browsers/official/152.0.4-beta.28-*/   # 浏览器二进制（约 663MB）
        └── addons/UBO/                            # uBlock Origin（含 manifest.json）
```

---

## 3. 完整部署步骤

### 3.1 准备工作：远程登录飞牛

飞牛 `admin` 用户对 `/home/admin` 无写权限（root:root 755），且很多依赖目录默认不可写，**务必先用 sudo 修复**：

```bash
# 手动 SSH 登录后执行
echo '123' | sudo -S chown -R admin:Users /home/admin
```

> 说明：`admin` 属于 `Administrators` 组，`sudo` 可用，密码同 SSH 密码。若不修复，Camoufox 启动会因 fontconfig/profile 缓存不可写而**挂起**。

### 3.2 安装 Python 依赖（关键：用国内镜像加速）

飞牛上 `pypi.org` 直连极慢（~20 kB/s），**必须换清华镜像**（实测 30~450 MB/s）：

```bash
cd /vol1/1000/programs/kzwr

# 本项目 .venv 使用系统 python3（3.11.2），无 uv，用 pip
.venv/bin/python -m pip install --timeout 60 -i https://pypi.tuna.tsinghua.edu.cn/simple \
    camoufox[geoip] playwright==1.60.0 requests
```

> **版本注意**：`camoufox 0.5.5` 依赖 `playwright<1.61`，会强制把已有 `playwright 1.62` 降为 `1.60.0`。若与 CloakBrowser 同装会冲突，建议 **卸载 CloakBrowser**：
> ```bash
> .venv/bin/python -m pip uninstall -y cloakbrowser
> ```

> 若代理方案（如 `192.168.0.105:7968`）也可用，但延迟高（单请求 ~15s，易超时）；清华镜像更快，优先镜像。

### 3.3 下载 Camoufox 浏览器二进制

GitHub Releases 直连飞牛很快（实测 15.6 MB/s，663 MB），**无需代理**：

```bash
cd /vol1/1000/programs/kzwr
mkdir -p .cache
# 必须指定 XDG_CACHE_HOME 到可写目录（默认 ~/.cache 不可写）
export XDG_CACHE_HOME=/vol1/1000/programs/kzwr/.cache
.venv/bin/python -m camoufox fetch
```

安装位置：`/vol1/1000/programs/kzwr/.cache/camoufox/browsers/official/<version>-<hash>/`

### 3.4 补装 uBlock Origin addon（必须）

Camoufox 默认要加载 **uBlock Origin**（从 `addons.mozilla.org` 下载），但该域名在飞牛上返回 **HTTP 451（地区封锁）**，必须手动从 GitHub 下载并解压：

```bash
export XDG_CACHE_HOME=/vol1/1000/programs/kzwr/.cache
ADDONS=$XDG_CACHE_HOME/camoufox/addons
UBO_DIR=$ADDONS/UBO
UBO_URL="https://github.com/gorhill/uBlock/releases/download/1.73.0/uBlock0_1.73.0.firefox.signed.xpi"

# GitHub release 资产直连较慢（~25 kB/s），用 setsid 脱离会话后台下载，避免 SSH 断开中断
setsid nohup curl -s -L -o /tmp/ubo.xpi "$UBO_URL" >/tmp/ubo_dl.log 2>&1 </dev/null &
# 轮询直到 /tmp/ubo.xpi 大小稳定（约 4.7 MB）后：
rm -rf "$UBO_DIR" && mkdir -p "$UBO_DIR"
cd "$UBO_DIR" && unzip -o /tmp/ubo.xpi
ls "$UBO_DIR/manifest.json"   # 确认存在
```

> 若 `manifest.json` 缺失，Camoufox 启动会报 `InvalidAddonPath: manifest.json is missing`。

### 3.5 安装 Xvfb（关键：headless="virtual" 需要）

纯 `headless=True` 在飞牛上 **Turnstile 验证 90s 内无法通过**（被判定自动化）。改用 `headless="virtual"`（Xvfb 虚拟显示，更接近真实浏览器）后验证通过。

```bash
echo '123' | sudo -S apt-get update
echo '123' | sudo -S apt-get install -y xvfb
which Xvfb xvfb-run   # 确认安装
```

---

## 4. 运行登录

```bash
cd /vol1/1000/programs/kzwr
export XDG_CACHE_HOME=/vol1/1000/programs/kzwr/.cache

.venv/bin/python kzwr_login_camoufox.py <邮箱> <密码>
# 例：
.venv/bin/python kzwr_login_camoufox.py mianju028@163.com mj281233
```

成功标志：

```
[*] 登录入口点击: clicked:Sign in
[*] 已点击 .reCAPTCHA-btn
[+] 检测到成功文案: ['成功!']
[+] 验证码验证成功!
[+] 登录成功! access-token = WENBeGphMlRhbD... 
[*] 会话已保存到 session.json
```

`session.json` 内容：

```json
{
  "access_token": "<access-token>",
  "email": "mianju028@163.com",
  "via": "camoufox"
}
```

### 脚本关键参数（`kzwr_login_camoufox.py`）

- `headless="virtual"`：默认值，Xvfb 虚拟显示，验证码通过率最高。
- `headless=True`：纯无头（更快，但验证码可能失败）。
- `headless=False`：有头模式（需显示服务器）。

---

## 5. 依赖清单与 requirements.txt

`requirements.txt`（飞牛可用的最终版本）：

```
camoufox[geoip]
playwright==1.60.0     # camoufox 需要 <1.61
requests==2.32.4
```

> 不再需要 `cloakbrowser`（Linux 不可用）；如已装请卸载，避免与 camoufox 的 playwright 版本冲突。

---

## 6. 排错速查表

| 现象 | 原因 | 解决 |
|------|------|------|
| 启动即 `SIGTRAP`/退出码 133 | CloakBrowser Chromium 打包缺陷 | 改用 Camoufox（本文方案） |
| pip 安装超慢/超时 | pypi.org 直连慢 | 清华镜像 + `--timeout 60` |
| Camoufox 启动挂起（无输出） | `/home/admin` 不可写 | `sudo chown -R admin:Users /home/admin` |
| `InvalidAddonPath manifest.json is missing` | uBlock 未安装 | 手动下载 xpi 解压到 `addons/UBO` |
| `CannotFindXvfb` | 缺 Xvfb | `apt-get install -y xvfb` |
| 验证码 90s 内不通过 | 纯 headless 被判定自动化 | 改用 `headless="virtual"` |
| 找不到登录按钮 `no-login-entry` | `networkidle` 超时页面未就绪 | 已改 `domcontentloaded` + 轮询等待（无需手动处理） |
| 页面打不开/超时 | 慢网络 | 确保 kzwr.com 可达；必要时调整脚本 timeout |

---

## 7. 远程自动化执行（可选）

若要从另一台机器自动 SSH 部署，可用 **Windows OpenSSH 的 `SSH_ASKPASS`** 非交互传密码：

```bash
export DISPLAY=:0
export SSH_ASKPASS_REQUIRE=force
export SSH_ASKPASS="/path/to/askpass.sh"   # 内容: echo '<密码>'
ssh -o StrictHostKeyChecking=no admin@192.168.100.10 '<远程命令>'
```

> 注意：外层若被 PowerShell 包裹，复杂引号（`>`、`2>&1`、`'`）会被误解析，**把操作封装进 `.sh` 脚本再用 bash 执行**最稳。

---

## 8. 备注

- **GitHub 直连快**（15.6 MB/s），**pypi.org 慢**，**addons.mozilla.org 被 451 封锁** —— 这是飞牛网络的关键特征。
- 每次运行前都要 `export XDG_CACHE_HOME=/vol1/1000/programs/kzwr/.cache`（否则会重新下载 663MB 浏览器，或写不进去）。
- 项目仅供学习与技术研究使用，请遵守目标网站的 robots.txt 与服务条款。
