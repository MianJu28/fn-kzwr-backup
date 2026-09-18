<script>
  import TreeNode from '../TreeNode.svelte';

  // 恢复
  export let restoreFolders = [];
  export let backupPaths = [];
  export let busy = false;
  // onRestore(files: string[], label: string, sourcePath: string) => Promise<{restored, restored_bytes, error}>
  export let onRestore = null;

  // 目录树（由扁平的快照文件列表构建）
  $: restoreTrees = restoreFolders.map((f) => buildTree(f.files));
  // 展开的目录路径集合（Set）
  let expandedSet = new Set();
  let expandedFolderIdx = null;
  let restoreMsg = '';
  let restoreResult = null;

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

  // 恢复单个文件（恢复到备份源路径）
  function restoreOne(relPath, sourcePath) {
    restoreFiles([relPath], relPath, sourcePath);
  }

  // 恢复文件列表（files 为相对路径数组，sourcePath 为恢复目标根=备份源路径）
  async function restoreFiles(files, label, sourcePath) {
    if (!files || files.length === 0) {
      restoreMsg = '该目录没有可恢复的文件';
      return;
    }
    restoreMsg = '';
    restoreResult = null;
    try {
      const data = await onRestore(files, sourcePath);
      restoreResult = data;
      restoreMsg = data.error
        ? `恢复失败: ${data.error}`
        : `已恢复 ${data.restored} 个文件到 ${sourcePath}: ${label}`;
    } catch (e) {
      restoreMsg = `恢复失败: ${e.message}`;
    }
  }
</script>

<section>
  <h2>⬇️ 恢复</h2>
  <p class="hint">展开文件夹选择要恢复的文件，恢复到默认目录。</p>

  {#if restoreFolders.length === 0}
    <p class="warn">
      {#if backupPaths.length === 0}
        尚未配置备份路径：请先到「备份」页添加备份路径
      {:else}
        暂无备份数据：请先到「备份」页执行一次备份
      {/if}
    </p>
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

<style>
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
  .result { margin-top: 14px; padding: 12px; background: #ecfdf3; border-radius: 6px; }
  .result p { margin: 0 0 6px; font-weight: 600; }
  .result ul { margin: 0; padding-left: 20px; }
</style>
