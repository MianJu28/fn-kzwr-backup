<script>
  /**
   * 外置插件（动态库）管理卡片
   *
   * 宿主启动时扫描插件目录，用 libloading 加载 `*.so`、校验稳定 C ABI，
   * 并**强制校验 Ed25519 签名**（ADR-013 决策 3）。
   *
   * 设计取舍（2026-09-27 调整）：
   * - **插件目录不再让用户配置**：只用默认目录（`$TRIM_PKGETC/plugins` 用户放置、
   *   `$TRIM_APPDEST/plugins` 随包分发）。少一个配置项就少一类「填错就静默不加载」的故障。
   * - **安装走界面**：`POST /api/plugins/install` 上传 `.so` + `.so.sig`，
   *   安装时即用**该插件自己的公钥**验签（一插件一公钥），并把绑定关系落盘。
   * - 未启用时下方设置**折叠**，避免默认关闭状态下展示一堆无关项。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';

  /** 是否启用外置插件加载（来自 /api/config） */
  export let enabled = false;
  /** 是否放行未签名插件（仅环境变量 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`；只读） */
  export let allowUnsigned = false;
  export let busy = false;
  /** 保存回调：(enabled) => Promise<{error?}> */
  export let onSave = null;

  let info = null; // /api/plugins 的 external / orphan_data 字段
  let orphanData = [];
  let loading = false;
  let formEnabled = enabled;
  let saved = false;
  let purgeBusy = '';
  /** 正在卸载的插件文件名 */
  let uninstallBusy = '';

  // ── 安装表单 ──────────────────────────────────────────────────
  let showInstall = false;
  let installing = false;
  let instName = ''; // 目标文件名（默认取所选 .so 的文件名）
  let instPubkey = '';
  let instSo = null; // 选中的 .so File
  let instSig = null; // 选中的 .sig File（可留空 = 由浏览器按同名推导？不：签名必须显式提供）
  let instMsg = '';
  let instOk = false;

  // 启用状态变化时同步表单（同值不覆盖，避免打断输入）
  $: if (enabled !== formEnabled && !saved) formEnabled = enabled;

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
    const r = await onSave(formEnabled);
    saved = false;
    if (r && r.error) {
      toast.error(r.error, '保存失败');
      return;
    }
    toast.success(
      formEnabled
        ? '已启用外置插件：重启应用后生效'
        : '已关闭外置插件加载：重启应用后生效'
    );
    await loadInfo();
  }

  // ── 安装：读文件 → base64 → 提交（后端先验签再落盘） ────────────

  /** File → base64（去掉 data URL 前缀） */
  function fileToB64(file) {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onerror = () => reject(new Error(`读取 ${file.name} 失败`));
      r.onload = () => {
        const s = String(r.result);
        const i = s.indexOf(',');
        resolve(i >= 0 ? s.slice(i + 1) : s);
      };
      r.readAsDataURL(file);
    });
  }

  function pickSo(e) {
    const f = e.target.files && e.target.files[0];
    if (!f) return;
    instSo = f;
    // 默认文件名取所选 .so 的名字（用户可改，但一般不用）
    if (!instName) instName = f.name;
    instMsg = '';
  }

  function pickSig(e) {
    const f = e.target.files && e.target.files[0];
    if (!f) return;
    instSig = f;
    instMsg = '';
  }

  async function install() {
    instMsg = '';
    instOk = false;
    if (!instSo) {
      instMsg = '请先选择插件文件（.so）';
      return;
    }
    if (!instSig) {
      instMsg = '请同时选择签名文件（<插件名>.so.sig）—— 宿主强制验签，缺签名无法安装';
      return;
    }
    const name = (instName || instSo.name).trim();
    if (!name.toLowerCase().endsWith('.so')) {
      instMsg = '文件名必须以 .so 结尾';
      return;
    }
    if (!instPubkey.trim()) {
      instMsg = '请填写用于校验该插件的公钥（用签名私钥对应的公钥）';
      return;
    }
    installing = true;
    try {
      const [data_b64, sig_b64] = await Promise.all([
        fileToB64(instSo),
        fileToB64(instSig),
      ]);
      const r = await api.pluginInstall({
        file_name: name,
        data_b64,
        sig_b64,
        pubkey: instPubkey.trim(),
      });
      if (r && r.error) {
        instMsg = r.error;
        toast.error(r.error, '安装失败');
      } else {
        instOk = true;
        instMsg = `已安装 ${name}；重启应用后加载。`;
        toast.success(`${name} 已安装并通过签名校验`, '安装成功');
        // 复位表单
        instSo = null;
        instSig = null;
        instPubkeysReset();
        showInstall = false;
        await loadInfo();
      }
    } catch (e) {
      instMsg = e.message;
      toast.error(e.message);
    } finally {
      installing = false;
    }
  }

  function instPubkeysReset() {
    instName = '';
    instPubkey = '';
  }

  /** 卸载**外置**插件：删除 .so/.sig 并解绑其公钥（内置/随包插件删不掉） */
  async function uninstall(file) {
    const yes = await confirmDialog({
      title: `卸载插件 ${file}？`,
      message:
        '将从插件目录删除该 .so 与其签名文件，并解绑它的公钥。\n' +
        '若它仍被目标或任务使用，那些任务会失效（建议先停用相关任务）。\n\n' +
        '注意：已加载到内存的代码要等**重启应用**才真正释放。',
      confirmText: '卸载',
      danger: true,
    });
    if (!yes) return;
    uninstallBusy = file;
    try {
      const r = await api.pluginUninstall(file);
      if (r && r.error) {
        toast.error(r.error, '卸载失败');
      } else {
        toast.success(`${file} 已卸载`, '重启应用后彻底释放');
        await loadInfo();
      }
    } catch (e) {
      toast.error(e.message);
    } finally {
      uninstallBusy = '';
    }
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
    <!-- 总开关：关闭时折叠下方全部设置 -->
    <label class="switch-row">
      <input
        type="checkbox"
        checked={formEnabled}
        on:change={(e) => (formEnabled = e.target.checked)}
        disabled={busy}
      />
      <span>启用外置插件加载<em class="opt">（重启应用后生效）</em></span>
    </label>

    {#if formEnabled !== enabled}
      <div class="alert alert-info">
        <Icon name="info" size={15} />
        <div class="alert-body">
          开关已改动但<strong>尚未保存</strong>。
          <button class="btn btn-sm btn-primary inline" on:click={save} disabled={busy}>保存并生效</button>
        </div>
      </div>
    {/if}

    {#if !formEnabled}
      <p class="field-hint">
        未启用：不会加载任何外置插件。下面的安装与管理在启用后才可用。
      </p>
    {:else}
      <!-- 安装插件：上传 .so + .so.sig，并填写该插件的公钥（一插件一公钥） -->
      <div class="install-block">
        <div class="install-head">
          <div class="grow">
            <div class="install-title">安装插件</div>
            <p class="field-hint">
              选择插件文件 <code>.so</code> 与它的签名 <code>.so.sig</code>，
              并填写<strong>该插件的公钥</strong>。安装时会先验签，不通过不会写入磁盘。
            </p>
          </div>
          <button class="btn btn-sm btn-primary" on:click={() => (showInstall = !showInstall)} disabled={busy || installing}>
            <Icon name={showInstall ? 'minus' : 'plus'} size={14} />{showInstall ? '收起' : '安装插件'}
          </button>
        </div>

        {#if showInstall}
          <div class="install-form">
            <div class="field">
              <label for="pl-so">插件文件（.so）</label>
              <input id="pl-so" type="file" accept=".so" on:change={pickSo} disabled={installing} />
            </div>
            <div class="field">
              <label for="pl-sig">签名文件（.so.sig）</label>
              <input id="pl-sig" type="file" accept=".sig" on:change={pickSig} disabled={installing} />
              <p class="field-hint">
                用 <code>Scripts/sign_plugin.sh sign &lt;插件.so&gt;</code> 生成（与所选私钥同源）。
              </p>
            </div>
            <div class="field">
              <label for="pl-name">安装文件名</label>
              <input id="pl-name" placeholder="默认取所选文件名" value={instName}
                on:input={(e) => (instName = e.target.value)} disabled={installing} />
            </div>
            <div class="field">
              <label for="pl-pub">该插件的公钥（base64 的 32 字节 Ed25519 公钥）</label>
              <input id="pl-pub" class="mono" placeholder="例如：YCzDjlN5uEHPulgwyGWnZYpYV3P7O1xPNpTT0zAkv+A="
                value={instPubkey} on:input={(e) => (instPubkey = e.target.value)} disabled={installing} />
              <p class="field-hint">
                用 <code>Scripts/sign_plugin.sh pubkey</code> 打印。
                <strong>一个插件只认它自己的公钥</strong>——其它插件的公钥无法通过校验。
              </p>
            </div>
            {#if instMsg}
              <p class={instOk ? 'field-hint ok' : 'field-error'}>{instMsg}</p>
            {/if}
            <div class="row-actions">
              <button class="btn btn-primary" on:click={install} disabled={installing}>
                {installing ? '校验并安装中…' : '校验并安装'}
              </button>
              <button class="btn btn-ghost" on:click={() => (showInstall = false)} disabled={installing}>取消</button>
            </div>
          </div>
        {/if}
      </div>
    {/if}

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
        <span class="meta"><b>扫描目录</b>{(info.dirs || []).length} 个（固定，无需配置）</span>
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
                <div class="row-sub">
                  {#if r.id}
                    <button
                      class="btn btn-ghost btn-sm"
                      on:click={() => purge(r.id)}
                      disabled={purgeBusy === r.id}
                      title="删除该插件的宿主代管配置（仍被目标/任务引用时会被拒绝）"
                    >
                      {purgeBusy === r.id ? '清除中…' : '清除代管数据'}
                    </button>
                  {/if}
                  <button
                    class="btn btn-ghost btn-sm danger"
                    on:click={() => uninstall(r.file)}
                    disabled={uninstallBusy === r.file}
                    title="从插件目录删除该 .so 与其签名，并解绑公钥（内置/随包插件删不掉）"
                  >
                    {uninstallBusy === r.file ? '卸载中…' : '卸载插件'}
                  </button>
                </div>
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

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .opt {
    font-weight: 400;
    color: var(--text-3);
    font-style: normal;
  }
  .inline {
    margin-left: 8px;
  }
  .field-hint.ok {
    color: var(--success);
  }

  /* 安装区：浅色面板，与插件列表区分 */
  .install-block {
    padding: var(--s3) var(--s4);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
  }
  .install-head {
    display: flex;
    align-items: flex-start;
    gap: var(--s3);
  }
  .install-title {
    font-size: 13.5px;
    font-weight: 620;
    color: var(--text);
  }
  .install-head .field-hint {
    margin-top: 2px;
  }
  .install-form {
    margin-top: var(--s3);
    padding-top: var(--s3);
    border-top: 1px dashed var(--border);
  }
  /* 文件选择框：原生 file input 很突兀，统一外观 */
  .install-form input[type='file'] {
    padding: 7px 10px;
    font-size: 12.5px;
    color: var(--text-2);
    background: var(--surface);
    cursor: pointer;
  }
  .install-form input[type='file']::file-selector-button {
    margin-right: 10px;
    padding: 5px 12px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-sm);
    background: var(--surface-3);
    color: var(--text);
    font-family: inherit;
    font-size: 12.5px;
    cursor: pointer;
    transition: background var(--t-fast);
  }
  .install-form input[type='file']::file-selector-button:hover {
    background: var(--surface-hover);
  }
  .row-actions {
    display: flex;
    gap: var(--s2);
  }
</style>
