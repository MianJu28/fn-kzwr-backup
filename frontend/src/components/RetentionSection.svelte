<script>
  // 保留策略（目标端孤儿文件清理）：回显当前配置并支持就地修改
  import Icon from './Icon.svelte';
  import { toast } from '../lib/toast.js';

  export let retention = null; // { enabled, cleanup_unmanaged, min_age_days, empty_recycle_bin }
  export let kzwrReady = false; // 是否已配置 access-token（清空回收站依赖它）
  export let busy = false;
  export let onSave = null; // ({enabled, cleanup_unmanaged, min_age_days, empty_recycle_bin}) => Promise<{error?}>

  let enabled = false;
  let cleanup = false;
  let minAge = 0;
  let emptyTrash = false;
  let recycleMaxGb = 0;
  let recycleMinAgeDays = 0;
  let working = false;

  // 仅在「后端回显值变化」时同步到表单，避免用户输入被覆盖
  let lastSeen = null;
  function syncFrom(r) {
    const sig = `${!!r.enabled}|${!!r.cleanup_unmanaged}|${Number(r.min_age_days) || 0}|${!!r.empty_recycle_bin}|${Number(r.recycle_max_gb) || 0}|${Number(r.recycle_min_age_days) || 0}`;
    if (sig === lastSeen) return;
    lastSeen = sig;
    enabled = !!r.enabled;
    cleanup = !!r.cleanup_unmanaged;
    minAge = Number(r.min_age_days) || 0;
    emptyTrash = !!r.empty_recycle_bin;
    recycleMaxGb = Number(r.recycle_max_gb) || 0;
    recycleMinAgeDays = Number(r.recycle_min_age_days) || 0;
  }
  $: syncFrom(retention || {});

  const nonNeg = (v) => Math.max(0, Math.floor(Number(v) || 0));

  async function save() {
    working = true;
    try {
      const r = await onSave({
        enabled,
        cleanup_unmanaged: cleanup,
        min_age_days: nonNeg(minAge),
        empty_recycle_bin: emptyTrash,
        recycle_max_gb: nonNeg(recycleMaxGb),
        recycle_min_age_days: nonNeg(recycleMinAgeDays),
      });
      if (r && r.error) toast.error(r.error, '保存失败');
      else toast.success('保留策略已保存并即时生效');
    } catch (e) {
      toast.error(e.message || String(e), '保存失败');
    } finally {
      working = false;
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="shield" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">保留策略</h2>
      <p class="card-desc">
        备份完成后清理云端残留文件，防止空间无限膨胀；仅作用于本应用自己的备份目录，不改动备份逻辑
      </p>
    </div>
    <span class="badge {enabled ? 'badge-ok' : ''}">{enabled ? '已启用' : '未启用'}</span>
  </div>

  <div class="card-body">
    <div class="row">
      <div class="row-text">
        <span class="row-title">启用保留策略</span>
        <span class="row-desc">关闭后备份不再执行任何清理</span>
      </div>
      <button
        class="switch"
        class:on={enabled}
        type="button"
        role="switch"
        aria-checked={enabled}
        aria-label="启用保留策略"
        on:click={() => (enabled = !enabled)}
      >
        <span class="knob"></span>
      </button>
    </div>

    <div class="row" class:dim={!enabled}>
      <div class="row-text">
        <span class="row-title">清理孤儿文件</span>
        <span class="row-desc">删除云端存在、但不在任何备份快照中的文件</span>
      </div>
      <button
        class="switch"
        class:on={enabled && cleanup}
        type="button"
        role="switch"
        aria-checked={enabled && cleanup}
        aria-label="清理孤儿文件"
        disabled={!enabled}
        on:click={() => (cleanup = !cleanup)}
      >
        <span class="knob"></span>
      </button>
    </div>

    <label class="field age-field" class:dim={!enabled || !cleanup}>
      <span class="label">
        最小保留天数 <span class="opt">（0 = 不限制，立即清理）</span>
      </span>
      <input
        class="input mono"
        type="number"
        min="0"
        step="1"
        bind:value={minAge}
        disabled={!enabled || !cleanup}
        placeholder="0"
      />
      <span class="field-hint">大于该天数的孤儿文件才会被清理，设为 0 表示只要发现就清理</span>
    </label>

    <div class="row" class:dim={!enabled || !kzwrReady}>
      <div class="row-text">
        <span class="row-title">清空云端回收站</span>
        <span class="row-desc">
          {#if kzwrReady}
            每次备份完成后永久删除回收站内全部条目
          {:else}
            需先在「增强功能」中配置 access-token
          {/if}
        </span>
      </div>
      <button
        class="switch"
        class:on={enabled && kzwrReady && emptyTrash}
        type="button"
        role="switch"
        aria-checked={enabled && kzwrReady && emptyTrash}
        aria-label="清空云端回收站"
        disabled={!enabled || !kzwrReady}
        on:click={() => (emptyTrash = !emptyTrash)}
      >
        <span class="knob"></span>
      </button>
    </div>

    <div class="recycle-gates" class:dim={!enabled || !kzwrReady || !emptyTrash}>
      <label class="field gate">
        <span class="label">占用超过（GB）才清理 <span class="opt">0 = 不限制</span></span>
        <input
          class="input mono"
          type="number"
          min="0"
          step="1"
          bind:value={recycleMaxGb}
          disabled={!enabled || !kzwrReady || !emptyTrash}
          placeholder="0"
        />
      </label>
      <label class="field gate">
        <span class="label">仅清理 N 天前的条目 <span class="opt">0 = 不限制</span></span>
        <input
          class="input mono"
          type="number"
          min="0"
          step="1"
          bind:value={recycleMinAgeDays}
          disabled={!enabled || !kzwrReady || !emptyTrash}
          placeholder="0"
        />
      </label>
    </div>

    <div class="alert alert-info">
      <Icon name="info" size={15} />
      <div class="alert-body">
        清理范围仅限云端目标文件夹内本应用管理的文件；本地文件永不删除。清空回收站为<strong>永久删除</strong>，不可恢复；
        设置「仅清理 N 天前」后，无法解析删除时间的条目会保守保留。设置页的「清空回收站」按钮为手动操作，不受上述门槛限制。
      </div>
    </div>
  </div>

  <div class="card-foot foot">
    <span class="foot-hint">修改后需点击保存；下次备份时生效</span>
    <button class="btn btn-primary" on:click={save} disabled={busy || working || !onSave}>
      {#if working}<span class="spin"></span>保存中{:else}<Icon name="check" size={15} />保存{/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
  }
  .row:last-of-type {
    border-bottom: none;
  }
  .row.dim {
    opacity: 0.55;
  }
  .row-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .row-title {
    font-size: 13px;
    font-weight: 560;
    color: var(--text);
  }
  .row-desc {
    font-size: 12px;
    color: var(--text-3);
  }
  .switch {
    position: relative;
    flex-shrink: 0;
    width: 42px;
    height: 24px;
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    background: var(--surface-3);
    cursor: pointer;
    transition: background var(--t-fast), border-color var(--t-fast);
  }
  .switch.on {
    background: var(--primary);
    border-color: var(--primary);
  }
  .switch:disabled {
    cursor: not-allowed;
    opacity: 0.6;
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.25);
    transition: transform var(--t-fast);
  }
  .switch.on .knob {
    transform: translateX(18px);
  }
  .age-field {
    margin-top: var(--s3);
  }
  .age-field.dim {
    opacity: 0.55;
  }
  .recycle-gates {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s3);
    margin-top: var(--s3);
  }
  .recycle-gates.dim {
    opacity: 0.55;
  }
  @media (max-width: 640px) {
    .recycle-gates {
      grid-template-columns: 1fr;
    }
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
