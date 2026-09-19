<script>
  import DashboardPage from './views/DashboardPage.svelte';
  import BackupPage from './views/BackupPage.svelte';
  import RestorePage from './views/RestorePage.svelte';
  import SettingsPage from './views/SettingsPage.svelte';
  import LiveStatus from './components/LiveStatus.svelte';
  import AlertBanner from './components/AlertBanner.svelte';

  // 全局状态
  let health = '检查中...';
  let error = null;
  let busy = false;

  // 当前页面（导航切换）
  let currentPage = 'dashboard';

  // WebDAV 配置状态（ADR-009：官方 WebDAV 为唯一目标）
  let webdavConfigured = false;
  let webdavUrl = '';

  // 备份配置
  let backupPaths = [];
  let targetFolder = 'fn-backup';
  let scheduleCron = '';
  let scheduleCronValid = true;

  // 备份/恢复
  let backupResult = null;
  // 可恢复文件（从 SQLite 查询）
  let restoreFolders = [];
  // 实时任务状态（WebSocket 推送）
  let liveStatus = null; // { kind, status, current_file, done, total }
  let wsConnected = false;
  // 账号信息（WebDAV 用户名）
  let userInfo = null;
  let userInfoError = null;
  // 加密密钥（age 公钥展示 / 自定义私钥 / 自动生成后提醒保存）
  let keyInfo = null; // { public_key }
  let revealKey = ''; // 首次启动自动生成的私钥（后端一次性下发）
  let keyBackedUp = false; // 用户是否已确认备份私钥
  // 监控告警
  let alerts = [];
  // 告警 Webhook 配置（通知设置）
  let webhookUrl = '';
  let webhookHeaders = []; // [{ name, value }]
  let webhookBody = '';

  const navItems = [
    { id: 'dashboard', label: '📊 概览' },
    { id: 'backup', label: '⬆️ 备份' },
    { id: 'restore', label: '⬇️ 恢复' },
    { id: 'settings', label: '⚙️ 设置' },
  ];

  // 切换页面；进入恢复页时刷新可恢复列表，避免展示过期数据
  function go(page) {
    currentPage = page;
    if (page === 'restore') loadRestoreFiles();
  }

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
              bytes_done: data.bytes_done,
              bytes_total: data.bytes_total,
              elapsed_ms: data.elapsed_ms,
              speed: data.speed,
              at: Date.now(),
              message: data.message,
            };
            // 任务结束（备份/恢复完成）后刷新可恢复列表，保证恢复页数据最新
            if (data.status === 'completed') {
              loadRestoreFiles();
            }
            // 任务失败后刷新告警（后端已记录告警）
            if (data.status === 'failed') {
              loadAlerts();
            }
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
      webdavConfigured = !!data.webdav_configured;
      webdavUrl = data.webdav_url || '';
      webhookUrl = data.webhook_url || '';
      webhookHeaders = data.webhook_headers || [];
      webhookBody = data.webhook_body || '';
      keyBackedUp = !!data.key_backed_up;
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    }
  }

  // 加载账号信息（WebDAV 用户名）
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

  // 加载密钥信息（只回传公钥；首次自动生成的私钥会一次性返回）
  async function loadKeys() {
    try {
      const res = await fetch('/api/keys');
      const data = await res.json();
      keyInfo = { public_key: data.public_key };
      if (data.private_key_once) {
        revealKey = data.private_key_once;
      }
    } catch (e) {
      error = e.message;
    }
  }

  // 使用自定义私钥（POST /api/keys）
  async function handleSetKey(privateKey) {
    const res = await fetch('/api/keys', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ private_key: privateKey }),
    });
    const data = await res.json();
    if (data.success) {
      keyInfo = { public_key: data.public_key };
      revealKey = '';
      keyBackedUp = true; // 私钥由用户提供，视为已备份
    }
    return data;
  }

  // 自动生成新密钥对（返回的私钥仅此一次展示，提醒用户保存）
  async function handleGenerateKey() {
    const res = await fetch('/api/keys/generate', { method: 'POST' });
    const data = await res.json();
    if (data.success) {
      keyInfo = { public_key: data.public_key };
      keyBackedUp = false; // 新生成的私钥尚未保存
    }
    return data;
  }

  // 导出当前私钥明文（用于另存备份；需管理员口令校验）
  // 展示私钥后即视为「需重新确认备份」，重置标记使「我已妥善保存」可再次点击
  async function handleExportKey(passphrase) {
    const res = await fetch('/api/keys/export', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ passphrase }),
    });
    const data = await res.json();
    if (data.private_key) keyBackedUp = false;
    return data;
  }

  // 确认已妥善备份私钥
  async function handleBackupAck() {
    await fetch('/api/keys/backup-ack', { method: 'POST' });
    keyBackedUp = true;
  }

  // 加载告警（监控：备份/恢复失败、配置缺失等）
  async function loadAlerts() {
    try {
      const res = await fetch('/api/alerts');
      const data = await res.json();
      alerts = data.alerts || [];
    } catch (e) {
      error = e.message;
    }
  }

  // 清空告警
  async function clearAlerts() {
    try {
      await fetch('/api/alerts', { method: 'DELETE' });
      alerts = [];
    } catch (e) {
      error = e.message;
    }
  }

  // 保存告警 Webhook 配置（地址空串 = 关闭外发；可选自定义 headers/body 模板）
  async function handleSaveWebhook(url, headers, bodyTemplate) {
    const res = await fetch('/api/notify/webhook', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ webhook_url: url, headers, body_template: bodyTemplate }),
    });
    const data = await res.json();
    if (data.success) {
      webhookUrl = data.webhook_url || '';
      webhookHeaders = data.headers || [];
      webhookBody = data.body_template || '';
    }
    return data;
  }

  // 测试 Webhook 连通性（用当前表单配置直接发一条测试通知）
  async function handleTestWebhook(url, headers, bodyTemplate) {
    const res = await fetch('/api/notify/webhook/test', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ webhook_url: url, headers, body_template: bodyTemplate }),
    });
    return res.json();
  }

  // 导出配置（含 WebDAV 凭据与 age 私钥；需管理员口令）
  async function handleExportConfig(passphrase) {
    const res = await fetch('/api/config/export', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ passphrase }),
    });
    return res.json();
  }

  // 导入配置（覆盖配置并可恢复 age 私钥；需管理员口令）
  async function handleImportConfig(passphrase, configText) {
    const res = await fetch('/api/config/import', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ passphrase, config: configText }),
    });
    const data = await res.json();
    if (data.success) {
      // 配置已变更：刷新本地状态
      await Promise.all([loadConfig(), loadKeys(), loadUserInfo(), loadRestoreFiles()]);
    }
    return data;
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

  // 保存 WebDAV 凭据（地址固定为官方地址，后端实测连通性后加密存储；返回消息字符串）
  async function handleSaveWebdav(username, password) {
    busy = true;
    error = null;
    try {
      const res = await fetch('/api/webdav/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      });
      const data = await res.json();
      if (data.success) {
        webdavConfigured = true;
        webdavUrl = data.url || 'https://dav.kzwr.com/dav';
        loadUserInfo();
        return 'WebDAV 已配置并验证通过';
      }
      return `配置失败: ${data.error}`;
    } catch (e) {
      error = e.message;
      return `配置失败: ${e.message}`;
    } finally {
      busy = false;
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
      if (data.error) {
        error = data.error;
      } else {
        // 备份产生新快照，刷新可恢复列表
        await loadRestoreFiles();
      }
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
  loadKeys();
  loadAlerts();
  connectWS();
</script>

<main>
  <header>
    <h1>🛡️ fnos 增量加密备份</h1>
    <p class="health" class:ok={health.startsWith('服务正常')}>{health}</p>
  </header>

  <!-- 顶部导航栏 -->
  <nav class="top-nav">
    {#each navItems as item}
      <button
        class="nav-item"
        class:active={currentPage === item.id}
        on:click={() => go(item.id)}
      >
        {item.label}
      </button>
    {/each}
  </nav>

  {#if error}
    <div class="error">⚠️ {error}</div>
  {/if}

  <!-- 监控告警（备份/恢复失败等） -->
  <AlertBanner {alerts} onClear={clearAlerts} />

  <!-- 主体内容 + 右侧常驻实时任务面板 -->
  <div class="layout">
    <div class="content">
      <!-- 按功能切换页面 -->
      {#if currentPage === 'dashboard'}
        <DashboardPage
          {backupPaths}
          {targetFolder}
          {webdavConfigured}
          {webdavUrl}
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
        <RestorePage {restoreFolders} {backupPaths} {busy} onRestore={handleRestore} />
      {:else if currentPage === 'settings'}
        <SettingsPage
          {webdavConfigured}
          {webdavUrl}
          {busy}
          {userInfo}
          {userInfoError}
          {keyInfo}
          {revealKey}
          {keyBackedUp}
          {webhookUrl}
          {webhookHeaders}
          {webhookBody}
          onSaveWebdav={handleSaveWebdav}
          onSetKey={handleSetKey}
          onGenerateKey={handleGenerateKey}
          onExportKey={handleExportKey}
          onBackupAck={handleBackupAck}
          onSaveWebhook={handleSaveWebhook}
          onTestWebhook={handleTestWebhook}
          onExportConfig={handleExportConfig}
          onImportConfig={handleImportConfig}
        />
      {/if}
    </div>

    <!-- 实时任务：常驻右侧独立区域，无任务时展示空闲态 -->
    <aside class="side">
      <LiveStatus {liveStatus} {wsConnected} />
    </aside>
  </div>

  <footer>fnos-backup · age 加密 · WebDAV 增量备份</footer>
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: system-ui, -apple-system, sans-serif;
    background: #f5f6fa;
    color: #1f2d3d;
  }
  main {
    max-width: 1100px;
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

  /* 主体 + 右侧面板 */
  .layout {
    display: flex;
    gap: 20px;
    align-items: flex-start;
  }
  .content { flex: 1; min-width: 0; }
  .side {
    width: 300px;
    flex-shrink: 0;
    position: sticky;
    top: 60px;
  }
  @media (max-width: 900px) {
    .layout { flex-direction: column; }
    .side { width: 100%; position: static; }
  }

  .error { background: #fef2f2; color: #b91c1c; padding: 12px; border-radius: 6px; margin-top: 12px; }
  /* 全局 section 间距（统一卡片之间的留白） */
  :global(.content > section) {
    margin-top: 20px;
  }
  footer { text-align: center; color: #8a94a6; font-size: 13px; margin-top: 28px; }
</style>
