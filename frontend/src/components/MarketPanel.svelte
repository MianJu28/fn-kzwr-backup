<script>
  /**
   * 插件市场面板
   *
   * ## 它在安全模型里的位置（改动前必读）
   *
   * 市场**不是信任源**：它只负责「发现与运输」。真正决定能不能装的是后端算出的
   * `installable` / `blocked_reason`（ABI / 架构 / glibc / 宿主版本只有宿主知道），
   * 前端**不做任何兼容性推断** —— 否则前后端两套判断迟早不一致。
   *
   * ## 产品基调（评审决定）
   *
   * 第三方插件有没有危险代码，**机制上防不住**（插件与宿主同进程）。
   * 因此本面板的职责是**把判断所需的信息如实摊开**，让用户自己决定：
   * 开源地址、发布者、审核人、同进程风险、可访问任意网络。
   * **不得**出现「已审核 = 安全」这类承诺性表述。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';

  /** 是否启用市场（来自 /api/config）；false 时整块折叠、不联网 */
  export let enabled = false;
  export let busy = false;
  /** 保存回调：(patch) => Promise<{error?}>，用于开启市场 */
  export let onSave = null;
  /** 安装完成后的回调（让外层刷新插件清单） */
  export let onDone = null;

  let loading = false;
  let installing = '';
  let data = null;
  let error = '';

  // 市场开启时才拉列表（"不在用户未要求时联网"）
  $: if (enabled) ensureLoaded();

  let loaded = false;
  async function ensureLoaded() {
    if (loaded || loading) return;
    loaded = true;
    await load();
  }

  async function load(refresh = false) {
    loading = true;
    error = '';
    try {
      const d = await api.marketCatalog(refresh);
      data = d;
      if (d && d.error) error = d.error;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    loading = true;
    error = '';
    try {
      const r = await api.marketRefresh();
      if (r && r.error) {
        error = r.error;
        toast.error(r.error, '刷新失败');
      } else {
        toast.success(`索引已更新（版本 ${r.catalog_version}）`);
        await load();
      }
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function enable() {
    if (!onSave) return;
    const r = await onSave({ market_enabled: true });
    if (r && r.error) {
      toast.error(r.error, '开启失败');
      return;
    }
    toast.success('插件市场已开启');
  }

  /**
   * 安装前的确认弹窗 —— **本面板最重要的一块 UI**
   *
   * 必须给出用户判断所需的全部信息。特别地：
   * - 明确说明插件与宿主**同进程**、能读进程内存；
   * - 明确说明插件**可访问任意网络**（我们不做端口限制）；
   * - 明确说明我们保证的是「源码与字节一致」，**不保证**不含恶意行为。
   */
  async function confirmInstall(p, v) {
    const yes = await confirmDialog({
      title: `安装插件「${p.name || p.id}」？`,
      message:
        `版本 ${v.version}    大小 ${(v.size_bytes / 1024).toFixed(0)} KB\n` +
        `发布者：${p.publisher || '未标注'}    审核：${p.reviewed_by || '未标注'}\n` +
        `源码：${p.homepage || '未提供'}\n` +
        (v.source_commit ? `commit：${v.source_commit}\n` : '') +
        `\n⚠️ 请注意：\n` +
        `· 该插件与备份程序运行在**同一进程**，能读取进程内存；\n` +
        `  沙箱只限制文件访问，无法阻止它读取内存中的密钥。\n` +
        `· 该插件**可访问任意网络**（包括外网）。\n` +
        `· 我们能保证的只是：你装到的字节，就是上面这个 commit 由 CI 构建并签名的那份。\n` +
        `· 我们**不保证**它不含恶意行为 —— 请查看源码后再决定。\n`,
      confirmText: '仍要安装',
      danger: true,
    });
    return yes;
  }

  async function install(p, v) {
    const yes = await confirmInstall(p, v);
    if (!yes) return;
    installing = p.id;
    try {
      const r = await api.marketInstall(p.id, v.version);
      if (r && r.error) {
        toast.error(r.error, '安装失败');
      } else {
        toast.success(
          `${p.name || p.id} ${v.version} ${r.loaded ? '已安装并加载' : '已安装'}`,
          r.loaded ? '成功' : '已安装（未加载）'
        );
        if (r.note && !r.loaded) toast.info(r.note);
        await load(true);
        if (onDone) await onDone();
      }
    } catch (e) {
      toast.error(e.message);
    } finally {
      installing = '';
    }
  }

  /** 版本列表（展开查看历史版本） */
  let expanded = '';
  function toggleExpand(id) {
    expanded = expanded === id ? '' : id;
  }

  $: plugins = (data && data.plugins) || [];
  /** 只展示有可安装版本的；不可安装的也留着（并说明原因），用户才知道为什么 */
  $: updatable = plugins.filter((p) => p.installable && p.installed_version);
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="download" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">插件市场</h2>
      <p class="card-desc">
        从官方索引浏览并安装插件。索引经官方签名校验，制品会比对哈希并验签 ——
        我们能保证「你装到的字节 = 该 commit 由 CI 构建并签名的那份」，
        <strong>但不保证插件不含恶意代码</strong>：请自行查看源码后决定。
      </p>
    </div>
    <span class="badge {enabled ? 'badge-ok' : ''}">{enabled ? '已开启' : '已关闭'}</span>
  </div>

  <div class="card-body">
    {#if !enabled}
      <div class="switch-bar">
        <div class="grow">
          <div class="switch-title">启用插件市场</div>
          <p class="field-hint">
            开启后可从官方索引安装插件。关闭时<strong>不会发起任何网络请求</strong>。
          </p>
        </div>
        <button class="btn btn-primary" on:click={enable} disabled={busy}>
          <Icon name="zap" size={14} />开启
        </button>
      </div>
    {:else}
      <div class="row-sub">
        <span class="meta"><b>索引版本</b>{data?.catalog_version ?? '—'}</span>
        <span class="meta"><b>生成时间</b>{data?.generated_at ?? '—'}</span>
        <span class="meta"><b>来源</b>{data?.source ?? '—'}</span>
        {#if updatable.length}
          <span class="meta"><b>可更新</b>{updatable.length} 个</span>
        {/if}
        <button class="btn btn-sm btn-ghost" on:click={refresh} disabled={loading}>
          <Icon name="refresh" size={13} />{loading ? '刷新中…' : '刷新索引'}
        </button>
      </div>

      {#if error}
        <div class="alert alert-warn">
          <Icon name="shield_alert" size={15} />
          <div class="alert-body">{error}</div>
        </div>
      {/if}

      {#if loading && !plugins.length}
        <p class="field-hint">正在读取索引…</p>
      {:else if !plugins.length}
        <p class="field-hint">
          索引中没有插件（或索引暂不可用）。{error ? '' : '可点「刷新索引」重试。'}
        </p>
      {:else}
        <div>
          {#each plugins as p (p.id)}
            {@const v = p.latest}
            <div class="row-item kind-{p.kind === 'target' ? 'target' : 'enhance'}">
              <div class="grow">
                <div class="row-title">
                  <code>{p.id}</code>
                  <span class="badge badge-info">{p.name}</span>
                  {#if p.origin === 'official'}
                    <span class="badge badge-ok" title="官方维护">官方</span>
                  {:else if p.origin === 'community'}
                    <span class="badge badge-info" title="社区贡献（经审核合入）">社区</span>
                  {/if}
                  {#if p.installed_version}
                    <span class="badge badge-ok">已装 {p.installed_version}</span>
                  {/if}
                  {#if !p.installable && p.blocked_reason}
                    <span class="badge badge-warn">{p.blocked_reason}</span>
                  {/if}
                </div>

                {#if p.description}
                  <div class="row-sub">{p.description}</div>
                {/if}

                <!-- 让用户能自行判断：给出可点击的源码地址 -->
                <div class="row-sub">
                  <span class="meta"><b>发布者</b>{p.publisher || '未标注'}</span>
                  <span class="meta"><b>审核</b>{p.reviewed_by || '未标注'}</span>
                  {#if p.homepage}
                    <a class="src-link" href={p.homepage} target="_blank" rel="noopener noreferrer">
                      <Icon name="external" size={12} />查看源码
                    </a>
                  {/if}
                  {#if v?.source_commit}
                    <span class="meta"><b>commit</b><code>{v.source_commit}</code></span>
                  {/if}
                </div>

                <div class="row-sub">
                  {#if v}
                    <button
                      class="btn {p.installed_version ? 'btn-ghost' : 'btn-primary'} btn-sm"
                      on:click={() => install(p, v)}
                      disabled={!p.installable || installing === p.id}
                    >
                      {installing === p.id
                        ? '安装中…'
                        : p.installed_version
                          ? `更新到 ${v.version}`
                          : `安装 ${v.version}`}
                    </button>
                    {#if p.versions && p.versions.length > 1}
                      <button class="btn btn-ghost btn-sm" on:click={() => toggleExpand(p.id)}>
                        <Icon name="chevron_{expanded === p.id ? 'down' : 'right'}" size={12} />
                        共 {p.versions.length} 个版本
                      </button>
                    {/if}
                  {:else}
                    <span class="field-hint">没有与本机兼容的版本</span>
                  {/if}
                </div>

                {#if expanded === p.id}
                  <div class="ver-list">
                    {#each p.versions as hv}
                      <div class="ver-row">
                        <code>{hv.version}</code>
                        <span class="meta">{hv.arch}</span>
                        {#if hv.yanked}<span class="badge badge-warn">已撤回</span>{/if}
                        {#if hv.glibc_min}<span class="meta">glibc ≥ {hv.glibc_min}</span>{/if}
                        {#if hv.min_host_version}
                          <span class="meta">宿主 ≥ {hv.min_host_version}</span>
                        {/if}
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}

      <div class="alert alert-warn">
        <Icon name="shield_alert" size={15} />
        <div class="alert-body">
          第三方插件与主程序运行在<strong>同一进程</strong>内。沙箱能挡住磁盘上的密钥与口令，
          但<strong>无法阻止它读取进程内存</strong>，也<strong>不限制它访问网络</strong>。
          安装前请查看源码，并只安装你信任的插件。
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
  .row-item {
    position: relative;
    border-left: 3px solid var(--border-strong);
  }
  .row-item.kind-target {
    border-left-color: var(--primary);
  }
  .row-item.kind-enhance {
    border-left-color: var(--info, #0369a1);
  }
  .switch-bar {
    display: flex;
    align-items: flex-start;
    gap: var(--s3);
    padding: var(--s3) var(--s4);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
  }
  .switch-title {
    font-size: 13.5px;
    font-weight: 620;
    color: var(--text);
  }
  .src-link {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--primary);
    text-decoration: none;
    font-size: 12.5px;
  }
  .src-link:hover {
    text-decoration: underline;
  }
  .ver-list {
    margin-top: var(--s2);
    padding: var(--s2) var(--s3);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
  }
  .ver-row {
    display: flex;
    align-items: center;
    gap: var(--s3);
    font-size: 12.5px;
    padding: 2px 0;
  }
</style>
