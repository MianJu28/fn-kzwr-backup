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
        body: JSON.stringify({ backup_paths: backupPaths, target_folder: targetFolder }),
      });
      const data = await res.json();
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
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
</script>

<main>
  <header>
    <h1>🛡️ fnos 增量加密备份</h1>
    <p class="health" class:ok={health.startsWith('服务正常')}>{health}</p>
  </header>

  {#if error}
    <div class="error">⚠️ {error}</div>
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
