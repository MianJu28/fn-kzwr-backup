#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
kzwr.com 全自动登录脚本 (Camoufox 版)
================================================================
与 kzwr_login_turnstile.py 等价，但使用 **Camoufox** 替代 CloakBrowser。

为什么用 Camoufox:
  - CloakBrowser 0.5.8 自带的那份 Linux Chromium 146 二进制有打包缺陷
    (spawn chrome_crashpad_handler 参数缺失 -> 主进程 FATAL/SIGTRAP)，
    在飞牛(fnOS)等 Linux 服务器上不可用，且改名/feature flag 均无效。
  - Camoufox 基于源码级反检测 Firefox，官方提供 Linux 构建，
    crashpad 流程正常，在服务器上可稳定运行。
  - Camoufox 也是 Playwright API，指纹/Canvas/WebGL/字体/时区伪装更强，
    对 Cloudflare Turnstile 判定有专门优化。

依赖安装:
    uv pip install camoufox[geoip]
    python -m camoufox fetch   # 下载 Camoufox 浏览器二进制(约 ~150MB)
    # 若提示缺 Firefox 运行时库，在 Debian/Ubuntu 上:
    #   sudo apt-get install -y libgtk-3-0 libdbus-glib-1-2 libasound2
    #   libx11-xcb1 libxtst6 libxrandr2 libxcomposite1 libxdamage1 libxfixes3

用法:
    python kzwr_login_camoufox.py <邮箱> <密码>
    python kzwr_login_camoufox.py your_email@example.com your_password

作为库调用:
    from kzwr_login_camoufox import login
    res = login("your_email@example.com", "your_password")
    if res:
        print("token:", res.access_token)
"""

import json
import os
import sys
import time

BASE_URL = "https://www.kzwr.com"


# ============ 与浏览器类型无关的辅助函数 (操作 page 对象) ============

def set_angular_value(page, selector, value):
    """以 Angular 感知的方式给 input 赋值（触发 ngModel 更新）。"""
    page.wait_for_selector(selector, timeout=10000)
    page.evaluate(
        """([sel, val]) => {
            const el = document.querySelector(sel);
            if (!el) return;
            const setter = Object.getOwnPropertyDescriptor(
                window.HTMLInputElement.prototype, 'value').set;
            setter.call(el, val);
            el.dispatchEvent(new Event('input', {bubbles: true}));
            el.dispatchEvent(new Event('change', {bubbles: true}));
            el.blur();
        }""",
        [selector, value],
    )


def ensure_turnstile_sdk(page, timeout=15):
    """确保 challenges.cloudflare.com/api.js 已加载，返回是否成功。"""
    loaded = page.evaluate(
        "() => typeof window.turnstile !== 'undefined' || "
        "typeof window.turnstile?.render === 'function'"
    )
    if loaded:
        return True
    page.evaluate(
        """() => {
            if (document.querySelector('script[src*="challenges.cloudflare.com"]')) return;
            const s = document.createElement('script');
            s.src = 'https://challenges.cloudflare.com/turnstile/api.js';
            document.head.appendChild(s);
        }"""
    )
    try:
        page.wait_for_function(
            "() => typeof window.turnstile !== 'undefined' && "
            "typeof window.turnstile.render === 'function'",
            timeout=timeout * 1000,
        )
        return True
    except Exception:
        return False


def trigger_turnstile(page):
    """点击 .reCAPTCHA-btn，触发 reCAPTCHA v3 -> Turnstile fallback 流程。"""
    try:
        btn = page.locator(".reCAPTCHA-btn")
        btn.wait_for(state="visible", timeout=10000)
        btn.click(timeout=8000)
        print("[*] 已点击 .reCAPTCHA-btn", flush=True)
    except Exception as e:
        print(f"[!] 点击 .reCAPTCHA-btn 失败: {e}", flush=True)
        # 兜底：手动加载 Turnstile SDK
        if not ensure_turnstile_sdk(page):
            print("[!] Turnstile SDK 加载失败", flush=True)


def find_turnstile_box(page):
    """定位 Turnstile 验证框（若走 iframe 方案）。"""
    return page.frame_locator("iframe[src*='challenges.cloudflare.com']").first


def try_click_checkbox_in_frame(page):
    """尝试在 Turnstile iframe 内点击验证复选框。"""
    try:
        frame = find_turnstile_box(page)
        box = frame.locator("body")
        box.wait_for(timeout=5000)
        box.click(timeout=3000)
        return True
    except Exception:
        return False


def wait_captcha_success(page, timeout=90):
    """等待验证成功。优先检测 label 文案变为"成功!"或 #robot-check 被勾选。"""
    end = time.time() + timeout
    while time.time() < end:
        try:
            status = page.evaluate(
                """() => {
                    const labels = [...document.querySelectorAll('label, .label, span, p')]
                        .map(e => (e.textContent||'').replace(/\\s+/g,' ').trim())
                        .filter(t => t.includes('成功') || t === 'Success');
                    const robot = document.querySelector('#robot-check');
                    const rc = robot ? robot.getAttribute('aria-checked') : null;
                    return {
                        labels: labels.slice(0, 3),
                        robotChecked: rc === 'true',
                        robotClass: robot ? robot.className : ''
                    };
                }"""
            )
            if status.get("robotChecked"):
                print("[+] #robot-check 已勾选", flush=True)
                return True
            if status.get("labels"):
                print("[+] 检测到成功文案: %s" % status["labels"], flush=True)
                return True
        except Exception:
            pass
        # 兜底：尝试直接点击 iframe 内复选框
        try_click_checkbox_in_frame(page)
        time.sleep(2)
    return False


class LoginResult:
    def __init__(self, success, access_token="", email="", session_path="", message=""):
        self.success = success
        self.access_token = access_token
        self.email = email
        self.session_path = session_path
        self.message = message

    def __bool__(self):
        return self.success

    def __repr__(self):
        return (
            f"LoginResult(success={self.success}, email={self.email!r}, "
            f"token_len={len(self.access_token)}, "
            f"session_path={self.session_path!r})"
        )


def login(email, password, session_path="session.json", headless="virtual", humanize=True):
    """对外暴露的登录接口，基于 Camoufox。

    参数:
        email (str):           账号/邮箱
        password (str):        密码
        session_path (str):    会话 token 保存路径，默认当前目录 session.json
        headless (bool|str):   无头模式。True=纯无头; "virtual"=Xvfb虚拟显示(默认,
                              更接近真实浏览器, 提升 reCAPTCHA/Turnstile 通过率);
                              False=有头(需显示服务器)
        humanize (bool):       Camoufox 拟人化移动(鼠标轨迹/输入速度) (默认 True)

    返回:
        LoginResult: 含 success / access_token / email / session_path / message

    示例:
        from kzwr_login_camoufox import login
        res = login("user@example.com", "pass")
        if res:
            print("token:", res.access_token)
    """
    # 延迟导入，保证 env 先设好、且未装时也能给友好报错
    try:
        from camoufox.sync_api import Camoufox
    except ImportError:
        print(
            "[!] 未安装 camoufox，请先执行: uv pip install camoufox[geoip] 然后 python -m camoufox fetch",
            flush=True,
        )
        return LoginResult(False, email=email, message="camoufox 未安装")

    print("[*] 启动 Camoufox (headless=%s, humanize=%s)..." % (headless, humanize), flush=True)

    # Camoufox 反检测参数：
    #   - os: 伪装操作系统
    #   - humanize: 拟人化鼠标/输入
    #   - 其余(geoip 定位、locales 语言)可再按需添加
    try:
        with Camoufox(headless=headless, humanize=humanize) as browser:
            page = browser.new_page()
            page.set_default_timeout(30000)
            return _do_login(page, email, password, session_path)
    except Exception as e:
        print(f"[!] 浏览器启动/运行异常: {e}", flush=True)
        return LoginResult(False, email=email, message=f"浏览器异常: {e}")


def _do_login(page, email, password, session_path):
    """在已启动的 page 上执行登录流程（与浏览器类型无关）。"""
    try:
        print("[*] 打开首页并点击登录...", flush=True)
        page.goto(BASE_URL, wait_until="domcontentloaded", timeout=30000)
        # 等待登录入口（Sign in / Log in / 登录 / 登入）渲染出来，避免 networkidle
        # 超时导致页面未就绪就 evaluate 找不到按钮。
        clicked = ""
        end = time.time() + 25
        while time.time() < end:
            try:
                clicked = page.evaluate(
                    """() => {
                        const norm = s => (s||'').replace(/\\s+/g,' ').trim();
                        const exact = ['Sign in', 'Log in', '登录', '登入'];
                        const all = [...document.querySelectorAll('a,button')];
                        for (const t of exact) {
                            const el = all.find(e => norm(e.textContent) === t);
                            if (el) { el.click(); return 'clicked:'+t; }
                        }
                        for (const t of exact) {
                            const el = all.find(e => norm(e.textContent).includes(t) && norm(e.textContent).length <= 30);
                            if (el) { el.click(); return 'clicked-substr:'+t; }
                        }
                        return '';
                    }"""
                )
                if clicked:
                    break
            except Exception:
                pass
            time.sleep(1)
        print(f"[*] 登录入口点击: {clicked or 'no-login-entry'}", flush=True)
        if not clicked:
            raise RuntimeError("未找到登录入口(Sign in/Log in/登录/登入)")
        page.wait_for_selector(".login-card input[type=text]", timeout=20000)

        print("[*] 填入账号密码...", flush=True)
        set_angular_value(page, ".login-card input[type=text]", email)
        set_angular_value(page, ".login-card input[type=password]", password)

        print("[*] 触发验证 (.reCAPTCHA-btn 点击)...", flush=True)
        trigger_turnstile(page)

        print("[*] 等待验证码验证成功 (reCAPTCHA v3 -> Turnstile fallback)...", flush=True)
        if not wait_captcha_success(page, timeout=90):
            print("[!] 90s 内验证未成功。请检查上方诊断信息。", flush=True)
            return LoginResult(False, email=email, message="验证码验证未成功")

        print("[+] 验证码验证成功!", flush=True)

        print("[*] 真实点击登录按钮 (Angular 自动用 preVerificationToken 提交)...", flush=True)
        try:
            login_btn = page.locator(".login-card button.btn-login")
            login_btn.wait_for(state="visible", timeout=8000)
            login_btn.click(timeout=8000)
            print("[*] 登录按钮已点击", flush=True)
        except Exception as e:
            print(f"[!] 点击登录按钮失败: {e}", flush=True)
            return LoginResult(False, email=email, message=f"点击登录按钮失败: {e}")

        print("[*] 等待 access-token 写入...", flush=True)
        end = time.time() + 25
        access_token = ""
        while time.time() < end:
            access_token = page.evaluate("() => localStorage.getItem('access-token') || ''")
            if access_token:
                break
            time.sleep(1)

        if access_token:
            print("[+] 登录成功! access-token =", access_token[:40], "...", flush=True)
            with open(session_path, "w", encoding="utf-8") as f:
                json.dump(
                    {"access_token": access_token, "email": email, "via": "camoufox"},
                    f,
                    ensure_ascii=False,
                    indent=2,
                )
            print(f"[*] 会话已保存到 {session_path}", flush=True)
            return LoginResult(True, access_token=access_token, email=email, session_path=session_path, message="登录成功")
        else:
            print("[!] 未拿到 access-token。登录可能失败。", flush=True)
            return LoginResult(False, email=email, message="未拿到 access-token")
    except Exception as e:
        print(f"[!] 登录流程异常: {e}", flush=True)
        return LoginResult(False, email=email, message=f"登录流程异常: {e}")


def main():
    if len(sys.argv) >= 3:
        email, password = sys.argv[1], sys.argv[2]
    else:
        email = input("邮箱/用户名: ").strip()
        password = input("密码: ").strip()

    res = login(email, password)
    return 0 if res.success else 1


if __name__ == "__main__":
    sys.exit(main())
