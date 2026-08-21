<script>
  import LiveStatus from './components/LiveStatus.svelte';
  import OverviewSection from './components/OverviewSection.svelte';
  import LoginSection from './components/LoginSection.svelte';
  import BackupConfigSection from './components/BackupConfigSection.svelte';
  import BackupSection from './components/BackupSection.svelte';
  import RestoreSection from './components/RestoreSection.svelte';

  // 全局状态
  let health = '检查中...';
  let error = null;
  let busy = false;

  // kzwr 登录
  let loggedIn = false;

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
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
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

  // kzwr 登录（供 LoginSection 调用，返回消息字符串）
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

  // 保存配置（供 BackupConfigSection 调用）
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

  // 立即备份（供 BackupSection 调用）
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

  // 恢复请求（供 RestoreSection 调用，返回 {restored, restored_bytes, error}）
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
  connectWS();
</script>

<main>
  <header>
    <h1>🛡️ fnos 增量加密备份</h1>
    <p class="health" class:ok={health.startsWith('服务正常')}>{health}</p>
  </header>

  {#if error}
    <div class="error">⚠️ {error}</div>
  {/if}

  <!-- 实时任务状态（WebSocket 推送） -->
  <LiveStatus {liveStatus} />

  <!-- 当前配置概览 -->
  <OverviewSection
    {backupPaths}
    {targetFolder}
    {loggedIn}
    {restoreFolders}
    {scheduleCron}
    {scheduleCronValid}
  />

  <!-- kzwr 登录 -->
  <LoginSection {loggedIn} {busy} onLogin={handleLogin} />

  <!-- 备份路径配置 + 定时 cron -->
  <BackupConfigSection
    bind:backupPaths
    bind:targetFolder
    bind:scheduleCron
    bind:scheduleCronValid
    {busy}
    onSave={handleSaveConfig}
  />

  <!-- 备份 -->
  <BackupSection {busy} {backupResult} onRunBackup={handleRunBackup} />

  <!-- 恢复 -->
  <RestoreSection {restoreFolders} {busy} onRestore={handleRestore} />

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
  header { padding: 24px 0 16px; border-bottom: 1px solid #e0e4ea; }
  h1 { margin: 0; font-size: 24px; }
  .health { color: #5a6a7a; }
  .health.ok { color: #22a06b; }
  .error { background: #fef2f2; color: #b91c1c; padding: 12px; border-radius: 6px; margin-top: 12px; }
  footer { text-align: center; color: #8a94a6; font-size: 13px; margin-top: 28px; }
</style>
