<script>
  /**
   * 概览页
   *
   * 插件化 + 多任务/多目标（ADR-014）之后，配置不再是一组「全局字段」，
   * 而是「若干任务 × 若干目标」。因此这里改为**按任务/目标聚合统计**，
   * 不再展示插件化前的全局备份路径 / WebDAV 凭据等遗留视图。
   *
   * 字段口径：`tasks` 来自 `/api/tasks`（`task_view`），`targets` 来自 `/api/targets`
   * （含 `ready` 与引用它的任务数 `tasks`）。
   */
  import SetupCheckSection from '../components/SetupCheckSection.svelte';
  import Icon from '../components/Icon.svelte';

  /** 任务列表（/api/tasks） */
  export let tasks = [];
  /** 目标列表（/api/targets） */
  export let targets = [];
  /** 云端可恢复的源文件夹（/api/restore/files） */
  export let restoreFolders = [];
  export let keyBackedUp = false;
  export let setupResult = null; // 一键体检结果
  export let busy = false;
  export let onSetupCheck = null; // () => Promise
  export let onGoto = null; // (pageId) => void

  $: taskTotal = tasks.length;
  $: taskEnabled = tasks.filter((t) => t.enabled !== false).length;
  /** 目标已就绪（凭据齐全）且任务已启用 = 真正能跑的任务 */
  $: taskReady = tasks.filter((t) => t.enabled !== false && t.target_ready).length;
  $: taskWithCron = tasks.filter((t) => t.schedule_cron).length;

  $: targetTotal = targets.length;
  $: targetReady = targets.filter((t) => t.ready).length;
  /** 被至少一个任务引用的目标数（其余为「闲置」） */
  $: targetUsed = targets.filter((t) => (t.tasks || 0) > 0).length;

  $: fileCount = restoreFolders.reduce((sum, f) => sum + (f.file_count || 0), 0);
  $: dirCount = restoreFolders.reduce((sum, f) => sum + (f.dir_count || 0), 0);

  /** 下一次定时触发（各启用任务取最近的一个） */
  $: nextRun = (() => {
    const xs = tasks
      .filter((t) => t.enabled !== false && t.schedule_next && t.schedule_next.length)
      .map((t) => t.schedule_next[0])
      .sort();
    return xs.length ? xs[0] : '';
  })();
</script>

<!-- 一键体检（配置是否完整、连通性是否正常） -->
<SetupCheckSection result={setupResult} {busy} onCheck={onSetupCheck} {onGoto} />

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="grid" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">备份概况</h2>
      <p class="card-desc">
        按「任务 × 目标」聚合。每个任务 = 源文件夹 + 目标 + 定时 + 保留策略，
        各自独立增量与快照
      </p>
    </div>
    <span class="badge">{taskReady}/{taskTotal} 个任务可运行</span>
  </div>

  <div class="card-body">
    <div class="stat-grid">
      <div class="stat">
        <div class="stat-label">备份任务</div>
        <div class="stat-value">{taskTotal || '—'}</div>
        <div class="stat-sub">
          {#if taskTotal === 0}
            尚未创建
          {:else}
            启用 {taskEnabled} · 定时 {taskWithCron}
          {/if}
        </div>
      </div>

      <div class="stat">
        <div class="stat-label">备份目标</div>
        <div class="stat-value">{targetTotal || '—'}</div>
        <div class="stat-sub">
          {#if targetTotal === 0}
            尚未配置
          {:else}
            就绪 {targetReady} · 被引用 {targetUsed}
          {/if}
        </div>
      </div>

      <div class="stat">
        <div class="stat-label">云端文件</div>
        <div class="stat-value">{fileCount || '—'}</div>
        <div class="stat-sub">{dirCount} 个目录可恢复</div>
      </div>

      <div class="stat">
        <div class="stat-label">私钥备份</div>
        <div class="stat-value sm">{keyBackedUp ? '已确认' : '未确认'}</div>
        <div class="stat-sub">
          {#if keyBackedUp}
            恢复时需要使用
          {:else}
            <button class="link" on:click={() => onGoto && onGoto('settings')}>前往设置</button>
          {/if}
        </div>
      </div>
    </div>

    <div class="kv-row">
      <div class="kv">
        <span class="kv-k">下次触发</span>
        <span class="kv-v">{nextRun || '未设置定时'}</span>
      </div>
    </div>

    {#if targetTotal === 0 || taskTotal === 0}
      <div class="alert alert-warn">
        <Icon name="alert" size={15} />
        <div class="alert-body">
          {#if targetTotal === 0}
            还没有备份目标，先创建一个才能新建任务。
            <button class="btn btn-sm btn-soft inline" on:click={() => onGoto && onGoto('targets')}>
              前往目标<Icon name="arrow-right" size={13} />
            </button>
          {:else}
            还没有备份任务，新建后即可开始增量备份。
            <button class="btn btn-sm btn-soft inline" on:click={() => onGoto && onGoto('tasks')}>
              前往任务<Icon name="arrow-right" size={13} />
            </button>
          {/if}
        </div>
      </div>
    {/if}
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .stat-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: var(--s3);
  }
  .kv-row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s4);
    margin-top: var(--s3);
    padding-top: var(--s3);
    border-top: 1px solid var(--border);
  }
  .kv {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .kv-k {
    font-size: 11.5px;
    color: var(--text-3);
  }
  .kv-v {
    font-size: 13px;
    color: var(--text);
  }
  .link {
    border: none;
    background: none;
    padding: 0;
    color: var(--primary);
    cursor: pointer;
    font-size: 11.5px;
  }
  .link:hover {
    text-decoration: underline;
  }
</style>
