<script>
  export let node;
  export let expandedSet = new Set();
  export let busy = false;
  export let onToggleDir = () => {};
  export let onRestore = () => {};
  export let onRestoreDir = () => {};

  // 目录子节点（有序）
  $: dirs = node.children
    ? Object.values(node.children).filter((c) => c.is_dir).sort((a, b) => a.name.localeCompare(b.name))
    : [];
  $: files = node.children
    ? Object.values(node.children).filter((c) => !c.is_dir).sort((a, b) => a.name.localeCompare(b.name))
    : [];

  function fmtSize(bytes) {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / 1024 / 1024 / 1024).toFixed(2) + ' GB';
    if (bytes >= 1024 * 1024) return (bytes / 1024 / 1024).toFixed(2) + ' MB';
    if (bytes >= 1024) return (bytes / 1024).toFixed(1) + ' KB';
    return bytes + ' B';
  }

  // 递归收集当前目录下的所有文件相对路径
  function collectDirFiles(n) {
    const result = [];
    const walk = (cur) => {
      if (cur.children) {
        for (const child of Object.values(cur.children)) {
          if (child.is_dir) {
            walk(child);
          } else {
            result.push(child.rel_path);
          }
        }
      }
    };
    walk(n);
    return result;
  }
</script>

<!-- 目录节点 -->
{#if node.is_dir}
  <div class="tree-dir">
    <div class="dir-row">
      <button class="dir-toggle" on:click={() => onToggleDir(node.rel_path)}>
        <span class="dir-icon">{expandedSet.has(node.rel_path) ? '▾' : '▸'}</span>
        <span class="dir-label">📂 {node.name}</span>
      </button>
      <button
        class="restore-btn"
        on:click={() => onRestoreDir(collectDirFiles(node))}
        disabled={busy}
      >恢复</button>
    </div>
    {#if expandedSet.has(node.rel_path)}
      <div class="dir-children">
        {#each dirs as child (child.rel_path)}
          <svelte:self
            node={child}
            expandedSet={expandedSet}
            busy={busy}
            onToggleDir={onToggleDir}
            onRestore={onRestore}
            onRestoreDir={onRestoreDir}
          />
        {/each}
        {#each files as f (f.rel_path)}
          <div class="file-row">
            <span class="file-icon">📄</span>
            <span class="file-name">{f.name}</span>
            <span class="file-size">{fmtSize(f.size)}</span>
            <button class="restore-btn" on:click={() => onRestore(f.rel_path)} disabled={busy}>恢复</button>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{:else}
  <div class="file-row">
    <span class="file-icon">📄</span>
    <span class="file-name">{node.name}</span>
    <span class="file-size">{fmtSize(node.size)}</span>
    <button class="restore-btn" on:click={() => onRestore(node.rel_path)} disabled={busy}>恢复</button>
  </div>
{/if}

<style>
  .tree-dir { margin: 0; }
  .dir-row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 2px 4px;
    border-radius: 4px;
  }
  .dir-row:hover { background: #f1f5f9; }
  .dir-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
    background: transparent;
    color: #1f2d3d;
    border: none;
    padding: 5px 4px;
    margin: 0;
    cursor: pointer;
    text-align: left;
    font-size: 13px;
    border-radius: 4px;
  }
  .dir-icon { color: #42526e; font-size: 12px; width: 14px; }
  .dir-label { font-weight: 500; }
  .dir-children { margin-left: 18px; border-left: 1px dashed #e0e4ea; padding-left: 8px; }
  .file-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    font-size: 13px;
    border-radius: 4px;
  }
  .file-row:hover { background: #f8fafc; }
  .file-icon { color: #8a94a6; width: 14px; }
  .file-name { flex: 1; word-break: break-all; }
  .file-size { color: #8a94a6; font-size: 12px; white-space: nowrap; }
  .restore-btn {
    background: #22a06b;
    color: #fff;
    border: none;
    border-radius: 4px;
    padding: 3px 12px;
    margin: 0;
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
  }
  .restore-btn:disabled { background: #8cc9b0; cursor: not-allowed; }
</style>
