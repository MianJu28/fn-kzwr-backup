<script>
  import DashboardPage from './views/DashboardPage.svelte';
  import RestorePage from './views/RestorePage.svelte';
  import TasksPage from './views/TasksPage.svelte';
  import TargetsPage from './views/TargetsPage.svelte';
  import PluginsPage from './views/PluginsPage.svelte';
  import SettingsPage from './views/SettingsPage.svelte';
  import AuditPage from './views/AuditPage.svelte';
  import LogsPage from './views/LogsPage.svelte';
  import LiveStatus from './components/LiveStatus.svelte';
  import MessagesPanel from './components/MessagesPanel.svelte';
  import Toast from './components/Toast.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';
  import { onMount, onDestroy } from 'svelte';
  import { confirmDialog } from './lib/confirm.js';
  import Icon from './components/Icon.svelte';
  import Logo from './components/Logo.svelte';

  import { api } from './lib/api.js';
  import { APP_BASE } from './lib/appBase.js';
  import { toast } from './lib/toast.js';
  import { theme, initTheme, toggleTheme } from './lib/theme.js';
  import { setHostTimezone } from './lib/format.js';
  import { loadPlugins } from './lib/plugins.js';

  /* ── 全局状态 ─────────────────────────────────────────────── */
  let health = ''; // 服务状态文本
  let version = '';
  let serviceOk = false;
  let error = null;

  let currentPage = 'dashboard';
  let loading = true;

  // 备份路径（**只用于恢复页**：判断「还没配过任何源」以给出引导文案；
  // 真正的路径配置在「任务」页，按任务管理）
  let backupPaths = [];
  // 宿主时区说明（如「CST (UTC+08:00)」，用于页面标注时间口径）
  let scheduleTimezone = '';
  // 插件清单（/api/plugins）：「插件」页的卡片区由它驱动
  let plugins = [];
  // 多任务 / 多目标（ADR-014）
  let tasks = [];
  let targets = []; // 目标详情（/api/targets）
  let targetOptions = []; // 目标精简项（/api/tasks 附带，供任务表单下拉）
  // 外置插件（动态库，ADR-013 方案 B）
  let pluginsEnabledCfg = false;
  /** 插件市场是否启用（来自 /api/config） */
  let marketEnabledCfg = false;
  let pluginsDirCfg = '';
  /** 插件签名公钥（base64 32 字节 Ed25519；空 = 拒绝加载任何外置插件） */
  let pluginsPubkeysCfg = [];
  /** 是否由环境变量放行未签名插件（只读，仅本机调试） */
  let pluginsAllowUnsigned = false;

  // 备份 / 恢复
  let busy = false;
  let backupResult = null;
  let restoreFolders = [];

  // 实时任务（WebSocket）
  let liveStatus = null;
  /** 取消请求已提交且尚未生效（`busy` 已有，复用现有的忙标志） */
  let cancelRequested = false;
  let wsConnected = false;

  // 密钥
  let keyInfo = null;
  let revealKey = '';
  let keyBackedUp = false;

  // 告警与通知
  let alerts = [];
  let debugOn = false;
  // 一键体检
  let setupResult = null;
  let webhookUrl = '';
  let webhookHeaders = [];
  let webhookBody = '';

  const NAV = [
    { id: 'dashboard', label: '概览', icon: 'grid' },
    { id: 'tasks', label: '任务', icon: 'package' },
    { id: 'targets', label: '目标', icon: 'cloud' },
    { id: 'plugins', label: '插件', icon: 'package' },
    { id: 'restore', label: '恢复', icon: 'download' },
    { id: 'settings', label: '设置', icon: 'sliders' },
    { id: 'audit', label: '审计', icon: 'file' },
    { id: 'logs', label: '日志', icon: 'file' },
  ];

  const PAGE_META = {
    dashboard: { title: '概览', desc: '备份状态、任务与目标一览、实时任务进度' },
    tasks: { title: '备份任务', desc: '每个任务 = 源文件夹 + 目标 + 定时 + 保留策略，各自独立增量与快照' },
    targets: { title: '备份目标', desc: '远程存储目的地与账号凭据，一个目标可被多个任务共用' },
    plugins: { title: '插件', desc: '插件能力卡片与外置插件（动态库）管理：开关、目录、签名公钥与诊断' },
    restore: { title: '恢复', desc: '浏览云端备份内容，按文件或目录恢复到原位置' },
    settings: { title: '设置', desc: '加密密钥、通知与配置迁移' },
    audit: { title: '操作审计', desc: '敏感与破坏性操作的本地留痕（audit.log）' },
    logs: { title: '运行日志', desc: '服务端运行日志查看、清空与下载' },
  };

  $: page = PAGE_META[currentPage] || PAGE_META.dashboard;
  $: alertsCount = alerts.length;

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
      // 插件清单：决定「插件」页显示哪些插件卡片、什么顺序
      plugins = await loadPlugins(true);
      // 时间展示统一按宿主（NAS）时区，而不是浏览器时区
      setHostTimezone(d.host_utc_offset_minutes);
      scheduleTimezone = d.schedule_timezone || '';
      pluginsEnabledCfg = !!d.plugins_enabled;
      marketEnabledCfg = !!d.market_enabled;
      pluginsDirCfg = d.plugins_dir || '';
      pluginsPubkeysCfg = d.plugins_pubkeys || [];
      pluginsAllowUnsigned = !!d.plugins_allow_unsigned;
      // 仅用于恢复页的空状态引导；路径本身按任务管理（见「任务」页）
      backupPaths = d.backup_paths || [];
      webhookUrl = d.webhook_url || '';
      webhookHeaders = d.webhook_headers || [];
      webhookBody = d.webhook_body || '';
      keyBackedUp = !!d.key_backed_up;
      debugOn = !!d.debug;
    } catch (e) {
      error = e.message;
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

  // 已见过的最大告警 id：用于「后台新增消息」时即时 toast 一次（消息本身留在「消息提醒」）
  let alertsMaxId = 0;
  let alertsLoadedOnce = false;

  async function loadAlerts() {
    try {
      const d = await api.alerts();
      const list = d.alerts || [];
      const maxId = list.reduce((m, a) => Math.max(m, a.id || 0), 0);
      if (alertsLoadedOnce && maxId > alertsMaxId) {
        const fresh = list.filter((a) => (a.id || 0) > alertsMaxId);
        const last = fresh[fresh.length - 1];
        if (last) {
          const title = fresh.length > 1 ? `消息提醒（${fresh.length} 条）` : '消息提醒';
          if (last.level === 'error') toast.error(last.message, title);
          else toast.warn(last.message, title);
        }
      }
      alerts = list;
      alertsMaxId = maxId;
      alertsLoadedOnce = true;
    } catch (e) {
      error = e.message;
    }
  }

  /**
   * 「只采纳最新一次请求」的序号守卫
   *
   * 多个 load 函数会被并发/连续触发（保存后 `handleTasksChanged` + WS 事件 +
   * 60 秒轮询）。若前一次请求比后一次**更晚返回**（网络抖动、后端偶发慢），
   * 旧数据就会覆盖新数据 —— 表现为「刚保存完，界面又跳回旧值」。
   *
   * 用法：进入时取号，回来时比对；不是最新就丢弃结果。
   */
  const _reqSeq = new Map();
  function seqFor(name) {
    const next = (_reqSeq.get(name) || 0) + 1;
    _reqSeq.set(name, next);
    return next;
  }
  function isLatest(name, seq) {
    return _reqSeq.get(name) === seq;
  }

  /** 多任务/多目标：任务列表 + 可选目标（/api/tasks 一并返回） */
  async function loadTasks() {
    const __seq = seqFor('tasks');
    try {
      const d = await api.tasks();
      if (!isLatest('tasks', __seq)) return; // 有更新的请求在飞：丢弃旧结果
      tasks = d.tasks || [];
      targetOptions = d.targets || [];
    } catch (e) {
      if (isLatest('tasks', __seq)) error = e.message;
    }
  }

  /** 目标详情列表（/api/targets：含地址、账号、被引用任务数） */
  async function loadTargets() {
    const __seq = seqFor('targets');
    try {
      const d = await api.targets();
      if (!isLatest('targets', __seq)) return;
      targets = d.targets || [];
    } catch (e) {
      if (isLatest('targets', __seq)) error = e.message;
    }
  }

  /** 任务或目标变更后：刷新任务、目标、配置（legacy 视图）与恢复列表 */
  async function handleTasksChanged() {
    // **一次性并行拉全部**：此前是「先等 tasks+targets，再等 config，再等 restoreFiles」
    // 三段串行，每段都要等一个来回 —— 保存任务后要等 3 个 RTT 界面才更新。
    // 这四项互不依赖，并发发出即可，感知延迟从 3×RTT 降到 1×RTT。
    // 实时任务卡也要跟着刷新：删掉任务后后端会把「最后一帧」一并丢弃，
    // 不重拉的话卡片会一直挂着那个已不存在任务的上次结果。
    await Promise.all([loadTasks(), loadTargets(), loadConfig(), loadRestoreFiles(), loadLive()]);
  }

  async function loadRestoreFiles() {
    try {
      const d = await api.restoreFiles();
      restoreFolders = d.folders || [];
    } catch (e) {
      error = e.message;
    }
  }

  /**
   * 拉取当前任务状态快照
   *
   * 为什么需要：WS 只推**增量**事件。刷新页面或退出应用再进来时 WebView 重建、
   * WS 重连，之前收到的进度全丢，界面会显示「没有任务在跑」，而后台其实还在跑。
   * 这里在首屏与 WS 重连时各拉一次，把「最后一帧」补回来。
   */
  async function loadLive() {
    try {
      const r = await api.tasksLive();
      // 优先展示备份，其次恢复（同时只会有一个在跑）
      let ev = r.backup || r.restore || null;
      // ⚠️ **事件总线的「最后一帧」可能停在 progress**：任务若是被取消、
      // 或因为重试耗尽而静默结束，未必有终态（completed/failed）事件发出。
      // 后端 `busy` 才是「此刻是否真有任务在跑」的唯一权威来源，
      // 所以不忙时不能把 progress/started 帧当成「还在进行中」渲染，
      // 否则任务早已结束、卡片却永久停在「进行中」（实测踩到）。
      if (ev && !r.busy && (ev.status === 'progress' || ev.status === 'started')) {
        ev = { ...ev, status: 'failed', message: ev.message || '任务已结束（未收到终态事件）' };
      }
      if (ev) {
        liveStatus = {
          kind: ev.kind,
          status: ev.status,
          phase: ev.phase,
          current_file: ev.current_file,
          done: ev.done,
          total: ev.total,
          bytes_done: ev.bytes_done,
          bytes_total: ev.bytes_total,
          elapsed_ms: ev.elapsed_ms,
          speed: ev.speed,
          at: Date.now(),
          message: ev.message,
        };
      }
      busy = !!r.busy;
      cancelRequested = !!r.cancel_requested;
    } catch (e) {
      /* 快照失败不影响主流程 */
    }
  }

  /**
   * 取消当前正在执行的备份/恢复
   *
   * 必须说清「不是立刻停」：后端是**协作式取消**，在当前文件传完后才停
   * （单文件的传输在插件 FFI 里推进，从外部打断会留下半截分片）。
   * 不这样写用户会以为按钮没生效而反复点。
   */
  async function cancelRunning() {
    const kindText = liveStatus && liveStatus.kind === 'restore' ? '恢复' : '备份';
    const ok = await confirmDialog({
      title: `取消${kindText}任务？`,
      message:
        // ⚠️ 确认框按**纯文本**渲染（`{state.message}` 会被转义），
        // 因此这里不能写 Markdown 的 `**` 或 HTML 标签 —— 用户会看到字面量。
        `将在当前文件传输结束后停止（不是立刻中断）。\n` +
        `已完成的文件会保留，不会回退；下次${kindText}会接着处理未完成的文件。`,
      confirmText: '取消任务',
      cancelText: '继续执行',
      danger: true,
    });
    if (!ok) return;
    try {
      const r = await api.cancelTask();
      if (r.success) {
        cancelRequested = true;
        toast.info(r.message || '已请求取消，等待当前文件结束', '正在取消');
      } else {
        toast.error(r.error || '取消失败', '取消任务');
      }
    } catch (e) {
      toast.error(String(e.message || e), '取消任务');
    }
  }

  async function loadAll() {
    loading = true;
    try {
      await Promise.all([
        loadHealth(),
        loadConfig(),
        loadKeys(),
        loadAlerts(),
        loadRestoreFiles(),
        loadTasks(),
        loadTargets(),
        loadLive(),
      ]);
    } catch (e) {
      // 各 load 内部都已 catch；这里兜住意外（例如某个 load 被改动后漏了 catch），
      // 否则 loading 会永远为 true —— 首屏骨架屏卡死、页面永不渲染。
      console.error('[loadAll]', e);
      error = String(e && e.message ? e.message : e);
    } finally {
      loading = false;
    }
  }

  /* ── 页面切换 ─────────────────────────────────────────────── */

  function go(pageId) {
    currentPage = pageId;
    error = null;
    if (pageId === 'restore') loadRestoreFiles();
  }

  /* ── WebSocket 实时状态 ───────────────────────────────────── */

  /** 当前 WS 实例（保留引用以便主动关闭；否则重连定时器会一直续命） */
  let ws = null;
  /** 重连定时器句柄（组件销毁时必须清掉） */
  let wsRetryTimer = null;
  /** 是否已销毁（阻止 close 回调里再排新的重连） */
  let disposed = false;

  function connectWS() {
    if (disposed) return;
    try {
      const proto = location.protocol === 'https:' ? 'wss' : 'ws';
      // 带网关前缀（统一网关下应用在 /app/<appname>，裸 /api 会落到宿主）
      ws = new WebSocket(`${proto}://${location.host}${APP_BASE}/api/ws`);
      ws.onopen = () => {
        wsConnected = true;
        // 重连后补一次快照：断线期间的事件已经丢了
        loadLive();
      };
      ws.onmessage = (evt) => {
        try {
          const data = JSON.parse(evt.data);
          if (data.type !== 'event') return;
          // 插件自报进度（kind === 'plugin'）**不得**占用顶部任务卡：
          // 它既不是备份也不是恢复，若照单全收会把正在进行备份的实时状态顶掉。
          // 这类事件只在浏览器控制台留痕，任务卡继续由宿主的 backup/restore 事件驱动。
          if (data.kind === 'plugin') {
            console.debug('[plugin]', data.job_id, data.message, `${data.done}/${data.total}`);
            return;
          }
          liveStatus = {
            kind: data.kind,
            status: data.status,
            phase: data.phase,
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
        // 组件已销毁（切换/关闭页面）就不要再排重连，否则会留下一个
        // 永不停止的 3 秒定时器链，持续占用连接与内存。
        if (disposed) return;
        if (wsRetryTimer) clearTimeout(wsRetryTimer);
        wsRetryTimer = setTimeout(connectWS, 3000);
      };
      ws.onerror = () => ws.close();
    } catch (e) {
      /* 忽略 */
    }
  }

  /** 保存外置插件开关、目录与签名公钥（重启应用后生效） */
  /**
   * 保存外置插件**总开关**
   *
   * 目录与公钥不再由用户在设置页维护：
   * - 目录固定用默认位置（少一个「填错就静默不加载」的故障点）；
   * - 公钥在**安装插件时**随文件一起登记（一插件一公钥），不在这里批量粘贴。
   */
  /**
   * 保存插件市场配置（目前只用于「开启市场」）
   *
   * 注意：**只在用户显式点击时**保存，不在这里自动拉索引 ——
   * "不在用户未要求时联网"由 MarketPanel 负责（开启后才 `marketCatalog`）。
   */
  async function handleSaveMarket(patch) {
    busy = true;
    error = null;
    try {
      const d = await api.saveConfig(patch);
      if (d.error) {
        error = d.error;
        return { error: d.error };
      }
      marketEnabledCfg = !!d.market_enabled;
      return {};
    } catch (e) {
      return { error: e.message };
    } finally {
      busy = false;
    }
  }

  async function handleSavePlugins(enabled) {
    busy = true;
    error = null;
    try {
      const d = await api.saveConfig({ plugins_enabled: !!enabled });
      if (d.error) {
        error = d.error;
        return { error: d.error };
      }
      pluginsEnabledCfg = !!d.plugins_enabled;
      // 后端已让开关热生效；清单刷新放后台跑，不阻塞按钮。
      // 用户马上能继续操作，列表会在下一帧自动补上。
      handlePluginDone();
      return {};
    } catch (e) {
      error = e.message;
      return { error: e.message };
    } finally {
      busy = false;
    }
  }

  /** 一键体检 */
  async function handleSetupCheck() {
    busy = true;
    error = null;
    try {
      setupResult = await api.setupCheck();
      // 体检结果的展示不依赖提醒列表：**后台刷新**，不阻塞按钮恢复可点。
      // （此前是 `await loadAlerts()` 后才解除 busy，用户点完要等一下才能再点。）
      loadAlerts();
      return setupResult;
    } catch (e) {
      error = e.message;
      toast.error(e.message, '体检失败');
      return null;
    } finally {
      busy = false;
    }
  }

  /**
   * 插件通用区块（UI Schema）完成一次操作后的回调：
   * 刷新消息提醒与配置（插件可用性可能已变化，如 token 已配置/清除）
   */
  async function handlePluginDone() {
    // 同上：四项互不依赖，一次并行发出（原为三段串行）
    await Promise.all([
      loadAlerts(),
      loadConfig(),
      loadTasks(),
      loadTargets(),
    ]);
  }

  /** 切换调试日志：立即生效并持久化 */
  async function handleSaveDebug(enabled) {
    busy = true;
    try {
      const d = await api.saveConfig({ debug: enabled });
      if (d.error) {
        error = d.error;
        return { error: d.error };
      }
      debugOn = !!d.debug;
      return {};
    } catch (e) {
      error = e.message;
      return { error: e.message };
    } finally {
      busy = false;
    }
  }

  /* ── 操作：恢复 ───────────────────────────────────────────── */

  /** 恢复：all=true 时按 sourcePath（可用 dir 限定子目录）全量恢复；task 指定任务 */
  async function handleRestore(files, sourcePath, all = false, dir = '', task = '') {
    return api.restore(files, sourcePath, all, dir, task);
  }

  /** 恢复树懒加载：展开目录时按需拉取一层；task 指定任务 */
  async function handleLoadTree(source, dir, task = '') {
    return api.restoreTree(source, dir, task);
  }

  /** 清理快照中云端已不存在的文件记录；完成后刷新概况 */
  async function handlePruneMissing(sourcePath, task = '') {
    const r = await api.pruneMissing(sourcePath, task);
    if (!r || !r.error) await loadRestoreFiles();
    return r;
  }

  /* ── 操作：密钥 / 通知 / 配置迁移 ─────────────────────────── */

  async function handleSetKey(privateKey) {
    // 更换私钥是**破坏性不可逆**操作（此前全部备份将无法解密），
    // 与「导出私钥」同一把闸门：先验证管理员口令，并明确告知后果。
    const pass = await confirmDialog({
      title: '更换加密私钥？',
      message:
        // 同上：纯文本渲染，不要写 HTML 标签
        '更换后，此前的所有备份都将无法解密（它们是用旧私钥加密的）。\n' +
        '请先确认已妥善保存旧私钥。此操作需要验证管理员口令。',
      confirmText: '验证并更换',
      danger: true,
      input: true,
      placeholder: '管理员口令',
    });
    if (pass === false || !pass) return { success: false, error: '已取消' };
    const d = await api.setKey(privateKey, pass);
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
      // 导入会替换任务/目标/插件配置：全部相关视图一起刷新
      await Promise.all([loadConfig(), loadKeys(), loadRestoreFiles(), loadTasks(), loadTargets()]);
      toast.success('配置已导入并即时生效');
    }
    return d;
  }

  async function clearAlerts() {
    try {
      await api.clearAlerts();
      alerts = [];
      toast.info('消息提醒已清空');
    } catch (e) {
      toast.error(e.message);
    }
  }

  /* ── 启动 ─────────────────────────────────────────────────── */

  /**
   * 启动与清理
   *
   * 之前这些是**模块体顶层语句**：Svelte 会在组件实例化时执行，但**永远不清理**。
   * 后果：
   * - WebSocket 的 `onclose` 里 `setTimeout(connectWS, 3000)` 会一直续命，
   *   组件销毁后仍在重连、仍占着一个长连接；
   * - 60 秒轮询定时器同理，且它每轮都会发一次 HTTP 请求。
   */
  let alertsTimer = null;

  onMount(() => {
    loadAll();
    connectWS();
    // 「消息提醒」轮询：后台巡检（空间预警等）产生的消息不依赖页面操作
    alertsTimer = setInterval(() => loadAlerts(), 60000);
  });

  onDestroy(() => {
    // 标记已销毁：阻止 WS 的 close 回调再排重连
    disposed = true;
    if (wsRetryTimer) clearTimeout(wsRetryTimer);
    if (alertsTimer) clearInterval(alertsTimer);
    try {
      if (ws && ws.readyState <= 1) ws.close();
    } catch (e) {
      /* 忽略 */
    }
    ws = null;
  });
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
      {#if scheduleTimezone}
        <div class="status-pill" title="页面上的时间均按宿主（NAS）时区显示">
          <span class="dot"></span>
          <span>时区 {scheduleTimezone}</span>
        </div>
      {/if}
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
    </header>

    <div class="scroll">
      {#if error}
        <div class="alert alert-danger" role="alert">
          <Icon name="alert" size={17} />
          <div class="alert-body">{error}</div>
          <button class="btn-icon btn-sm" on:click={() => (error = null)} aria-label="关闭">
            <Icon name="x" size={15} />
          </button>
        </div>
      {/if}

      <MessagesPanel {alerts} onClear={clearAlerts} />

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
              {tasks}
              {targets}
              {restoreFolders}
              {keyBackedUp}
              {setupResult}
              {busy}
              onSetupCheck={handleSetupCheck}
              onGoto={go}
            />
          {:else if currentPage === 'tasks'}
            <TasksPage
              {tasks}
              {targetOptions}
              {busy}
              onChanged={handleTasksChanged}
            />
          {:else if currentPage === 'targets'}
            <TargetsPage {targets} {plugins} {busy} onChanged={handleTasksChanged} />
          {:else if currentPage === 'plugins'}
            <PluginsPage
              {plugins}
              onPluginDone={handlePluginDone}
              pluginsEnabled={pluginsEnabledCfg}
              pluginsAllowUnsigned={pluginsAllowUnsigned}
              onSavePlugins={handleSavePlugins}
              marketEnabled={marketEnabledCfg}
              onSaveMarket={handleSaveMarket}
              {busy}
            />
          {:else if currentPage === 'restore'}
            <RestorePage
              {restoreFolders}
              {backupPaths}
              {busy}
              onRestore={handleRestore}
              onLoadTree={handleLoadTree}
              onPrune={handlePruneMissing}
              onGoto={go}
            />
          {:else if currentPage === 'audit'}
            <AuditPage {busy} />
          {:else if currentPage === 'logs'}
            <LogsPage debug={debugOn} onSaveDebug={handleSaveDebug} />
          {:else if currentPage === 'settings'}
            <SettingsPage
              {busy}
              {keyInfo}
              {revealKey}
              {keyBackedUp}
              {webhookUrl}
              {webhookHeaders}
              {webhookBody}
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

        <aside class="rail">
          <LiveStatus {liveStatus} {wsConnected} onCancel={cancelRunning} {cancelRequested} />
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
    /* 账号卡与实时任务卡之间留出间距（此前两块卡片紧贴，显得很挤） */
    display: flex;
    flex-direction: column;
    gap: var(--s4);
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
