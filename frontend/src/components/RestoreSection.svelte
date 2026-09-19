<script>
  // 恢复：文件夹概况（文件数/文件夹数/大小）+ 按目录懒加载的目录树
  // 支持「全部恢复」（所有已备份文件夹）、「恢复」单个文件夹 / 目录 / 文件
  import TreeNode from '../TreeNode.svelte';
  import Icon from './Icon.svelte';
  import { fmtBytes } from '../lib/format.js';

  export let restoreFolders = [];
  export let backupPaths = [];
  export let busy = false;
  export let onRestore = null; // (files, sourcePath, all, dir) => Promise<{restored, restored_bytes, error}>
  export let onLoadTree = null; // (source, dir) => Promise<{nodes, error}>
  export let onGoto = null; // (pageId) => void

  /**
   * 每个备份文件夹一份 UI 状态：
   * path -> { open, children: { [rel]: nodes[] | 'loading' }, expanded: { [rel]: true } }
   */
  let states = {};
  let working = false;
  let msg = '';
  let msgOk = true;
  let result = null;

  $: backedUp = restoreFolders.filter((f) => f.has_backup);
  $: totalFiles = backedUp.reduce((s, f) => s + (f.file_count || 0), 0);
  $: totalDirs = backedUp.reduce((s, f) => s + (f.dir_count || 0), 0);
  $: busyAll = working || busy;

  const EMPTY = { open: false, children: {}, expanded: {} };
  const stateOf = (path) => states[path] || EMPTY;

  function patch(path, changes) {
    states = { ...states, [path]: { ...stateOf(path), ...changes } };
  }

  /** 懒加载某目录的直接子项（'' = 文件夹根层） */
  async function loadDir(folderPath, dirRel) {
    const st = stateOf(folderPath);
    const cached = st.children[dirRel];
    if (Array.isArray(cached) || cached === 'loading') return;

    patch(folderPath, { children: { ...st.children, [dirRel]: 'loading' } });
    try {
      const res = onLoadTree ? await onLoadTree(folderPath, dirRel) : { nodes: [] };
      if (res && res.error) notify(res.error, false);
      patch(folderPath, {
        children: { ...stateOf(folderPath).children, [dirRel]: (res && res.nodes) || [] },
      });
    } catch (e) {
      notify(`加载目录失败：${e.message}`, false);
      patch(folderPath, { children: { ...stateOf(folderPath).children, [dirRel]: [] } });
    }
  }

  function toggleFolder(path) {
    const st = stateOf(path);
    const open = !st.open;
    patch(path, { open });
    if (open) loadDir(path, '');
  }

  function toggleDir(folderPath, node) {
    const st = stateOf(folderPath);
    const isOpen = !!st.expanded[node.rel_path];
    patch(folderPath, {
      expanded: isOpen
        ? Object.fromEntries(Object.entries(st.expanded).filter(([k]) => k !== node.rel_path))
        : { ...st.expanded, [node.rel_path]: true },
    });
    if (!isOpen) loadDir(folderPath, node.rel_path);
  }

  /** 重新拉取某文件夹（恢复后快照已回写，计数可能变化） */
  async function refreshFolder(folderPath) {
    const st = stateOf(folderPath);
    const children = { ...st.children };
    delete children[''];
    patch(folderPath, { children, expanded: {} });
    if (st.open) await loadDir(folderPath, '');
  }

  function notify(text, ok) {
    msg = text;
    msgOk = ok;
  }

  async function runRestore(files, sourcePath, all, dir, label) {
    working = true;
    msg = '';
    result = null;
    try {
      const r = await onRestore(files, sourcePath, all, dir);
      result = r;
      if (r && r.error) notify(`恢复失败：${r.error}`, false);
      else notify(`${label}恢复完成：${r.restored} 个文件（${fmtBytes(r.restored_bytes)}）`, true);
    } catch (e) {
      notify(`恢复失败：${e.message}`, false);
    } finally {
      working = false;
    }
  }

  const restoreFile = (folderPath, node) =>
    runRestore([node.rel_path], folderPath, false, '', `文件 ${node.name}：`);

  const restoreDir = (folderPath, node) =>
    runRestore(null, folderPath, true, node.rel_path, `目录 ${node.name}：`);

  const restoreFolder = (folder) =>
    runRestore(null, folder.path, true, '', `文件夹 ${folder.path} `);

  /** 全部恢复：依次恢复所有「已备份」的文件夹 */
  async function restoreAll() {
    if (!backedUp.length) return;
    working = true;
    msg = '';
    result = null;
    let restored = 0;
    let bytes = 0;
    let done = 0;
    const failed = [];
    for (const f of backedUp) {
      try {
        const r = await onRestore(null, f.path, true, '');
        if (r && r.error) failed.push(`${f.path}：${r.error}`);
        else {
          restored += r.restored || 0;
          bytes += r.restored_bytes || 0;
          done++;
        }
      } catch (e) {
        failed.push(`${f.path}：${e.message}`);
      }
    }
    working = false;
    result = { restored, restored_bytes: bytes };
    if (failed.length) {
      notify(`已恢复 ${done} 个文件夹；以下失败：${failed.join('；')}`, false);
    } else {
      notify(`全部恢复完成：${done} 个文件夹、${restored} 个文件（${fmtBytes(bytes)}）`, true);
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="download" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">选择要恢复的内容</h2>
      <p class="card-desc">
        {#if backedUp.length}
          共 {backedUp.length} 个已备份文件夹 · {totalFiles} 个文件 · {totalDirs} 个文件夹；
          展开目录按需加载，恢复到原备份位置（覆盖同名文件）
        {:else}
          展开文件夹选择要恢复的文件，恢复到原备份位置
        {/if}
      </p>
    </div>
    {#if backedUp.length}
      <button class="btn btn-primary nowrap" on:click={restoreAll} disabled={busyAll}>
        {#if working}
          <span class="spin"></span>恢复中…
        {:else}
          <Icon name="download" size={15} />全部恢复
        {/if}
      </button>
    {/if}
  </div>

  <div class="card-body">
    {#if restoreFolders.length === 0}
      <div class="empty">
        <div class="icon-wrap"><Icon name="database" size={19} /></div>
        {#if backupPaths.length === 0}
          <strong>尚未配置备份路径</strong>
          请先在「备份」页添加要备份的文件夹
          <button class="btn btn-sm btn-soft cta" on:click={() => onGoto && onGoto('backup')}>
            前往备份页<Icon name="arrow-right" size={13} />
          </button>
        {:else}
          <strong>暂无备份数据</strong>
          请先在「备份」页执行一次备份
          <button class="btn btn-sm btn-soft cta" on:click={() => onGoto && onGoto('backup')}>
            前往备份页<Icon name="arrow-right" size={13} />
          </button>
        {/if}
      </div>
    {:else}
      <div class="folders">
        {#each restoreFolders as folder (folder.path)}
          <!-- 注意：必须在此直接引用 `states`，否则 Svelte 编译器分析不到依赖，展开后不会重渲染 -->
          {@const st = states[folder.path] || EMPTY}
          <div class="folder" class:open={st.open}>
            <div class="folder-head">
              <button
                class="toggle"
                on:click={() => toggleFolder(folder.path)}
                disabled={!folder.has_backup}
                aria-expanded={st.open}
                aria-label={st.open ? '收起' : '展开'}
              >
                <Icon
                  name={st.open ? 'chevron_down' : 'chevron_right'}
                  size={15}
                />
              </button>
              <Icon name="folder" size={15} />
              <span class="folder-name mono">{folder.path}</span>

              {#if folder.has_backup}
                <span class="badge badge-info nowrap">{folder.file_count} 文件</span>
                <span class="badge nowrap">{folder.dir_count} 文件夹</span>
                <span class="size nowrap">{fmtBytes(folder.total_bytes)}</span>
                <button
                  class="btn btn-sm btn-soft"
                  on:click={() => restoreFolder(folder)}
                  disabled={busyAll}
                >
                  <Icon name="download" size={13} />恢复
                </button>
              {:else}
                <span class="badge badge-warn">未备份</span>
              {/if}
            </div>

            {#if st.open}
              <div class="tree">
                {#if st.children[''] === 'loading'}
                  <div class="tree-state"><span class="spin"></span>读取备份目录…</div>
                {:else if !(st.children[''] || []).length}
                  <div class="tree-state">该文件夹暂无备份文件</div>
                {:else}
                  {#each st.children[''] as node (node.rel_path)}
                    <TreeNode
                      {node}
                      expanded={st.expanded}
                      cache={st.children}
                      busy={busyAll}
                      onToggle={(n) => toggleDir(folder.path, n)}
                      onRestoreFile={(n) => restoreFile(folder.path, n)}
                      onRestoreDir={(n) => restoreDir(folder.path, n)}
                    />
                  {/each}
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>

  {#if msg}
    <div class="card-foot">
      <div class="alert {msgOk ? 'alert-ok' : 'alert-danger'}">
        <Icon name={msgOk ? 'check-circle' : 'x-circle'} size={15} />
        <div class="alert-body">{msg}</div>
      </div>
    </div>
  {/if}

  {#if result && !result.error}
    <div class="card-foot">
      <div class="stat-grid">
        <div class="stat">
          <div class="stat-label"><Icon name="check" size={12} />恢复文件</div>
          <div class="stat-value">{result.restored}</div>
        </div>
        <div class="stat">
          <div class="stat-label"><Icon name="hard_drive" size={12} />恢复字节</div>
          <div class="stat-value">{fmtBytes(result.restored_bytes)}</div>
        </div>
      </div>
    </div>
  {/if}
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .cta {
    margin-top: var(--s3);
  }
  .folders {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .folder {
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    overflow: hidden;
    background: var(--surface-2);
    transition: border-color var(--t-fast);
  }
  .folder.open {
    border-color: var(--primary-soft-border);
    background: var(--surface);
  }
  .folder-head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    padding: 10px 12px;
  }
  .toggle {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border: none;
    background: transparent;
    color: var(--text-3);
    border-radius: var(--r-xs);
    cursor: pointer;
    flex-shrink: 0;
  }
  .toggle:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--text);
  }
  .toggle:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .folder-name {
    flex: 1;
    min-width: 140px;
    color: var(--text);
    font-size: 12.5px;
    font-weight: 550;
    word-break: break-all;
  }
  .size {
    color: var(--text-3);
    font-size: 11.5px;
    font-family: var(--mono);
  }
  .tree {
    padding: 6px 8px 10px;
    border-top: 1px solid var(--border);
  }
  .tree-state {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 8px 10px;
    color: var(--text-3);
    font-size: 12.5px;
  }
  .spin {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid currentColor;
    border-right-color: transparent;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
