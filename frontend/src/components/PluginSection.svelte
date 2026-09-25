<script>
  /**
   * 外置插件（动态库）管理卡片
   *
   * 宿主启动时扫描插件目录（$TRIM_PKGETC/plugins、$TRIM_APPDEST/plugins 或自定义目录），
   * 用 libloading 加载 *.so 并校验 ABI 与编译期宿主版本。
   * 默认关闭（加载动态库 = 执行任意本地代码）；开关变更**需重启应用**生效。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';

  /** 是否启用外置插件加载（来自 /api/config） */
  export let enabled = false;
  /** 自定义插件目录（`:` 分隔多个；空 = 默认目录） */
  export let dir = '';
  export let busy = false;
  /** 保存回调：(enabled, dir) => Promise<{error?}> */
  export let onSave = null;

  let info = null; // /api/plugins 的 external 字段
  let loading = false;
  let formEnabled = enabled;
  let formDir = dir;
  let saved = false;

  // 外部值变化时同步表单（同值不覆盖，避免打断输入）
  $: if (enabled !== formEnabled && !saved) formEnabled = enabled;
  $: if (dir !== formDir && !saved) formDir = dir;

  async function loadInfo() {
    loading = true;
    try {
      const d = await api.plugins();
      info = d.external || null;
    } catch (e) {
      info = null;
    } finally {
      loading = false;
    }
  }

  loadInfo();

  async function save() {
    if (!onSave) return;
    saved = true;
    const r = await onSave(formEnabled, formDir.trim());
    saved = false;
    if (r && r.error) {
      toast.error(r.error, '保存失败');
      return;
    }
    toast.success('已保存，重启应用后生效');
    await loadInfo();
  }

  async function refresh() {
    await loadInfo();
    toast.info('已刷新插件状态');
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="package" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">外置插件（动态库）</h2>
      <p class="card-desc">
        把额外的 <code>.so</code> 插件放进插件目录即可扩展功能（新卡片、新接口、新备份目标），
        <strong>无需重新打包主程序与前端</strong>。默认关闭
      </p>
    </div>
    <span class="badge {formEnabled ? 'badge-ok' : ''}">
      {formEnabled ? '已启用' : '已关闭'}
    </span>
  </div>

  <div class="card-body">
    <label class="switch-row">
      <input
        type="checkbox"
        checked={formEnabled}
        on:change={(e) => (formEnabled = e.target.checked)}
        disabled={busy}
      />
      <span>启用外置插件加载（重启应用后生效）</span>
    </label>

    <div class="field">
      <label for="pl-dir">插件目录（可选，多个用 : 分隔）</label>
      <div class="field-row">
        <input
          id="pl-dir"
          placeholder="留空则用默认目录 $TRIM_PKGETC/plugins"
          value={formDir}
          on:input={(e) => (formDir = e.target.value)}
          disabled={busy}
        />
        <button class="btn btn-primary" on:click={save} disabled={busy}>保存</button>
        <button class="btn btn-ghost" on:click={refresh} disabled={loading}>刷新</button>
      </div>
      <p class="field-hint">
        留空时扫描：<code>$TRIM_PKGETC/plugins</code>（用户放置）与
        <code>$TRIM_APPDEST/plugins</code>（随应用分发）；同名文件以靠前的目录为准
      </p>
    </div>

    <div class="alert alert-warn">
      <Icon name="shield_alert" size={15} />
      <div class="alert-body">
        加载动态库等价于<strong>执行任意本地代码</strong>，请只放入你信任的插件；
        插件必须与当前主程序<strong>同版本编译</strong>（升级主程序后需重新编译插件），
        否则会被拒绝加载。单个插件加载失败不影响核心功能。
      </div>
    </div>

    {#if info}
      <div class="row-sub">
        <span class="meta"><b>扫描目录</b>{(info.dirs || []).length} 个</span>
        <span class="meta"><b>加载结果</b>{(info.reports || []).filter((r) => r.loaded).length} 成功 /
          {(info.reports || []).filter((r) => !r.loaded).length} 失败</span>
        {#if info.env_override !== null && info.env_override !== undefined}
          <span class="meta"><b>环境变量覆盖</b>{info.env_override ? '已强制开启' : '已强制关闭'}</span>
        {/if}
      </div>

      {#if (info.dirs || []).length}
        {#each info.dirs as d}
          <div class="row-sub"><code>{d.path}</code><span class="meta">{d.source}</span></div>
        {/each}
      {/if}

      {#if (info.reports || []).length}
        <div>
          {#each info.reports as r}
            <div class="row-item">
              <div class="grow">
                <div class="row-title">
                  <code>{r.file}</code>
                  <span class="badge {r.loaded ? 'badge-ok' : 'badge-danger'}">
                    {r.loaded ? '已加载' : '加载失败'}
                  </span>
                  {#if r.id}<span class="badge badge-info">{r.id}</span>{/if}
                  {#if r.kind}<span class="meta">{r.kind}</span>{/if}
                </div>
                {#if r.error}
                  <div class="row-sub"><Icon name="alert" size={13} />{r.error}</div>
                {:else if r.path}
                  <div class="row-sub"><code>{r.path}</code></div>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {:else if formEnabled}
        <p class="field-hint">插件目录中没有发现 <code>.so</code> 文件</p>
      {/if}
    {/if}

    {#if !formEnabled}
      <p class="field-hint">当前未启用：不会加载任何外置插件，已加载列表为空属正常现象</p>
    {/if}
  </div>
</section>
