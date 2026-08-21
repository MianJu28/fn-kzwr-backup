<script>
  import TreeNode from './TreeNode.svelte';

  let health = '检查中...';
  let error = null;
  let busy = false;

  // kzwr 登录
  let username = '';
  let password = '';
  let loggedIn = false;
  let loginMsg = '';

  // 备份配置
  let backupPaths = [];
  let pathInput = '';
  let targetFolder = 'fn-backup';
  // 定时备份 cron 配置
  let scheduleCron = '';
  let scheduleCronValid = true;
  const cronPresets = [
    { label: '每天 00:00', value: '0 0 * * *' },
    { label: '每天 06:00', value: '0 6 * * *' },
    { label: '每天 23:00', value: '0 23 * * *' },
    { label: '每小时整点', value: '0 * * * *' },
    { label: '每 12 小时', value: '0 */12 * * *' },
    { label: '每周一 02:00', value: '0 2 * * 1' },
  ];

  // 备份/恢复
  let backupResult = null;
  let restoreResult = null;
  let restoreMsg = '';
  // 可恢复文件（从 SQLite 查询）
  let restoreFolders = [];
  let restoreTrees = [];
  // 展开的目录路径集合（Set）
  let expandedSet = new Set();
  let expandedFolderIdx = null;
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

  // 加载可恢复文件列表（从 SQLite 快照查询），并构建目录树
  async function loadRestoreFiles() {
    try {
      const res = await fetch('/api/restore/files');
      const data = await res.json();
      restoreFolders = data.folders || [];
      restoreTrees = restoreFolders.map((f) => buildTree(f.files));
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    }
  }

  // 把扁平的快照文件列表构造成目录树
  function buildTree(files) {
    const root = {};
    for (const f of files || []) {
      const parts = f.rel_path.split('/');
      let node = root;
      let cur = '';
      for (let i = 0; i < parts.length; i++) {
        const part = parts[i];
        cur = cur ? `${cur}/${part}` : part;
        if (i === parts.length - 1) {
          node[part] = { rel_path: cur, name: part, size: f.size, is_dir: f.is_dir, children: f.is_dir ? {} : null };
        } else {
          if (!node[part]) {
            node[part] = { rel_path: cur, name: part, size: 0, is_dir: true, children: {} };
          }
          node = node[part].children;
        }
      }
    }
    return root;
  }

  // 恢复单个文件（恢复到备份源路径）
  async function restoreOne(relPath, sourcePath) {
    await restoreFiles([relPath], relPath, sourcePath);
  }

  // 恢复文件列表（files 为相对路径数组，sourcePath 为恢复目标根=备份源路径）
  async function restoreFiles(files, label, sourcePath) {
    if (!files || files.length === 0) {
      restoreMsg = '该目录没有可恢复的文件';
      return;
    }
    busy = true;
    error = null;
    restoreMsg = '';
    restoreResult = null;
    try {
      // 传 source_path（备份源路径），恢复到原位置
      const body = { files, source_path: sourcePath };
      const res = await fetch('/api/restore/run', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      const data = await res.json();
      restoreResult = data;
      restoreMsg = data.error
        ? `恢复失败: ${data.error}`
        : `已恢复 ${data.restored} 个文件到 ${sourcePath}: ${label}`;
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  function toggleFolder(i) {
    expandedFolderIdx = expandedFolderIdx === i ? null : i;
  }

  // 展开/折叠目录节点
  function toggleDir(relPath) {
    if (expandedSet.has(relPath)) {
      expandedSet = new Set([...expandedSet].filter((p) => p !== relPath));
    } else {
      expandedSet = new Set([...expandedSet, relPath]);
    }
  }

  function statusText(s) {
    const map = {
      started: '开始',
      progress: '进行中',
      completed: '完成',
      failed: '失败',
    };
    return map[s] || s;
  }

  async function login() {
    busy = true;
    error = null;
    loginMsg = '';
    try {
      const res = await fetch('/api/auth/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      });
      const data = await res.json();
      if (data.success) {
        loggedIn = true;
        loginMsg = `已登录: ${data.username}`;
        password = '';
      } else {
        loginMsg = `登录失败: ${data.error}`;
      }
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  function addPath() {
    const p = pathInput.trim();
    if (p && !backupPaths.includes(p)) {
      backupPaths = [...backupPaths, p];
      pathInput = '';
    }
  }

  function removePath(i) {
    backupPaths = backupPaths.filter((_, idx) => idx !== i);
  }

  async function saveConfig() {
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

  function applyCronPreset(val) {
    scheduleCron = val;
    scheduleCronValid = true;
  }

  async function runBackup() {
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
  {#if liveStatus}
    <section class="live-status">
      <h2>{liveStatus.kind === 'backup' ? '⬆️' : '⬇️'} 实时任务</h2>
      <div class="ls-row">
        <span class="ls-label">状态</span>
        <span class="ls-value">{statusText(liveStatus.status)}</span>
      </div>
      {#if liveStatus.current_file}
        <div class="ls-row">
          <span class="ls-label">当前文件</span>
          <span class="ls-value file">{liveStatus.current_file}</span>
        </div>
      {/if}
      {#if liveStatus.total > 0}
        <div class="ls-row">
          <span class="ls-label">进度</span>
          <span class="ls-value">{liveStatus.done} / {liveStatus.total}</span>
        </div>
        <div class="progress-bar">
          <div class="progress-fill" style="width: {liveStatus.total ? (liveStatus.done / liveStatus.total * 100) : 0}%"></div>
        </div>
      {/if}
      {#if liveStatus.message}
        <div class="ls-row">
          <span class="ls-label">信息</span>
          <span class="ls-value">{liveStatus.message}</span>
        </div>
      {/if}
    </section>
  {/if}

  <!-- 当前配置概览（从服务器读取） -->
  <section class="overview">
    <h2>📋 当前配置</h2>
    <div class="ov-row">
      <span class="ov-label">备份路径</span>
      <span class="ov-value">{backupPaths.length > 0 ? backupPaths.join('、') : '未配置'}</span>
    </div>
    <div class="ov-row">
      <span class="ov-label">目标文件夹</span>
      <span class="ov-value">{targetFolder || '未配置'}</span>
    </div>
    <div class="ov-row">
      <span class="ov-label">kzwr 登录</span>
      <span class="ov-value">{loggedIn ? '✅ 已登录' : '⚠️ 未登录'}</span>
    </div>
    <div class="ov-row">
      <span class="ov-label">定时备份</span>
      <span class="ov-value">
        {scheduleCron ? (scheduleCronValid ? `⏰ ${scheduleCron}` : '⚠️ cron 无效') : '未启用'}
      </span>
    </div>
    <div class="ov-row">
      <span class="ov-label">可恢复文件</span>
      <span class="ov-value">{restoreFolders.length > 0 ? `${restoreFolders.length} 个文件夹` : '未配置'}</span>
    </div>
  </section>

  <!-- kzwr 登录 -->
  <section>
    <h2>🔑 kzwr 登录</h2>
    <p class="hint">登录酷族网软（kzwr.com），凭据加密存储，token 过期自动重新登录。</p>
    {#if loggedIn}
      <p class="ok">✅ 已登录</p>
    {:else}
      <p class="warn">⚠️ 未登录，请填写凭据</p>
    {/if}
    <label>用户名（邮箱）
      <input bind:value={username} type="email" placeholder="you@example.com" />
    </label>
    <label>密码
      <input bind:value={password} type="password" placeholder="••••••••" />
    </label>
    <button on:click={login} disabled={busy || !username || !password}>
      {busy ? '登录中...' : (loggedIn ? '更新凭据' : '登录 kzwr')}
    </button>
    {#if loginMsg}
      <p class:ok={loggedIn} class:warn={!loggedIn}>{loginMsg}</p>
    {/if}
  </section>

  <!-- 备份路径配置 -->
  <section>
    <h2>📁 备份路径配置</h2>
    <p class="hint">设置要备份的文件夹路径，支持多个。</p>
    <label>目标文件夹
      <input bind:value={targetFolder} placeholder="fn-backup" />
    </label>
    <div class="path-add">
      <input bind:value={pathInput} placeholder="/volume1/data" />
      <button on:click={addPath} disabled={busy || !pathInput.trim()}>添加</button>
    </div>
    <ul class="paths">
      {#each backupPaths as p, i (p)}
        <li>
          <span>{p}</span>
          <button class="remove" on:click={() => removePath(i)}>✕</button>
        </li>
      {/each}
    </ul>

    <div class="cron-block">
      <h3>⏰ 定时备份</h3>
      <p class="hint">设置 cron 表达式定时自动触发备份。留空关闭定时备份。标准 5 段格式：<code>分 时 日 月 周</code>。</p>
      <div class="cron-presets">
        {#each cronPresets as preset}
          <button class="chip" type="button" on:click={() => applyCronPreset(preset.value)}>{preset.label}</button>
        {/each}
        <button class="chip off" type="button" on:click={() => applyCronPreset('')}>关闭定时</button>
      </div>
      <label>cron 表达式
        <input
          bind:value={scheduleCron}
          placeholder="0 0 * * *  (每天零点)"
          class:invalid={!scheduleCronValid && scheduleCron.trim() !== ''}
        />
      </label>
      {#if !scheduleCronValid && scheduleCron.trim() !== ''}
        <p class="warn">⚠️ cron 表达式无效，请检查格式（分 时 日 月 周）</p>
      {/if}
      <p class="example">示例：<code>0 */12 * * *</code> 每 12 小时 · <code>0 2 * * 1</code> 每周一 02:00</p>
    </div>

    <button on:click={saveConfig} disabled={busy}>
      {busy ? '保存中...' : '保存配置'}
    </button>
  </section>

  <!-- 备份 -->
  <section>
    <h2>⬆️ 备份</h2>
    <p class="hint">将配置的文件夹增量加密备份至 kzwr。</p>
    <button on:click={runBackup} disabled={busy}>
      {busy ? '执行中...' : '立即备份'}
    </button>
    {#if backupResult && !backupResult.error}
      <div class="result">
        <p>✅ 备份完成</p>
        <ul>
          <li>上传文件：{backupResult.uploaded}</li>
          <li>上传字节：{backupResult.uploaded_bytes}</li>
          <li>删除：{backupResult.deleted}</li>
          <li>未变化：{backupResult.unchanged}</li>
        </ul>
      </div>
    {/if}
  </section>

  <!-- 恢复 -->
  <section>
    <h2>⬇️ 恢复</h2>
    <p class="hint">展开文件夹选择要恢复的文件，恢复到默认目录。</p>

    <!-- 配置的备份文件夹 + 目录树（来自 SQLite 快照） -->
    {#if restoreFolders.length === 0}
      <p class="warn">尚未配置备份路径或没有备份数据</p>
    {:else}
      <div class="folders">
        {#each restoreFolders as folder, i (folder.path)}
          <div class="folder">
            <button class="folder-head" on:click={() => toggleFolder(i)}>
              <span class="folder-icon">{expandedFolderIdx === i ? '▾' : '▸'}</span>
              <span class="folder-name">📁 {folder.path}</span>
              {#if folder.has_backup}
                <span class="badge">{folder.files.length} 个文件</span>
              {:else}
                <span class="badge warn">未备份</span>
              {/if}
            </button>
            {#if expandedFolderIdx === i}
              <div class="tree-root">
                {#each Object.values(restoreTrees[i] || {}) as node (node.rel_path)}
                  <TreeNode
                    node={node}
                    expandedSet={expandedSet}
                    busy={busy}
                    onToggleDir={toggleDir}
                    onRestore={(relPath) => restoreOne(relPath, folder.path)}
                    onRestoreDir={(files) => restoreFiles(files, folder.path, folder.path)}
                  />
                {/each}
                {#if !restoreTrees[i] || Object.keys(restoreTrees[i]).length === 0}
                  <p class="empty">该文件夹暂无备份文件</p>
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    {#if restoreMsg}
      <p class:ok={!restoreResult || !restoreResult.error} class:warn={restoreResult?.error}>{restoreMsg}</p>
    {/if}
    {#if restoreResult && !restoreResult.error}
      <div class="result">
        <p>✅ 恢复完成</p>
        <ul>
          <li>恢复文件：{restoreResult.restored}</li>
          <li>恢复字节：{restoreResult.restored_bytes}</li>
        </ul>
      </div>
    {/if}
  </section>

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
  section {
    background: #fff;
    border-radius: 10px;
    padding: 20px;
    margin-top: 16px;
    box-shadow: 0 1px 3px rgba(0,0,0,.06);
  }
  h2 { margin: 0 0 8px; font-size: 18px; }
  .hint { color: #5a6a7a; font-size: 13px; margin: 0 0 10px; }
  .ok { color: #22a06b; }
  .warn { color: #b45309; }
  .live-status { background: #f0fdf4; border: 1px solid #bbf7d0; }
  .ls-row {
    display: flex;
    gap: 12px;
    padding: 7px 0;
    font-size: 14px;
    border-bottom: 1px solid #e9f9ef;
  }
  .ls-row:last-child { border-bottom: none; }
  .ls-label { color: #42526e; font-weight: 600; width: 90px; flex-shrink: 0; }
  .ls-value { color: #1f2d3d; word-break: break-all; }
  .ls-value.file { font-family: monospace; }
  .progress-bar {
    height: 8px;
    background: #e5e7eb;
    border-radius: 4px;
    overflow: hidden;
    margin: 8px 0;
  }
  .progress-fill {
    height: 100%;
    background: #22c55e;
    transition: width 0.3s;
  }
  .overview { background: #f0f7ff; border: 1px solid #cfe4ff; }
  .ov-row {
    display: flex;
    gap: 12px;
    padding: 7px 0;
    font-size: 14px;
    border-bottom: 1px solid #e2efff;
  }
  .ov-row:last-child { border-bottom: none; }
  .ov-label { color: #42526e; font-weight: 600; width: 110px; flex-shrink: 0; }
  .ov-value { color: #1f2d3d; word-break: break-all; }
  button {
    background: #2563eb;
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 10px 20px;
    font-size: 15px;
    cursor: pointer;
    margin-top: 12px;
  }
  button:disabled { background: #9db4e8; cursor: not-allowed; }
  button.remove { background: transparent; color: #b91c1c; padding: 2px 8px; margin: 0; }
  label { display: block; margin: 12px 0 4px; font-size: 14px; color: #42526e; }
  input, textarea {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    margin-top: 4px;
    font-size: 14px;
    box-sizing: border-box;
  }
  .radio { display: inline-flex; align-items: center; gap: 6px; margin-right: 16px; }
  .radio input { width: auto; }
  .path-add { display: flex; gap: 8px; margin-top: 8px; }
  .path-add input { flex: 1; }
  .path-add button { margin: 0; white-space: nowrap; }
  .paths { list-style: none; padding: 0; margin: 8px 0 0; }
  .paths li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px 10px;
    background: #f8fafc;
    border-radius: 6px;
    margin-top: 6px;
    font-family: monospace;
    font-size: 13px;
  }
  .cron-block { margin-top: 16px; padding-top: 14px; border-top: 1px solid #eef1f6; }
  .cron-block h3 { margin: 0 0 6px; font-size: 15px; }
  .cron-block code { background: #f1f5f9; padding: 2px 6px; border-radius: 4px; font-size: 12px; }
  .cron-presets { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 2px; }
  .chip {
    background: #f1f5f9;
    color: #334155;
    border: 1px solid #e2e8f0;
    border-radius: 20px;
    padding: 6px 12px;
    margin: 0;
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
  }
  .chip:hover { background: #e0e7ff; color: #2563eb; border-color: #a5b4fc; }
  .chip.off { color: #b91c1c; }
  input.invalid { border-color: #dc2626; background: #fef2f2; }
  .example { color: #64748b; font-size: 12px; margin: 8px 0 0; }
  .result { margin-top: 14px; padding: 12px; background: #ecfdf3; border-radius: 6px; }
  .result p { margin: 0 0 6px; font-weight: 600; }
  .result ul { margin: 0; padding-left: 20px; }
  .error { background: #fef2f2; color: #b91c1c; padding: 12px; border-radius: 6px; margin-top: 12px; }
  footer { text-align: center; color: #8a94a6; font-size: 13px; margin-top: 28px; }

  /* 恢复文件列表 */
  .folders { margin-top: 12px; }
  .folder {
    border: 1px solid #e0e4ea;
    border-radius: 8px;
    margin-top: 8px;
    overflow: hidden;
  }
  .folder-head {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    background: #f8fafc;
    color: #1f2d3d;
    padding: 10px 12px;
    margin: 0;
    border: none;
    border-radius: 0;
    cursor: pointer;
    text-align: left;
  }
  .folder-icon { color: #42526e; font-size: 14px; }
  .folder-name {
    flex: 1;
    min-width: 0;
    color: #1f2d3d;
    font-size: 14px;
    font-weight: 500;
    word-break: break-all;
    line-height: 1.4;
  }
  .badge {
    flex-shrink: 0;
    background: #e6f4ff;
    color: #2563eb;
    border-radius: 12px;
    padding: 2px 10px;
    font-size: 12px;
    white-space: nowrap;
  }
  .badge.warn { background: #fef3c7; color: #b45309; }
  .tree-root { padding: 4px 8px; border-top: 1px solid #eef1f6; }
  .empty { color: #8a94a6; font-style: italic; padding: 10px 12px; }
</style>
