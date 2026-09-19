<script>
  // 恢复：按备份文件夹浏览快照目录树，支持单文件 / 整目录恢复
  import TreeNode from '../TreeNode.svelte';
  import Icon from './Icon.svelte';
  import { fmtBytes } from '../lib/format.js';

  export let restoreFolders = [];
  export let backupPaths = [];
  export let busy = false;
  export let onRestore = null; // (files, sourcePath) => Promise<{restored, restored_bytes, error}>
  export let onGoto = null; // (pageId) => void

  $: trees = restoreFolders.map((f) => buildTree(f.files));
  let expandedSet = new Set();
  let expandedFolderIdx = null;
  let restoreMsg = '';
  let restoreOk = true;
  let restoreResult = null;
  let working = false;

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
          node[part] = {
            rel_path: cur,
            name: part,
            size: f.size,
            is_dir: f.is_dir,
            children: f.is_dir ? {} : null,
          };
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

  function toggleFolder(i) {
    expandedFolderIdx = expandedFolderIdx === i ? null : i;
  }

  function toggleDir(relPath) {
    if (expandedSet.has(relPath)) {
      expandedSet = new Set([...expandedSet].filter((p) => p !== relPath));
    } else {
      expandedSet = new Set([...expandedSet, relPath]);
    }
  }

  function restoreOne(relPath, sourcePath) {
    restoreFiles([relPath], relPath, sourcePath);
  }

  async function restoreFiles(files, label, sourcePath) {
    if (!files || files.length === 0) {
      restoreMsg = '该目录没有可恢复的文件';
      restoreOk = false;
      return;
    }
    restoreMsg = '';
    restoreResult = null;
    working = true;
    try {
      const data = await onRestore(files, sourcePath);
      restoreResult = data;
      if (data.error) {
        restoreMsg = `恢复失败：${data.error}`;
        restoreOk = false;
      } else {
        restoreMsg = `已恢复 ${data.restored} 个文件到 ${sourcePath}`;
        restoreOk = true;
      }
    } catch (e) {
      restoreMsg = `恢复失败：${e.message}`;
      restoreOk = false;
    } finally {
      working = false;
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="download" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">选择要恢复的内容</h2>
      <p class="card-desc">展开文件夹勾选文件或整个目录，恢复到原备份位置（覆盖同名文件）</p>
    </div>
    <span class="badge">{restoreFolders.length} 个文件夹</span>
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
        {#each restoreFolders as folder, i (folder.path)}
          <div class="folder" class:open={expandedFolderIdx === i}>
            <button class="folder-head" on:click={() => toggleFolder(i)}>
              <Icon
                name={expandedFolderIdx === i ? 'chevron_down' : 'chevron_right'}
                size={15}
              />
              <Icon name="folder" size={15} />
              <span class="folder-name mono">{folder.path}</span>
              {#if folder.has_backup}
                <span class="badge badge-info">{folder.files.length} 个文件</span>
              {:else}
                <span class="badge badge-warn">未备份</span>
              {/if}
            </button>

            {#if expandedFolderIdx === i}
              <div class="tree">
                {#each Object.values(trees[i] || {}) as node (node.rel_path)}
                  <TreeNode
                    {node}
                    {expandedSet}
                    busy={busy || working}
                    onToggleDir={toggleDir}
                    onRestore={(relPath) => restoreOne(relPath, folder.path)}
                    onRestoreDir={(files) => restoreFiles(files, folder.path, folder.path)}
                  />
                {/each}
                {#if !trees[i] || Object.keys(trees[i]).length === 0}
                  <div class="empty slim">
                    <div class="icon-wrap"><Icon name="file" size={17} /></div>
                    该文件夹暂无备份文件
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>

  {#if restoreMsg}
    <div class="card-foot">
      <div class="alert {restoreOk ? 'alert-ok' : 'alert-danger'}">
        <Icon name={restoreOk ? 'check-circle' : 'x-circle'} size={15} />
        <div class="alert-body">{restoreMsg}</div>
      </div>
    </div>
  {/if}

  {#if restoreResult && !restoreResult.error}
    <div class="card-foot">
      <div class="stat-grid">
        <div class="stat">
          <div class="stat-label"><Icon name="check" size={12} />恢复文件</div>
          <div class="stat-value">{restoreResult.restored}</div>
        </div>
        <div class="stat">
          <div class="stat-label"><Icon name="hard_drive" size={12} />恢复字节</div>
          <div class="stat-value">{fmtBytes(restoreResult.restored_bytes)}</div>
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
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 11px 12px;
    background: transparent;
    border: none;
    color: var(--text-2);
    font-family: inherit;
    font-size: 13px;
    cursor: pointer;
    text-align: left;
    transition: background var(--t-fast);
  }
  .folder-head:hover {
    background: var(--surface-3);
  }
  .folder-name {
    flex: 1;
    min-width: 0;
    color: var(--text);
    font-size: 12.5px;
    font-weight: 550;
    word-break: break-all;
  }
  .tree {
    padding: 6px 8px 10px;
    border-top: 1px solid var(--border);
  }
  .slim {
    padding: var(--s4);
  }
</style>
