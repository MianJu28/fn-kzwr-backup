<script>
  // 备份路径配置 + 定时备份 cron
  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let busy = false;
  export let onSave = null; // () => Promise

  let pathInput = '';
  const cronPresets = [
    { label: '每天 00:00', value: '0 0 * * *' },
    { label: '每天 06:00', value: '0 6 * * *' },
    { label: '每天 23:00', value: '0 23 * * *' },
    { label: '每小时整点', value: '0 * * * *' },
    { label: '每 12 小时', value: '0 */12 * * *' },
    { label: '每周一 02:00', value: '0 2 * * 1' },
  ];

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

  function applyCronPreset(val) {
    scheduleCron = val;
    scheduleCronValid = true;
  }
</script>

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

  <button on:click={onSave} disabled={busy}>
    {busy ? '保存中...' : '保存配置'}
  </button>
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
  .warn { color: #b45309; }
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
  input {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    margin-top: 4px;
    font-size: 14px;
    box-sizing: border-box;
  }
  input.invalid { border-color: #dc2626; background: #fef2f2; }
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
  .example { color: #64748b; font-size: 12px; margin: 8px 0 0; }
</style>
