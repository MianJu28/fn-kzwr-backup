<script>
  // 备份路径（增删即自动保存）+ 定时备份 cron（手动保存）
  import { pickBackupFolder, isInTrimHost } from '../trimSdk.js';
  import Icon from './Icon.svelte';

  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let busy = false;
  export let onSave = null; // () => Promise

  let pathInput = '';
  let pathMsg = '';
  let pathMsgOk = true;
  let inTrimHost = isInTrimHost();
  let savedHint = '';
  let saveTimer = null;

  const CRON_PRESETS = [
    { label: '每天 00:00', value: '0 0 * * *' },
    { label: '每天 06:00', value: '0 6 * * *' },
    { label: '每天 23:00', value: '0 23 * * *' },
    { label: '每小时整点', value: '0 * * * *' },
    { label: '每 12 小时', value: '0 */12 * * *' },
    { label: '每周一 02:00', value: '0 2 * * 1' },
  ];

  $: cronInvalid = !scheduleCronValid && scheduleCron.trim() !== '';

  // 路径增删后立即落盘（离散操作，无需手动保存）
  async function autoSave() {
    if (!onSave) return;
    clearTimeout(saveTimer);
    await onSave();
    savedHint = '备份路径已自动保存';
    setTimeout(() => (savedHint = ''), 2200);
  }

  function addPath() {
    const p = pathInput.trim();
    if (!p) return;
    if (backupPaths.includes(p)) {
      pathMsg = '该路径已在列表中';
      pathMsgOk = false;
      return;
    }
    backupPaths = [...backupPaths, p];
    pathInput = '';
    pathMsg = '';
    autoSave();
  }

  async function pickDir() {
    pathMsg = '';
    try {
      const dirs = await pickBackupFolder();
      if (dirs && dirs.length > 0) {
        const added = [];
        for (const d of dirs) {
          const p = d.trim();
          if (p && !backupPaths.includes(p)) {
            backupPaths = [...backupPaths, p];
            added.push(p);
          }
        }
        pathMsg = added.length > 0 ? `已添加 ${added.length} 个目录` : '这些目录已在列表中';
        pathMsgOk = added.length > 0;
        if (added.length > 0) await autoSave();
      } else {
        pathMsg = '已取消选择';
        pathMsgOk = true;
      }
    } catch (e) {
      pathMsg = e.message || '选择目录失败';
      pathMsgOk = false;
    }
  }

  function removePath(i) {
    backupPaths = backupPaths.filter((_, idx) => idx !== i);
    autoSave();
  }

  function applyCronPreset(val) {
    scheduleCron = val;
    scheduleCronValid = true;
    autoSave();
  }

  function onPathKey(e) {
    if (e.key === 'Enter') {
      e.preventDefault();
      addPath();
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="folder" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">备份路径</h2>
      <p class="card-desc">要备份的本地文件夹，支持多个（每个会在云端按文件夹名单独建目录）</p>
    </div>
    <span class="badge">{backupPaths.length} 个</span>
  </div>

  <div class="card-body">
    <label class="field">
      <span class="label">云端目标文件夹</span>
      <input class="input" bind:value={targetFolder} placeholder="fn-backup" />
      <span class="field-hint">备份文件将存放在此目录下，建议保持默认</span>
    </label>

    <div class="path-add">
      <input
        class="input mono"
        bind:value={pathInput}
        on:keydown={onPathKey}
        placeholder="/vol1/1000/data"
        aria-label="备份路径"
      />
      <button class="btn btn-soft" on:click={addPath} disabled={busy || !pathInput.trim()}>
        <Icon name="plus" size={14} />添加
      </button>
      {#if inTrimHost}
        <button class="btn btn-ghost" on:click={pickDir} disabled={busy}>
          <Icon name="folder-open" size={14} />选择目录
        </button>
      {/if}
    </div>

    {#if !inTrimHost}
      <p class="field-hint">当前不在飞牛宿主环境（开发模式），可手动输入路径。</p>
    {/if}
    {#if pathMsg}
      <div class="alert {pathMsgOk ? 'alert-ok' : 'alert-warn'} slim">
        <Icon name={pathMsgOk ? 'check-circle' : 'alert'} size={14} />
        <div class="alert-body">{pathMsg}</div>
      </div>
    {/if}

    {#if backupPaths.length}
      <ul class="paths">
        {#each backupPaths as p, i (p)}
          <li>
            <Icon name="folder" size={13} />
            <span class="mono grow path-text">{p}</span>
            <button
              class="btn-icon btn-sm"
              on:click={() => removePath(i)}
              disabled={busy}
              aria-label="移除 {p}"
              title="移除"
            >
              <Icon name="x" size={14} />
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <div class="empty slim">
        <div class="icon-wrap"><Icon name="folder" size={19} /></div>
        <strong>尚未添加备份路径</strong>
        点击上方「添加」或「选择目录」
      </div>
    {/if}
  </div>

  <div class="divider-title cron-title">定时备份</div>

  <div class="card-body">
    <p class="card-desc cron-desc">
      设置 cron 表达式定时自动触发备份（标准 5 段：<code>分 时 日 月 周</code>），留空即关闭。
    </p>
    <div class="presets">
      {#each CRON_PRESETS as preset (preset.value)}
        <button
          class="chip"
          class:active={scheduleCron === preset.value}
          type="button"
          on:click={() => applyCronPreset(preset.value)}
        >
          {preset.label}
        </button>
      {/each}
      <button
        class="chip danger"
        class:active={!scheduleCron}
        type="button"
        on:click={() => applyCronPreset('')}
      >
        <Icon name="x" size={12} />关闭定时
      </button>
    </div>

    <label class="field">
      <span class="label">cron 表达式</span>
      <input
        class="input mono"
        class:invalid={cronInvalid}
        bind:value={scheduleCron}
        placeholder="0 0 * * *"
      />
      {#if cronInvalid}
        <span class="field-error">表达式无效，请按「分 时 日 月 周」填写</span>
      {:else}
        <span class="field-hint">
          示例：<code>0 */12 * * *</code> 每 12 小时 · <code>0 2 * * 1</code> 每周一 02:00
        </span>
      {/if}
    </label>
  </div>

  <div class="card-foot foot">
    <span class="foot-hint">
      {#if savedHint}
        <span class="saved"><Icon name="check" size={13} />{savedHint}</span>
      {:else}
        路径增删会自动保存；目标文件夹与定时需点击保存
      {/if}
    </span>
    <button class="btn btn-primary" on:click={onSave} disabled={busy}>
      {#if busy}<span class="spin"></span>保存中{:else}<Icon name="check" size={15} />保存配置{/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .path-add {
    display: flex;
    gap: var(--s2);
    align-items: center;
    flex-wrap: wrap;
  }
  .path-add .input {
    flex: 1;
    min-width: 190px;
  }
  .slim {
    margin-top: var(--s3);
    padding: 9px 11px;
    font-size: 12.5px;
  }
  .paths {
    list-style: none;
    margin: var(--s3) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .paths li {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-2);
  }
  .path-text {
    font-size: 12px;
    word-break: break-all;
  }
  .cron-title {
    margin: var(--s5) var(--s5) 0;
  }
  .cron-desc {
    margin-bottom: var(--s3);
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: var(--s4);
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
    flex-wrap: wrap;
  }
  .foot-hint {
    color: var(--text-3);
    font-size: 12px;
  }
  .saved {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--success);
    font-weight: 560;
  }
</style>
