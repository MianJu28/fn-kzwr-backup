<script>
  /**
   * 外置插件（动态库）管理卡片
   *
   * 宿主启动时扫描插件目录（$TRIM_PKGETC/plugins、$TRIM_APPDEST/plugins 或自定义目录），
   * 用 libloading 加载 *.so、校验稳定 C ABI，并**强制校验 Ed25519 签名**
   * （ADR-013 决策 3：信任锚 = 宿主内置的官方公钥 ∪ 下方「插件公钥」，任一验签通过即加载）。
   * 随包插件用官方私钥签名 → **开箱即用、无需配置**；自签插件才需要把公钥填在这里。
   * 默认关闭（加载动态库 = 执行任意本地代码）；开关变更**需重启应用**生效。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';

  /** 是否启用外置插件加载（来自 /api/config） */
  export let enabled = false;
  /** 自定义插件目录（`:` 分隔多个；空 = 默认目录） */
  export let dir = '';
  /** 插件签名公钥（base64 的 32 字节 Ed25519 公钥，每行一个） */
  export let pubkeys = [];
  /** 是否放行未签名插件（仅环境变量 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`；只读） */
  export let allowUnsigned = false;
  export let busy = false;
  /** 保存回调：(enabled, dir, pubkeys) => Promise<{error?}> */
  export let onSave = null;

  let info = null; // /api/plugins 的 external / orphan_data 字段
  let orphanData = [];
  let loading = false;
  let formEnabled = enabled;
  let formDir = dir;
  // 公钥每行一个，便于用户从 sign_plugin.sh 的输出直接粘贴
  let formPubkeys = (pubkeys || []).join('\n');
  let saved = false;
  let purgeBusy = '';

  // 外部值变化时同步表单（同值不覆盖，避免打断输入）
  $: if (enabled !== formEnabled && !saved) formEnabled = enabled;
  $: if (dir !== formDir && !saved) formDir = dir;
  $: if ((pubkeys || []).join('\n') !== formPubkeys && !saved) {
    formPubkeys = (pubkeys || []).join('\n');
  }

  async function loadInfo() {
    loading = true;
    try {
      const d = await api.plugins();
      info = d.external || null;
      orphanData = d.orphan_data || [];
    } catch (e) {
      info = null;
      orphanData = [];
    } finally {
      loading = false;
    }
  }

  loadInfo();

  /** 签名徽标（ADR-013 决策 3）：区分「已签名 / 未签名 / 验签失败」 */
  function sigBadge(r) {
    if (r.signature === 'verified') {
      return { cls: 'badge-ok', text: '已签名', title: 'Ed25519 签名校验通过' };
    }
    if (r.signature === 'unsigned') {
      return {
        cls: 'badge-warn',
        text: '未签名',
        title: '已由 FN_KZWR_PLUGINS_ALLOW_UNSIGNED 放行，仅限本机调试',
      };
    }
    // 区分「没有 .sig 文件」与「签名不匹配」，便于用户判断该签还是该换公钥
    return {
      cls: 'badge-danger',
      text: r.sig_file ? '验签失败' : '未签名',
      title: r.sig_file
        ? '签名与内置官方公钥、以及配置的公钥均不匹配（公钥错误或插件被篡改）'
        : '缺少同名 .sig 文件，请用 Scripts/sign_plugin.sh sign 生成',
    };
  }

  async function save() {
    if (!onSave) return;
    saved = true;
    const keys = formPubkeys
      .split('\n')
      .map((s) => s.trim())
      .filter(Boolean);
    const r = await onSave(formEnabled, formDir.trim(), keys);
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

  /** 卸载清除：删除该插件的宿主代管数据（仍被引用时后端拒绝） */
  async function purge(id) {
    const yes = await confirmDialog({
      title: `卸载清除插件 ${id}`,
      message:
        '将删除该插件的宿主代管配置（凭据等）并调用其清理钩子。' +
        '动态库文件本身不会被删除，仍被目标或任务引用时会被拒绝。此操作不可恢复。',
      confirmText: '清除数据',
      danger: true,
    });
    if (!yes) return;
    purgeBusy = id;
    try {
      const r = await api.pluginPurge(id);
      if (r && r.error) {
        const refs = (r.referenced_by || []).join('、');
        toast.error(refs ? `${r.error}：${refs}` : r.error);
      } else {
        toast.success(`已清除插件 ${id} 的代管数据`);
        await loadInfo();
      }
    } catch (e) {
      toast.error(e.message);
    } finally {
      purgeBusy = '';
    }
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
      </div>
      <p class="field-hint">
        留空时扫描：<code>$TRIM_PKGETC/plugins</code>（用户放置）与
        <code>$TRIM_APPDEST/plugins</code>（随应用分发）；同名文件以靠前的目录为准。
        推荐插件使用<strong>稳定 C ABI</strong>（只依赖冻结的 JSON 契约）——升级本应用后
        <strong>无需重新编译插件</strong>
      </p>
    </div>

    <div class="field">
      <label for="pl-pubkeys">插件公钥（每行一个，base64 的 32 字节 Ed25519 公钥）</label>
      <div class="field-row">
        <textarea
          id="pl-pubkeys"
          class="textarea mono"
          rows="3"
          placeholder="例如：YCzDjlN5uEHPulgwyGWnZYpYV3P7O1xPNpTT0zAkv+A=&#10;只填自签插件的公钥；随包官方插件已内置公钥，无需填写"
          value={formPubkeys}
          on:input={(e) => (formPubkeys = e.target.value)}
          disabled={busy}
        />
        <button class="btn btn-primary" on:click={save} disabled={busy}>保存</button>
        <button class="btn btn-ghost" on:click={refresh} disabled={loading}>刷新</button>
      </div>
      <p class="field-hint">
        <strong>随包插件无需填写</strong>：它们由官方私钥签名，宿主已内置对应公钥
        （随包示例插件开箱即用）。这里只填<strong>你自己签的</strong>插件的公钥。
        用 <code>Scripts/sign_plugin.sh keygen</code> 生成密钥对（私钥保密、勿入库），
        把输出的公钥粘贴到这里；再用 <code>Scripts/sign_plugin.sh sign &lt;插件目录&gt;</code>
        为每个 <code>.so</code> 生成同名 <code>.so.sig</code>。公钥可填多个（任一匹配即通过）。
        <strong>未签名、或验签失败的插件一律不加载</strong>——留空并不等于放行。
      </p>
    </div>

    {#if allowUnsigned}
      <div class="alert alert-warn">
        <Icon name="shield_alert" size={15} />
        <div class="alert-body">
          检测到环境变量 <code>FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1</code>：
          <strong>当前已跳过全部插件签名校验</strong>，任何 <code>.so</code> 都会被加载。
          该开关仅供本机调试，请勿在正式环境使用。
        </div>
      </div>
    {/if}

    <div class="alert alert-warn">
      <Icon name="shield_alert" size={15} />
      <div class="alert-body">
        加载动态库等价于<strong>执行任意本地代码</strong>，请只放入你信任的插件；
        自 v0.4.0 起插件默认<strong>强制验签</strong>（Ed25519），验签失败或缺签名的插件会被拒绝加载。
        单个插件加载失败不影响核心功能。
      </div>
    </div>

    {#if orphanData.length}
      <div class="alert alert-info">
        <Icon name="info" size={15} />
        <div class="alert-body">
          发现<strong>遗留的插件配置</strong>：{orphanData.join('、')}。
          这些插件当前未加载（可能已卸载或未启用），但其代管配置仍在。
          确认不再需要时可点击下方对应插件的「清除代管数据」。
        </div>
      </div>
    {/if}

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
            {@const sig = sigBadge(r)}
            <div class="row-item">
              <div class="grow">
                <div class="row-title">
                  <code>{r.file}</code>
                  <span class="badge {r.loaded ? 'badge-ok' : 'badge-danger'}">
                    {r.loaded ? '已加载' : '加载失败'}
                  </span>
                  <span class="badge {sig.cls}" title={sig.title}>{sig.text}</span>
                  {#if r.id}<span class="badge badge-info">{r.id}</span>{/if}
                  {#if r.kind}<span class="meta">{r.kind}</span>{/if}
                  {#if r.mechanism === 'c-abi-v1'}
                    <span class="badge badge-ok" title="只依赖冻结的 C ABI 契约：升级本应用无需重编此插件">
                      稳定 ABI v{r.abi || 1}
                    </span>
                  {/if}
                </div>
                {#if r.error}
                  <div class="row-sub"><Icon name="alert" size={13} />{r.error}</div>
                {:else if r.path}
                  <div class="row-sub"><code>{r.path}</code></div>
                {/if}
                {#if r.id}
                  <div class="row-sub">
                    <button
                      class="btn btn-ghost btn-sm"
                      on:click={() => purge(r.id)}
                      disabled={purgeBusy === r.id}
                      title="删除该插件的宿主代管配置（仍被目标/任务引用时会被拒绝）"
                    >
                      {purgeBusy === r.id ? '清除中…' : '清除代管数据'}
                    </button>
                  </div>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {:else if formEnabled}
        <p class="field-hint">插件目录中没有发现 <code>.so</code> 文件</p>
      {/if}

      {#if orphanData.length && !(info.reports || []).length}
        <div>
          {#each orphanData as id}
            <div class="row-item">
              <div class="grow">
                <div class="row-title">
                  <code>{id}</code>
                  <span class="badge badge-warn">未加载</span>
                </div>
                <div class="row-sub">
                  <button class="btn btn-ghost btn-sm" on:click={() => purge(id)} disabled={purgeBusy === id}>
                    {purgeBusy === id ? '清除中…' : '清除代管数据'}
                  </button>
                </div>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    {/if}

    {#if !formEnabled}
      <p class="field-hint">当前未启用：不会加载任何外置插件，已加载列表为空属正常现象</p>
    {/if}
  </div>
</section>
