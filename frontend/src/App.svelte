<script>
  import DashboardPage from './views/DashboardPage.svelte';
  import BackupPage from './views/BackupPage.svelte';
  import RestorePage from './views/RestorePage.svelte';
  import SettingsPage from './views/SettingsPage.svelte';
  import AuditPage from './views/AuditPage.svelte';
  import LiveStatus from './components/LiveStatus.svelte';
  import AlertBanner from './components/AlertBanner.svelte';
  import Toast from './components/Toast.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';
  import Icon from './components/Icon.svelte';
  import Logo from './components/Logo.svelte';
  import RailAccount from './components/RailAccount.svelte';

  import { api } from './lib/api.js';
  import { toast } from './lib/toast.js';
  import { theme, initTheme, toggleTheme } from './lib/theme.js';

  /* ── 全局状态 ─────────────────────────────────────────────── */
  let health = ''; // 服务状态文本
  let version = '';
  let serviceOk = false;
  let error = null;

  let currentPage = 'dashboard';
  let loading = true;

  // WebDAV 目标
  let webdavConfigured = false;
  let webdavUrl = '';

  // 备份配置
  let backupPaths = [];
  let targetFolder = 'fn-backup';
  let scheduleCron = '';
  let scheduleCronValid = true;

  // 备份 / 恢复
  let busy = false;
  let backupResult = null;
  let restoreFolders = [];

  // 实时任务（WebSocket）
  let liveStatus = null;
  let wsConnected = false;

  // 账号
  let userInfo = null;
  let userInfoError = null;

  // 密钥
  let keyInfo = null;
  let revealKey = '';
  let keyBackedUp = false;

  // 告警与通知
  let alerts = [];
  // 非敏感配置回显：WebDAV 用户名（密码永不返回）与保留策略
  let webdavUsername = '';
  let retention = {
    enabled: false,
    cleanup_unmanaged: false,
    min_age_days: 0,
    empty_recycle_bin: false,
  };
  // kzwr REST 增强功能（可选）
  let kzwrConfigured = false;
  let kzwrUser = null;
  let kzwrQuotaWarnPercent = 85;
  // 账号一致性提醒 / 一键体检
  let webdavWarning = '';
  let setupResult = null;
  let webhookUrl = '';
  let webhookHeaders = [];
  let webhookBody = '';

  const NAV = [
    { id: 'dashboard', label: '概览', icon: 'grid' },
    { id: 'backup', label: '备份', icon: 'upload' },
    { id: 'restore', label: '恢复', icon: 'download' },
    { id: 'settings', label: '设置', icon: 'sliders' },
    { id: 'audit', label: '审计', icon: 'file' },
  ];

  const PAGE_META = {
    dashboard: { title: '概览', desc: '备份状态、配置一览与实时任务进度' },
    backup: { title: '备份', desc: '配置备份路径与定时任务，或立即执行一次增量备份' },
    restore: { title: '恢复', desc: '浏览云端备份内容，按文件或目录恢复到原位置' },
    settings: { title: '设置', desc: 'WebDAV 凭据、加密密钥、通知与配置迁移' },
    audit: { title: '操作审计', desc: '敏感与破坏性操作的本地留痕（audit.log）' },
  };

  $: page = PAGE_META[currentPage] || PAGE_META.dashboard;
  $: alertsCount = alerts.length;
  $: readyToRun = webdavConfigured && backupPaths.length > 0;

  /* ── 数据加载 ─────────────────────────────────────────────── */

  async function loadHealth() {
    try {
      const d = await api.health();
      version = d.version || '';
      health = d.status === 'ok' ? `服务正常` : `状态异常`;
      serviceOk = d.status === 'ok';
    } catch (e) {
      health = '服务不可达';
      serviceOk = false;
    }
  }

  async function loadConfig() {
    try {
      const d = await api.config();
      backupPaths = d.backup_paths || [];
      targetFolder = d.target_folder || 'fn-backup';
      scheduleCron = d.schedule_cron || '';
      scheduleCronValid = d.schedule_cron_valid !== false;
      webdavConfigured = !!d.webdav_configured;
      webdavUrl = d.webdav_url || '';
      webdavUsername = d.webdav_username || '';
      retention = d.retention || {
        enabled: false,
        cleanup_unmanaged: false,
        min_age_days: 0,
        empty_recycle_bin: false,
      };
      kzwrConfigured = !!d.kzwr_token_configured;
      kzwrQuotaWarnPercent = d.kzwr_quota_warn_percent ?? 85;
      webhookUrl = d.webhook_url || '';
      webhookHeaders = d.webhook_headers || [];
      webhookBody = d.webhook_body || '';
      keyBackedUp = !!d.key_backed_up;
    } catch (e) {
      error = e.message;
    }
  }

  /** 加载 kzwr 增强信息（未配置 token 时不请求，避免无谓提示） */
  async function loadKzwrUser() {
    if (!kzwrConfigured) {
      kzwrUser = null;
      return;
    }
    try {
      kzwrUser = await api.kzwrUser();
    } catch (e) {
      kzwrUser = { error: e.message };
    }
  }

  async function loadUserInfo() {
    try {
      const d = await api.userInfo();
      userInfo = d;
      userInfoError = d.error || null;
    } catch (e) {
      userInfoError = e.message;
    }
  }

  async function loadKeys() {
    try {
      const d = await api.keys();
      keyInfo = { public_key: d.public_key };
      if (d.private_key_once) revealKey = d.private_key_once;
    } catch (e) {
      error = e.message;
    }
  }

  async function loadAlerts() {
    try {
      const d = await api.alerts();
      alerts = d.alerts || [];
    } catch (e) {
      error = e.message;
    }
  }

  async function loadRestoreFiles() {
    try {
      const d = await api.restoreFiles();
      restoreFolders = d.folders || [];
    } catch (e) {
      error = e.message;
    }
  }

  async function loadAll() {
    loading = true;
    await Promise.all([loadHealth(), loadConfig(), loadUserInfo(), loadKeys(), loadAlerts(), loadRestoreFiles()]);
    // 依赖 loadConfig 得到的 kzwrConfigured，故串行放在其后
    await loadKzwrUser();
    loading = false;
  }

  /* ── 页面切换 ─────────────────────────────────────────────── */

  function go(pageId) {
    currentPage = pageId;
    error = null;
    if (pageId === 'restore') loadRestoreFiles();
  }

  /* ── WebSocket 实时状态 ───────────────────────────────────── */

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
          if (data.type !== 'event') return;
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
          if (data.status === 'completed') {
            loadRestoreFiles();
            if (data.kind === 'backup') toast.success(data.message || '备份任务已完成', '备份完成');
            if (data.kind === 'restore') toast.success(data.message || '恢复任务已完成', '恢复完成');
          }
          if (data.status === 'failed') {
            loadAlerts();
            toast.error(data.message || '任务执行失败', '任务失败');
          }
        } catch (e) {
          /* 忽略非法事件 */
        }
      };
      ws.onclose = () => {
        wsConnected = false;
        setTimeout(connectWS, 3000);
      };
      ws.onerror = () => ws.close();
    } catch (e) {
      /* 忽略 */
    }
  }

  /* ── 操作：备份 ───────────────────────────────────────────── */

  async function handleSaveConfig() {
    busy = true;
    error = null;
    try {
      const d = await api.saveConfig({
        backup_paths: backupPaths,
        target_folder: targetFolder,
        schedule_cron: scheduleCron.trim(),
      });
      scheduleCronValid = d.schedule_cron_valid !== false;
      if (d.error) {
        error = d.error;
        toast.error(d.error, '保存失败');
      } else if (!scheduleCronValid) {
        error = 'cron 表达式无效，已拒绝保存';
        toast.error('cron 表达式无效，已拒绝保存');
      } else {
        toast.success('配置已保存并即时生效');
      }
    } catch (e) {
      error = e.message;
      toast.error(e.message);
    } finally {
      busy = false;
    }
  }

  /** kzwr 增强：保存或清除 access-token（保存成功后刷新账号信息） */
  async function handleSaveKzwrToken(token) {
    busy = true;
    error = null;
    try {
      const d = await api.kzwrSaveToken(token);
      kzwrConfigured = !!d.configured;
      if (d.success) await loadKzwrUser();
      else if (!kzwrConfigured) kzwrUser = null;
      return d;
    } catch (e) {
      return { success: false, configured: kzwrConfigured, error: e.message };
    } finally {
      busy = false;
    }
  }

  /** kzwr 增强：保存空间预警阈值（复用 /api/config） */
  async function handleSaveKzwrQuota(percent) {
    busy = true;
    error = null;
    try {
      const d = await api.saveConfig({
        backup_paths: backupPaths,
        target_folder: targetFolder,
        kzwr_quota_warn_percent: Math.max(0, Math.min(100, Math.floor(Number(percent) || 0))),
      });
      if (d.error) {
        error = d.error;
        return { error: d.error };
      }
      kzwrQuotaWarnPercent = d.kzwr_quota_warn_percent ?? percent;
      return {};
    } catch (e) {
      error = e.message;
      return { error: e.message };
    } finally {
      busy = false;
    }
  }

  /** 定时任务预览：cron → 未来 5 次触发时间 */
  async function handlePreviewCron(cron) {
    try {
      return await api.schedulePreview(cron || '');
    } catch (e) {
      return { valid: false, next: [], error: e.message };
    }
  }

  /** 一键体检 */
  async function handleSetupCheck() {
    busy = true;
    error = null;
    try {
      setupResult = await api.setupCheck();
      await loadAlerts();
      if (kzwrConfigured) await loadKzwrUser();
      return setupResult;
    } catch (e) {
      error = e.message;
      toast.error(e.message, '体检失败');
      return null;
    } finally {
      busy = false;
    }
  }

  /** kzwr 增强：清空回收站 */
  async function handleEmptyTrash() {
    busy = true;
    error = null;
    try {
      return await api.kzwrTrashEmpty();
    } catch (e) {
      return { emptied: 0, error: e.message };
    } finally {
      busy = false;
    }
  }

  /** 保存保留策略：只提交保留策略（不提交 cron，避免被无效表达式阻塞） */
  async function handleSaveRetention(next) {
    busy = true;
    error = null;
    try {
      const d = await api.saveConfig({
        backup_paths: backupPaths,
        target_folder: targetFolder,
        retention_enabled: next.enabled,
        retention_cleanup_unmanaged: next.cleanup_unmanaged,
        retention_min_age_days: next.min_age_days,
        retention_empty_recycle_bin: next.empty_recycle_bin,
      });
      if (d.error) {
        error = d.error;
        return { error: d.error };
      }
      // 用后端返回值回写，保证页面与磁盘一致
      retention = d.retention || next;
      return {};
    } catch (e) {
      error = e.message;
      return { error: e.message };
    } finally {
      busy = false;
    }
  }

  async function handleRunBackup() {
    busy = true;
    error = null;
    backupResult = null;
    try {
      const d = await api.runBackup();
      backupResult = d;
      if (d.error) {
        error = d.error;
        if (d.skipped) toast.warn(d.error, '已跳过');
        else toast.error(d.error, '备份失败');
      } else {
        await loadRestoreFiles();
        toast.success(
          `上传 ${d.uploaded} 个文件（${d.uploaded_bytes} 字节），未变化 ${d.unchanged}`,
          '备份完成',
        );
      }
    } catch (e) {
      error = e.message;
      toast.error(e.message, '备份失败');
    } finally {
      busy = false;
    }
  }

  /* ── 操作：恢复 ───────────────────────────────────────────── */

  /** 恢复：all=true 时按 sourcePath（可用 dir 限定子目录）全量恢复 */
  async function handleRestore(files, sourcePath, all = false, dir = '') {
    return api.restore(files, sourcePath, all, dir);
  }

  /** 恢复树懒加载：展开目录时按需拉取一层 */
  async function handleLoadTree(source, dir) {
    return api.restoreTree(source, dir);
  }

  /* ── 操作：WebDAV / 密钥 / 通知 / 配置迁移 ───────────────── */

  async function handleSaveWebdav(username, password) {
    busy = true;
    try {
      const d = await api.saveWebdav(username, password);
      if (d.success) {
        webdavConfigured = true;
        webdavUrl = d.url || webdavUrl;
        webdavWarning = d.warning || '';
        await loadUserInfo();
        if (webdavWarning) toast.warn('凭据已保存，但检测到账号不一致', '请检查账号');
        else toast.success('WebDAV 凭据已保存并验证通过');
        return '';
      }
      toast.error(d.error || '凭据验证未通过');
      return d.error || '配置失败';
    } catch (e) {
      toast.error(e.message);
      return e.message;
    } finally {
      busy = false;
    }
  }

  async function handleSetKey(privateKey) {
    const d = await api.setKey(privateKey);
    if (d.success) {
      keyInfo = { public_key: d.public_key };
      revealKey = '';
      keyBackedUp = true;
      toast.success('私钥已更新并立即生效');
    }
    return d;
  }

  async function handleGenerateKey() {
    const d = await api.generateKey();
    if (d.success) {
      keyInfo = { public_key: d.public_key };
      keyBackedUp = false;
      toast.warn('已生成新密钥对，请立即保存私钥', '密钥已轮换');
    }
    return d;
  }

  async function handleExportKey(passphrase) {
    const d = await api.exportKey(passphrase);
    if (d.private_key) keyBackedUp = false;
    return d;
  }

  async function handleBackupAck() {
    await api.backupAck();
    keyBackedUp = true;
    toast.success('已记录「私钥已妥善保存」', '风险提示已关闭');
  }

  async function handleSaveWebhook(url, headers, bodyTemplate) {
    const d = await api.saveWebhook(url, headers, bodyTemplate);
    if (d.success) {
      webhookUrl = d.webhook_url || '';
      webhookHeaders = d.headers || [];
      webhookBody = d.body_template || '';
    }
    return d;
  }

  async function handleTestWebhook(url, headers, bodyTemplate) {
    return api.testWebhook(url, headers, bodyTemplate);
  }

  async function handleExportConfig(passphrase) {
    return api.exportConfig(passphrase);
  }

  async function handleImportConfig(passphrase, configText) {
    const d = await api.importConfig(passphrase, configText);
    if (d.success) {
      await Promise.all([loadConfig(), loadKeys(), loadUserInfo(), loadRestoreFiles()]);
      await loadKzwrUser();
      toast.success('配置已导入并即时生效');
    }
    return d;
  }

  async function clearAlerts() {
    try {
      await api.clearAlerts();
      alerts = [];
      toast.info('告警已清空');
    } catch (e) {
      toast.error(e.message);
    }
  }

  /* ── 启动 ─────────────────────────────────────────────────── */

  initTheme();
  loadAll();
  connectWS();
</script>

<div class="app">
  <!-- 侧边栏：品牌 + 导航 + 运行状态 -->
  <aside class="sidebar">
    <div class="brand">
      <Logo size={36} />
      <div class="brand-text">
        <strong>酷族备份</strong>
        <span>增量加密 · WebDAV</span>
      </div>
    </div>

    <nav class="nav">
      {#each NAV as item (item.id)}
        <button
          class="nav-item"
          class:active={currentPage === item.id}
          on:click={() => go(item.id)}
        >
          <Icon name={item.icon} size={17} />
          <span class="nav-label">{item.label}</span>
          {#if item.id === 'dashboard' && alertsCount > 0}
            <span class="nav-badge">{alertsCount}</span>
          {/if}
        </button>
      {/each}
    </nav>

    <div class="sidebar-foot">
      <div class="status-pill" class:ok={serviceOk}>
        <span class="dot" class:on={serviceOk} class:off={!serviceOk}></span>
        <span>{health || '检查中'}</span>
        {#if version}<em>v{version}</em>{/if}
      </div>
      <div class="status-pill" class:ok={wsConnected}>
        <span class="dot" class:on={wsConnected} class:off={!wsConnected}></span>
        <span>{wsConnected ? '实时通道已连接' : '实时通道断开'}</span>
      </div>
      <button class="theme-btn" on:click={toggleTheme}>
        <Icon name={$theme === 'dark' ? 'sun' : 'moon'} size={15} />
        <span>{$theme === 'dark' ? '浅色模式' : '深色模式'}</span>
      </button>
    </div>
  </aside>

  <!-- 主区域 -->
  <div class="main">
    <header class="topbar">
      <div class="titles">
        <h1>{page.title}</h1>
        <p>{page.desc}</p>
      </div>
      <div class="actions">
        <button
          class="btn btn-primary"
          on:click={handleRunBackup}
          disabled={busy || !webdavConfigured}
          title={webdavConfigured ? '立即执行一次增量备份' : '请先在设置中配置 WebDAV 凭据'}
        >
          {#if busy}
            <span class="spin"></span>执行中
          {:else}
            <Icon name="zap" size={15} />立即备份
          {/if}
        </button>
      </div>
    </header>

    <div class="scroll">
      {#if !webdavConfigured}
        <div class="alert alert-warn" role="status">
          <Icon name="alert" size={17} />
          <div class="alert-body">
            <div class="alert-title">尚未配置 WebDAV 凭据</div>
            填写账号密码后才能执行备份与恢复。
            <button class="btn btn-sm btn-soft inline" on:click={() => go('settings')}>
              前往设置<Icon name="arrow-right" size={13} />
            </button>
          </div>
        </div>
      {/if}

      {#if error}
        <div class="alert alert-danger" role="alert">
          <Icon name="alert" size={17} />
          <div class="alert-body">{error}</div>
          <button class="btn-icon btn-sm" on:click={() => (error = null)} aria-label="关闭">
            <Icon name="x" size={15} />
          </button>
        </div>
      {/if}

      <AlertBanner {alerts} onClear={clearAlerts} />

      <div class="columns">
        <div class="content">
          {#if loading}
            <div class="card">
              <div class="card-body loading-card">
                <div class="skeleton" style="height: 18px; width: 38%"></div>
                <div class="skeleton" style="height: 64px"></div>
                <div class="skeleton" style="height: 64px"></div>
              </div>
            </div>
          {:else if currentPage === 'dashboard'}
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
              {keyBackedUp}
              kzwr={kzwrUser}
              {setupResult}
              {busy}
              onSetupCheck={handleSetupCheck}
              onGoto={go}
            />
          {:else if currentPage === 'backup'}
            <BackupPage
              bind:backupPaths
              bind:targetFolder
              bind:scheduleCron
              bind:scheduleCronValid
              {busy}
              {backupResult}
              {webdavConfigured}
              onSave={handleSaveConfig}
              onRunBackup={handleRunBackup}
              onPreviewCron={handlePreviewCron}
              onGoto={go}
            />
          {:else if currentPage === 'restore'}
            <RestorePage
              {restoreFolders}
              {backupPaths}
              {busy}
              onRestore={handleRestore}
              onLoadTree={handleLoadTree}
              onGoto={go}
            />
          {:else if currentPage === 'audit'}
            <AuditPage {busy} />
          {:else if currentPage === 'settings'}
            <SettingsPage
              {webdavConfigured}
              {webdavUrl}
              {webdavUsername}
              {webdavWarning}
              {retention}
              {kzwrConfigured}
              {kzwrUser}
              {kzwrQuotaWarnPercent}
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
              onSaveRetention={handleSaveRetention}
              onSaveKzwrToken={handleSaveKzwrToken}
              onSaveKzwrQuota={handleSaveKzwrQuota}
              onEmptyTrash={handleEmptyTrash}
              onSaveWebhook={handleSaveWebhook}
              onTestWebhook={handleTestWebhook}
              onExportConfig={handleExportConfig}
              onImportConfig={handleImportConfig}
            />
          {/if}
        </div>

        <aside class="rail">
          <RailAccount
            username={webdavUsername || (userInfo && userInfo.username) || ''}
            configured={webdavConfigured}
            kzwr={kzwrUser}
            onGoto={go}
          />
          <LiveStatus {liveStatus} {wsConnected} />
        </aside>
      </div>
    </div>
  </div>
</div>

<Toast />
<ConfirmDialog />

<style>
  .app {
    display: flex;
    min-height: 100vh;
    background: var(--bg);
  }

  /* ── 侧边栏 ─────────────────────────────────────────────── */
  .sidebar {
    width: var(--sidebar-w);
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    padding: var(--s4) var(--s3);
    background: var(--surface);
    border-right: 1px solid var(--border);
    position: sticky;
    top: 0;
    height: 100vh;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: var(--s2) var(--s2) var(--s3);
  }
  .brand-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .brand-text strong {
    font-size: 14.5px;
    font-weight: 680;
    letter-spacing: -0.01em;
  }
  .brand-text span {
    font-size: 11.5px;
    color: var(--text-3);
  }

  .nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 10px;
    border: none;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text-2);
    font-family: inherit;
    font-size: 13.5px;
    font-weight: 550;
    cursor: pointer;
    text-align: left;
    transition: background var(--t-fast), color var(--t-fast);
  }
  .nav-item:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .nav-item.active {
    background: var(--primary-soft);
    color: var(--primary);
  }
  .nav-label {
    flex: 1;
  }
  .nav-badge {
    min-width: 18px;
    height: 18px;
    padding: 0 5px;
    border-radius: var(--r-full);
    background: var(--danger);
    color: #fff;
    font-size: 11px;
    font-weight: 650;
    display: grid;
    place-items: center;
  }

  .sidebar-foot {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: var(--s3);
    border-top: 1px solid var(--border);
  }
  .status-pill {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 6px 9px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-3);
    font-size: 11.5px;
  }
  .status-pill.ok {
    color: var(--text-2);
  }
  .status-pill em {
    margin-left: auto;
    font-style: normal;
    font-family: var(--mono);
    font-size: 10.5px;
    color: var(--text-3);
  }
  .theme-btn {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 9px;
    border-radius: var(--r-sm);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text-2);
    font-family: inherit;
    font-size: 12px;
    cursor: pointer;
    transition: background var(--t-fast), color var(--t-fast);
  }
  .theme-btn:hover {
    background: var(--surface-3);
    color: var(--text);
  }

  /* ── 主区域 ─────────────────────────────────────────────── */
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .topbar {
    display: flex;
    align-items: center;
    gap: var(--s4);
    padding: var(--s5) var(--s6);
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--bg) 82%, transparent);
    backdrop-filter: blur(8px);
    position: sticky;
    top: 0;
    z-index: 20;
  }
  .titles {
    flex: 1;
    min-width: 0;
  }
  .titles h1 {
    font-size: 19px;
    line-height: 1.3;
  }
  .titles p {
    margin-top: 3px;
    color: var(--text-3);
    font-size: 12.5px;
  }
  .actions {
    display: flex;
    gap: var(--s2);
  }

  .scroll {
    padding: var(--s5) var(--s6) var(--s8);
    display: flex;
    flex-direction: column;
    gap: var(--s4);
  }
  .columns {
    display: flex;
    gap: var(--s5);
    align-items: flex-start;
  }
  .content {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s4);
  }
  .rail {
    width: var(--rail-w);
    flex-shrink: 0;
    position: sticky;
    top: 92px;
  }
  .inline {
    margin-left: 6px;
  }
  .loading-card {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    padding: var(--s5);
  }

  /* ── 响应式 ─────────────────────────────────────────────── */
  @media (max-width: 1100px) {
    .rail {
      width: 290px;
    }
  }
  @media (max-width: 900px) {
    .app {
      flex-direction: column;
    }
    .sidebar {
      width: 100%;
      height: auto;
      position: static;
      flex-direction: row;
      align-items: center;
      gap: var(--s3);
      overflow-x: auto;
      border-right: none;
      border-bottom: 1px solid var(--border);
      padding: var(--s2) var(--s3);
    }
    .brand {
      padding: 0 var(--s2) 0 0;
      flex-shrink: 0;
    }
    .nav {
      flex-direction: row;
      gap: 4px;
      flex: 1;
    }
    .nav-item {
      padding: 7px 11px;
    }
    .nav-label {
      display: none;
    }
    .sidebar-foot {
      margin-top: 0;
      flex-direction: row;
      border-top: none;
      padding-top: 0;
      flex-shrink: 0;
    }
    .sidebar-foot .status-pill span:not(.dot) {
      display: none;
    }
    .status-pill em {
      display: none;
    }
    .theme-btn span {
      display: none;
    }
    .columns {
      flex-direction: column;
    }
    .rail {
      width: 100%;
      position: static;
    }
    .topbar {
      padding: var(--s4) var(--s4);
    }
    .scroll {
      padding: var(--s4) var(--s4) var(--s7);
    }
  }
</style>
