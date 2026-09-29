<script>
  /**
   * 目标编辑弹窗（**schema 驱动**：表单完全由目标插件声明）
   *
   * 设计原则与插件设置弹窗（`PluginSettingsModal` + `PluginBlocks`）一致：
   * 宿主**只负责渲染与存取**，不预设任何字段语义。插件的 `describe_json.target.form`
   * 声明要哪些字段、什么类型、是否必填/敏感，本弹窗按声明渲染。
   *
   * ## 为什么用弹窗
   * 目标编辑含「名称 + 类型 + N 个插件自定义字段 + 启用开关」，内联展开会把列表淹没；
   * 弹窗让「配置一个目标」有明确的开始/结束。
   *
   * ## 字段的三条来源
   * 1. **宿主内置**：`name`（名称）、`enabled`（启用）—— 所有目标都有，故由宿主渲染；
   * 2. **插件声明**：`plugin.form`（url/username/password 是 well-known 键，其余进目标自定义字段）；
   * 3. **回退**：插件未声明 `form` 时用内置的 WebDAV 默认表单（老插件零改动）。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';

  /** 正在编辑的目标（null = 关闭）；`{id:'',kind:...}` = 新建 */
  export let editing = null;
  /** 可选的目标插件（`kind==='target'` 且未禁用） */
  export let targetPlugins = [];
  /** 保存成功后的回调（刷新目标列表） */
  export let onSaved = null;
  /** 关闭回调 */
  export let onClose = null;

  let saving = false;
  /** 字段值：key -> 字符串（toggle 用 '1'/'0'，secret 用「是否已设置」布尔另存） */
  let values = {};
  /** 字段键 -> 是否已设置（仅敏感字段用；回显时不带明文） */
  let secretSet = {};
  let formMsg = '';
  let formOk = true;
  /** 防止把外层刷新导致的对象换引用当成「切换了目标」 */
  let initedKey = null;

  $: plugin = (targetPlugins || []).find((p) => p.id === editing?.kind) || null;

  /**
   * 插件声明的字段；插件没声明则用**内置 WebDAV 默认表单**
   *
   * 回退是必要的：老插件（未声明 `form`）必须继续能编辑，
   * 且 WebDAV 的 url/username/password 语义是既成契约。
   */
  $: fields = (() => {
    const declared = (plugin && plugin.form) || [];
    if (declared.length) return declared;
    // 回退：按 WebDAV 语义给默认表单（与 v0.4.8 之前的硬编码表单等价）
    return [
      {
        key: 'url',
        label: plugin?.url_label || '地址',
        kind: 'text',
        required: true,
        placeholder: plugin?.url_placeholder || 'https://dav.kzwr.com/dav',
        hint: plugin?.url_hint || '留空则用官方默认地址 https://dav.kzwr.com/dav',
      },
      ...(plugin?.needs_credentials === false
        ? []
        : [
            {
              key: 'username',
              label: '账号',
              kind: 'text',
              required: true,
              placeholder: '酷族用户名 / 邮箱',
            },
            {
              key: 'password',
              label: '应用密码',
              kind: 'password',
              secret: true,
              required: false,
              placeholder: '在 kzwr 官网「应用密码」创建',
              hint: '保存前会实测一次连通性；建议选择「永不过期」与读写权限',
            },
          ]),
    ];
  })();

  /** 切换目标时初始化表单（外层刷新会换对象引用，故用 id+kind 作键） */
  $: {
    const key = editing ? `${editing.id || 'new'}:${editing.kind}` : null;
    if (key && key !== initedKey) {
      initedKey = key;
      const next = {};
      const sec = {};
      const src = editing.fields || {};
      for (const f of fields) {
        if (f.key === 'username') {
          next[f.key] = editing.username || '';
        } else if (f.key === 'password') {
          next[f.key] = ''; // 密码永不回显明文
          sec[f.key] = !!editing.password_set;
        } else if (f.kind === 'toggle') {
          // 布尔字段：'1'/'0'；未设置则用 default
          const v = src[f.key];
          next[f.key] = v === true || v === '1' || v === 'true' ? '1' : v === false || v === '0' ? '0' : (f.default === '1' ? '1' : '0');
        } else if (f.kind === 'password') {
          next[f.key] = '';
          // 敏感字段：后端回显的是布尔（是否已设置），不是明文
          sec[f.key] = src[f.key] === true;
        } else {
          next[f.key] = src[f.key] != null ? String(src[f.key]) : (f.default || '');
        }
      }
      values = next;
      secretSet = sec;
      formMsg = '';
      formOk = true;
    }
  }

  function setValue(key, v) {
    values = { ...values, [key]: v };
  }

  function close() {
    if (onClose) onClose();
  }

  function onKeydown(e) {
    if (e.key === 'Escape' && !saving) close();
  }

  /** 必填校验（只用插件声明的 required；密码编辑时留空 = 不修改，故不算缺失） */
  function validate() {
    for (const f of fields) {
      if (!f.required) continue;
      const v = (values[f.key] ?? '').toString().trim();
      if (f.kind === 'password' && editing.id && !v) continue; // 编辑时留空 = 保持原值
      if (!v) return `请填写${f.label}`;
    }
    return null;
  }

  async function save() {
    const bad = validate();
    if (bad) {
      formMsg = bad;
      formOk = false;
      return;
    }
    saving = true;
    formMsg = '正在保存…';
    formOk = true;
    try {
      const body = {
        name: (values.__name ?? editing.name ?? '').trim() || undefined,
        kind: editing.kind,
        enabled: values.__enabled !== '0',
        // 插件字段统一走 `fields`；url/username/password 由后端归位到既有存储
        fields: {},
      };
      if (editing.id) body.id = editing.id;
      for (const f of fields) {
        const raw = (values[f.key] ?? '').toString();
        if (f.kind === 'password') {
          // 敏感字段留空 = 不修改（与既有 password 语义一致）
          if (raw) body.fields[f.key] = raw;
        } else {
          body.fields[f.key] = raw;
        }
      }
      // 后端仍从顶层读 url/username/password；为兼容老路径也带上（值为空则省略）
      if (body.fields.url != null) body.url = body.fields.url;
      if (body.fields.username) body.username = body.fields.username;
      if (body.fields.password) body.password = body.fields.password;

      const r = await api.saveTarget(body);
      if (r.error) {
        formMsg = r.error;
        formOk = false;
        return;
      }
      if (r.warning) toast.success(r.warning, '已保存');
      else toast.success('目标已保存', '已保存');
      if (onSaved) await onSaved();
      close();
    } catch (e) {
      formMsg = e.message;
      formOk = false;
    } finally {
      saving = false;
    }
  }
</script>

<svelte:window on:keydown={onKeydown} />

{#if editing}
  <div class="scrim" role="presentation" on:click|self={close}>
    <div class="modal target-modal" role="dialog" aria-modal="true"
      aria-label={editing.id ? '编辑目标' : '新建目标'}>
      <div class="modal-title">
        <Icon name="server" size={18} />
        <span class="grow">{editing.id ? '编辑目标' : '新建目标'}</span>
        <button class="btn-icon" on:click={close} aria-label="关闭" disabled={saving}>
          <Icon name="x" size={16} />
        </button>
      </div>

      <div class="modal-body scroll-body">
        <!-- 名称：所有目标都有，由宿主渲染 -->
        <div class="field">
          <label for="tg-name">名称</label>
          <input id="tg-name" placeholder="如：酷族主账号" value={values.__name ?? editing.name ?? ''}
            on:input={(e) => setValue('__name', e.target.value)} disabled={saving} />
        </div>

        <!-- 类型：来自目标插件清单；创建后不可更改（换类型=换一种存储） -->
        <div class="field">
          <label for="tg-kind">类型</label>
          {#if editing.id}
            <input id="tg-kind" value={plugin?.name || editing.kind} disabled />
            <p class="field-hint">类型创建后不可更改（如需换类型请新建目标）</p>
          {:else}
            <select id="tg-kind" value={editing.kind}
              on:change={(e) => onClose && onClose({ switchKind: e.target.value })} disabled={saving}>
              {#each targetPlugins as p (p.id)}
                <option value={p.id}>{p.name}{p.builtin ? '' : '（外置插件）'}</option>
              {/each}
            </select>
            <p class="field-hint">每种类型对应一个目标插件；表单由该插件声明</p>
          {/if}
        </div>

        <!-- 插件声明的字段：宿主只渲染，不解释语义 -->
        {#each fields as f (f.key)}
          <div class="field">
              {#if f.kind === 'toggle'}
                <div class="field-row">
                  <label for="tf-{f.key}">{f.label}</label>
                  <input id="tf-{f.key}" type="checkbox" checked={values[f.key] === '1'}
                    on:change={(e) => setValue(f.key, e.target.checked ? '1' : '0')} disabled={saving} />
                </div>
              {:else if f.kind === 'select'}
                <label for="tf-{f.key}">{f.label}{f.required ? ' *' : ''}</label>
                <select id="tf-{f.key}" value={values[f.key] ?? ''}
                  on:change={(e) => setValue(f.key, e.target.value)} disabled={saving}>
                  <option value="">（未选择）</option>
                  {#each f.options || [] as o (o.value)}
                    <option value={o.value}>{o.label || o.value}</option>
                  {/each}
                </select>
              {:else}
                <label for="tf-{f.key}">{f.label}{f.required ? ' *' : ''}</label>
                <input
                  id="tf-{f.key}"
                  type={f.kind === 'password' ? 'password' : f.kind === 'number' ? 'number' : 'text'}
                  placeholder={f.kind === 'password' && secretSet[f.key]
                    ? '留空 = 不修改已保存的值'
                    : (f.placeholder || '')}
                  value={values[f.key] ?? ''}
                  on:input={(e) => setValue(f.key, e.target.value)}
                  disabled={saving}
                />
              {/if}
              {#if f.hint}
                <p class="field-hint">{f.hint}</p>
              {/if}
              {#if f.kind === 'password' && secretSet[f.key]}
                <p class="field-hint">已设置（留空则保持不变）</p>
              {/if}
          </div>
        {/each}

        <!-- 启用：所有目标都有，由宿主渲染 -->
        <label class="switch-row">
          <input type="checkbox" checked={values.__enabled !== '0'}
            on:change={(e) => setValue('__enabled', e.target.checked ? '1' : '0')} disabled={saving} />
          <span>启用该目标</span>
        </label>

        {#if formMsg}
          <div class="alert {formOk ? 'alert-ok' : 'alert-warn'}">
            <Icon name={formOk ? 'check' : 'alert'} size={15} />
            <div class="alert-body">{formMsg}</div>
          </div>
        {/if}
      </div>

      <div class="modal-actions">
        <button class="btn btn-primary" on:click={save} disabled={saving}>
          {saving ? '保存中…' : editing.id ? '保存修改' : '创建目标'}
        </button>
        <button class="btn btn-ghost" on:click={close} disabled={saving}>取消</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .target-modal {
    max-width: 560px;
    width: 100%;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .scroll-body {
    max-height: min(62vh, 560px);
    overflow-y: auto;
    margin-bottom: var(--s4);
    padding-right: 2px;
  }
</style>
