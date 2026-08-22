<script>
  import DashboardPage from './views/DashboardPage.svelte';
  import BackupPage from './views/BackupPage.svelte';
  import RestorePage from './views/RestorePage.svelte';
  import SettingsPage from './views/SettingsPage.svelte';

  // 全局状态
  let health = '检查中...';
  let error = null;
  let busy = false;

  // 当前页面（导航切换）
  let currentPage = 'dashboard';

  // kzwr 登录
  let loggedIn = false;

  // 备份配置
  let backupPaths = [];
  let targetFolder = 'fn-backup';
  let scheduleCron = '';
  let scheduleCronValid = true;
  // 登录二进制 debug 日志开关
  let loginDebug = false;

  // 备份/恢复
  let backupResult = null;
  // 可恢复文件（从 SQLite 查询）
  let restoreFolders = [];
  // 实时任务状态（WebSocket 推送）
  let liveStatus = null; // { kind, status, current_file, done, total }
  let wsConnected = false;
  // 用户信息（含存储容量）
  let userInfo = null;
  let userInfoError = null;

  // 登录环境（Camoufox 浏览器 + uBlock）初始化
  let loginEnv = null;      // { ready, state, progress, message, mirror, mirrors, ... }
  let loginEnvChecking = false;
  let initRunning = false;
  let initMessage = '';
  let initProgress = null;  // 0-100
  let selectedMirror = '';
  let pollTimer = null;

  // 检测登录环境状态（前端页面打开时）
  async function checkLoginEnv() {
    loginEnvChecking = true;
    try {
      const res = await fetch('/api/login-env/status');
      loginEnv = await res.json();
      if (!selectedMirror && loginEnv.mirrors && loginEnv.mirrors.length) {
        selectedMirror = loginEnv.mirror || loginEnv.mirrors[0];
      }
      initRunning = loginEnv.running;
      initProgress = loginEnv.progress >= 0 ? loginEnv.progress : null;
      initMessage = loginEnv.message || '';
      // 若在初始化中，启动轮询
      if (initRunning) startPolling();
    } catch (e) {
      loginEnv = { ready: true, error: e.message }; // 查询失败不打扰用户
    } finally {
      loginEnvChecking = false;
    }
  }

  // 轮询状态（初始化进行中，实时刷新进度条）
  function startPolling() {
    stopPolling();
    pollTimer = setInterval(async () => {
      try {
        const res = await fetch('/api/login-env/status');
        const st = await res.json();
        loginEnv = st;
        initRunning = st.running;
        initProgress = st.progress >= 0 ? st.progress : null;
        initMessage = st.message || '';
        if (!st.running) {
          stopPolling();
          // 完成/失败/取消后，延迟关闭横幅（失败/取消保留提示）
          if (st.state === 'error' || st.state === 'cancelled') {
            // 保留显示错误信息，用户可重试
          }
        }
      } catch (e) {}
    }, 1000);
  }

  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  // 初始化登录环境（选择镜像），完成后轮询进度
  async function runLoginEnvInit() {
    initRunning = true;
    initProgress = 0;
    initMessage = '正在启动初始化...';
    try {
      const res = await fetch('/api/login-env/init', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mirror: selectedMirror }),
      });
      const st = await res.json();
      loginEnv = st;
      startPolling();
    } catch (e) {
      initRunning = false;
      initMessage = `初始化启动失败: ${e.message}`;
    }
  }

  // 取消初始化
  async function cancelLoginEnvInit() {
    try {
      await fetch('/api/login-env/cancel', { method: 'POST' });
      stopPolling();
      initRunning = false;
      await checkLoginEnv();
    } catch (e) {
      initMessage = `取消失败: ${e.message}`;
    }
  }


  const navItems = [
    { id: 'dashboard', label: '📊 概览' },
    { id: 'backup', label: '⬆️ 备份' },
    { id: 'restore', label: '⬇️ 恢复' },
    { id: 'settings', label: '⚙️ 设置' },
  ];

  // 连接 WebSocket 实时状态流
  function connectWS() {
    try {
      const proto = location.protocol === 'https:' ? 'wss' : 'ws';
      const ws = new WebSocket(`${proto}://${location.host}/api/ws`);
      ws.onopen = () => {
        wsConnected = true;
      };
      ws.onmessage = (evt) => {
        try {
          const data = JSON.parse(evt.data);
          if (data.type === 'event') {
            liveStatus = {
              kind: data.kind,
              status: data.status,
              current_file: data.current_file,
              done: data.done,
              total: data.total,
              message: data.message,
            };
          }
        } catch (e) {}
      };
      ws.onclose = () => {
        wsConnected = false;
        // 3 秒后重连
        setTimeout(connectWS, 3000);
      };
      ws.onerror = () => {
        ws.close();
      };
    } catch (e) {}
  }

  async function checkHealth() {
    try {
      const res = await fetch('/api/health');
      const data = await res.json();
      health = `服务正常 (v${data.version})`;
    } catch (e) {
      health = `服务异常: ${e.message}`;
    }
  }

  async function loadConfig() {
    try {
      const res = await fetch('/api/config');
      const data = await res.json();
      backupPaths = data.backup_paths || [];
      targetFolder = data.target_folder || 'fn-backup';
      scheduleCron = data.schedule_cron || '';
      scheduleCronValid = data.schedule_cron_valid !== false;
      loggedIn = data.logged_in;
      loginDebug = !!data.login_debug;
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    }
  }

  // 加载用户信息与存储容量
  async function loadUserInfo() {
    try {
      const res = await fetch('/api/user/info');
      const data = await res.json();
      userInfo = data;
      userInfoError = data.error || null;
    } catch (e) {
      userInfoError = e.message;
    }
  }

  // 加载可恢复文件列表（从 SQLite 快照查询）
  async function loadRestoreFiles() {
    try {
      const res = await fetch('/api/restore/files');
      const data = await res.json();
      restoreFolders = data.folders || [];
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    }
  }

  // kzwr 登录（供 SettingsPage 调用，返回消息字符串）
  async function handleLogin(username, password) {
    busy = true;
    error = null;
    try {
      const res = await fetch('/api/auth/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      });
      const data = await res.json();
      if (data.success) {
        loggedIn = true;
        loadUserInfo();
        return `已登录: ${data.username}`;
      }
      return `登录失败: ${data.error}`;
    } catch (e) {
      error = e.message;
      return `登录失败: ${e.message}`;
    } finally {
      busy = false;
    }
  }

  // 切换登录二进制 debug 日志开关（保存到配置）
  async function toggleLoginDebug(next) {
    loginDebug = next;
    try {
      const res = await fetch('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          backup_paths: backupPaths,
          target_folder: targetFolder,
          schedule_cron: scheduleCron,
          login_debug: next,
        }),
      });
      const data = await res.json();
      loginDebug = !!data.login_debug;
    } catch (e) {
      error = e.message;
    }
  }

  // 保存配置（供 BackupPage 调用）
  async function handleSaveConfig() {
    busy = true;
    error = null;
    try {
      const res = await fetch('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          backup_paths: backupPaths,
          target_folder: targetFolder,
          schedule_cron: scheduleCron.trim(),
        }),
      });
      const data = await res.json();
      scheduleCronValid = data.schedule_cron_valid !== false;
      if (data.error) error = data.error;
      else if (!scheduleCronValid) error = 'cron 表达式无效，已拒绝保存';
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  // 立即备份（供 BackupPage 调用）
  async function handleRunBackup() {
    busy = true;
    error = null;
    backupResult = null;
    try {
      const res = await fetch('/api/backup/run', { method: 'POST' });
      const data = await res.json();
      backupResult = data;
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  // 恢复请求（供 RestorePage 调用，返回 {restored, restored_bytes, error}）
  async function handleRestore(files, sourcePath) {
    const body = { files, source_path: sourcePath };
    const res = await fetch('/api/restore/run', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    return res.json();
  }

  checkHealth();
  loadConfig();
  loadRestoreFiles();
  loadUserInfo();
  connectWS();
  checkLoginEnv();
</script>

<main>
  <header>
    <h1>🛡️ fnos 增量加密备份</h1>
    <p class="health" class:ok={health.startsWith('服务正常')}>{health}</p>
  </header>

  <!-- 登录环境初始化横幅 -->
  {#if !loginEnvChecking && loginEnv && !loginEnv.ready && loginEnv.state !== 'cancelled'}
    <div class="login-env-banner">
      <div class="login-env-head">
        <strong>⚠️ 登录环境未就绪</strong>
        <span class="login-env-sub">
          {#if !loginEnv.camoufox_browser}
            Camoufox 浏览器尚未下载（约 600MB，仅首次需要）
          {:else}
            uBlock addon 尚未就绪
          {/if}
        </span>
      </div>

      {#if !initRunning}
        <div class="login-env-start">
          <div class="login-env-row">
            <label class="login-env-label">国内镜像</label>
            <select class="login-env-select" bind:value={selectedMirror}>
              {#each (loginEnv.mirrors || []) as m}
                <option value={m}>{m}</option>
              {/each}
            </select>
          </div>
          <button class="btn" on:click={runLoginEnvInit} disabled={loginEnvChecking}>
            初始化登录环境
          </button>
        </div>
      {:else}
        <div class="login-env-progress">
          <div class="progress-track">
            <div class="progress-fill" style="width:{Math.min(initProgress ?? 0, 100)}%"></div>
          </div>
          <span class="login-env-pct">{Math.min(initProgress ?? 0, 100)}%</span>
          <p class="login-env-msg">{initMessage || '正在准备，请耐心等待…'}</p>
          <button class="btn btn-cancel" on:click={cancelLoginEnvInit}>取消初始化</button>
        </div>
      {/if}

      {#if !initRunning && loginEnv.state === 'error'}
        <p class="login-env-err">初始化失败：{initMessage || loginEnv.message}</p>
        <button class="btn" on:click={runLoginEnvInit}>重试</button>
      {/if}
    </div>
  {/if}

  <!-- 顶部导航栏 -->
  <nav class="top-nav">
    {#each navItems as item}
      <button
        class="nav-item"
        class:active={currentPage === item.id}
        on:click={() => (currentPage = item.id)}
      >
        {item.label}
      </button>
    {/each}
  </nav>

  {#if error}
    <div class="error">⚠️ {error}</div>
  {/if}

  <!-- 按功能切换页面 -->
  {#if currentPage === 'dashboard'}
    <DashboardPage
      {liveStatus}
      {backupPaths}
      {targetFolder}
      {loggedIn}
      {restoreFolders}
      {scheduleCron}
      {scheduleCronValid}
      {userInfo}
      {userInfoError}
    />
  {:else if currentPage === 'backup'}
    <BackupPage
      bind:backupPaths
      bind:targetFolder
      bind:scheduleCron
      bind:scheduleCronValid
      {busy}
      {backupResult}
      onSave={handleSaveConfig}
      onRunBackup={handleRunBackup}
    />
  {:else if currentPage === 'restore'}
    <RestorePage {restoreFolders} {busy} onRestore={handleRestore} />
  {:else if currentPage === 'settings'}
    <SettingsPage
      {loggedIn}
      {busy}
      {userInfo}
      onLogin={handleLogin}
      {loginDebug}
      onToggleLoginDebug={toggleLoginDebug}
    />
  {/if}

  <footer>fnos-backup · age 加密 · kzwr 增量备份</footer>
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: system-ui, -apple-system, sans-serif;
    background: #f5f6fa;
    color: #1f2d3d;
  }
  main {
    max-width: 720px;
    margin: 0 auto;
    padding: 24px 16px 40px;
  }
  header { padding: 24px 0 12px; border-bottom: 1px solid #e0e4ea; }
  h1 { margin: 0; font-size: 24px; }
  .health { color: #5a6a7a; }
  .health.ok { color: #22a06b; }

  /* 顶部导航栏 */
  .top-nav {
    display: flex;
    gap: 4px;
    padding: 12px 0;
    border-bottom: 1px solid #e0e4ea;
    position: sticky;
    top: 0;
    background: #f5f6fa;
    z-index: 10;
  }
  .nav-item {
    flex: 1;
    background: transparent;
    color: #5a6a7a;
    border: none;
    border-radius: 8px;
    padding: 10px 12px;
    margin: 0;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.2s, color 0.2s;
  }
  .nav-item:hover { background: #eef1f6; color: #1f2d3d; }
  .nav-item.active {
    background: #2563eb;
    color: #fff;
  }

  .error { background: #fef2f2; color: #b91c1c; padding: 12px; border-radius: 6px; margin-top: 12px; }
  /* 全局 section 间距（统一卡片之间的留白） */
  :global(main > section) {
    margin-top: 20px;
  }
  footer { text-align: center; color: #8a94a6; font-size: 13px; margin-top: 28px; }
  .login-env-banner {
    margin: 12px 0;
    padding: 14px 16px;
    border: 1px solid #ffd591;
    border-left: 4px solid #ff7a00;
    background: #fff7e6;
    border-radius: 8px;
    font-size: 14px;
  }
  .login-env-head { display: flex; align-items: baseline; gap: 10px; margin-bottom: 10px; }
  .login-env-sub { color: #d46b08; font-size: 13px; }
  .login-env-progress { margin-top: 4px; }
  .progress-track { height: 10px; background: #f0e6d2; border-radius: 5px; overflow: hidden; margin-bottom: 6px; }
  .progress-fill { height: 100%; background: #ff7a00; transition: width 0.3s ease; }
  .login-env-pct { font-size: 13px; color: #d46b08; font-weight: 600; }
  .login-env-msg { margin: 6px 0 0; color: #666; font-size: 13px; word-break: break-all; }
  .login-env-err { margin: 6px 0 0; color: #cf1322; font-size: 13px; }
  .login-env-start { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; }
  .login-env-row { display: flex; align-items: center; gap: 8px; }
  .login-env-label { font-size: 13px; color: #666; }
  .login-env-select { padding: 6px 10px; border: 1px solid #d9d9d9; border-radius: 6px; font-size: 14px; background: #fff; color: #333; }
  .btn { padding: 8px 16px; border: none; border-radius: 6px; background: #ff7a00; color: #fff; font-size: 14px; cursor: pointer; }
  .btn:disabled { opacity: 0.6; cursor: not-allowed; }
  .btn-cancel { margin-top: 8px; background: #f0f0f0; color: #555; border: 1px solid #d9d9d9; }
</style>
