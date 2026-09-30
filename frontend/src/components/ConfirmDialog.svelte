<script>
  // 全局确认弹窗（由 lib/confirm.js 的 confirmDialog() 驱动）
  // input 模式：显示口令/文本输入框，确认时把输入值回传给 confirmDialog 的 Promise
  //
  // ⚠️ 不要写成 `$: state = $confirmState;` + `$: if (state) inputValue = '';`：
  //    当绑定变量（inputValue）被响应式语句写入时，Svelte 会在输入回调里**连带把该语句
  //    的依赖（state、$confirmState）置脏**，于是「重置输入」在每次按键后重跑，
  //    用户刚敲进去的字符立刻被清空（症状：口令框完全输入不了字符）。
  //    改法：显式订阅 store，只在弹窗状态真正变化时重置输入并聚焦。
  import { onDestroy, tick } from 'svelte';
  import { confirmState, answerConfirm } from '../lib/confirm.js';
  import Icon from './Icon.svelte';

  let confirmBtn = null;
  let inputEl = null;
  let state = null;
  let inputValue = '';
  /** choices 模式下用户的选择（**初始 null = 无预选**，未选则确认按钮禁用） */
  let choice = null;

  const unsub = confirmState.subscribe((s) => {
    state = s;
    inputValue = '';
    choice = null;
    if (s) {
      tick().then(() => {
        if (s.input) inputEl?.focus();
        else confirmBtn?.focus();
      });
    }
  });
  onDestroy(unsub);

  function onKey(e) {
    if (!state) return;
    if (e.key === 'Escape') answerConfirm(false);
  }
</script>

<svelte:window on:keydown={onKey} />

{#if state}
  <div
    class="scrim"
    role="presentation"
    on:click|self={() => answerConfirm(false)}
  >
    <div class="modal" role="dialog" aria-modal="true" aria-label={state.title}>
      <div class="modal-title" class:danger={state.danger}>
        <Icon name={state.danger ? 'alert' : 'info'} size={19} />
        <span>{state.title}</span>
      </div>
      {#if state.message}
        <div class="modal-body">{state.message}</div>
      {/if}
      {#if state.input}
        <!-- svelte-ignore a11y-autofocus -->
        <input
          class="input modal-input mono"
          type="password"
          bind:value={inputValue}
          bind:this={inputEl}
          placeholder={state.placeholder || '管理员口令'}
          autocomplete="off"
          on:keydown={(e) => e.key === 'Enter' && answerConfirm(inputValue)}
        />
      {/if}
      {#if state.choices}
        <div class="choice-list">
          {#each state.choices as c}
            <label class="choice" class:sel={choice === c.value}>
              <input
                type="radio"
                name="confirm-choice"
                value={c.value}
                checked={choice === c.value}
                on:change={() => (choice = c.value)}
              />
              <span class="choice-body">
                <span class="choice-label" class:danger={c.danger}>{c.label}</span>
                {#if c.desc}<span class="choice-desc">{c.desc}</span>{/if}
              </span>
            </label>
          {/each}
        </div>
      {/if}
      <div class="modal-actions">
        <button class="btn btn-ghost" on:click={() => answerConfirm(false)}>
          {state.cancelText}
        </button>
        <button
          class="btn {state.danger ? 'btn-danger' : 'btn-primary'}"
          bind:this={confirmBtn}
          disabled={state.choices && !choice}
          on:click={() =>
            answerConfirm(state.choices ? choice : state.input ? inputValue : true)}
        >
          {state.confirmText}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-title.danger {
    color: var(--danger);
  }
  .modal-input {
    width: 100%;
    margin: 0;
  }
  /* 单选组：每个选项是一整块可点区域，选中态有明显边框 */
  .choice-list {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    margin-top: var(--s3);
  }
  .choice {
    display: flex;
    align-items: flex-start;
    gap: var(--s3);
    padding: var(--s3);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    cursor: pointer;
    background: var(--surface-2);
  }
  .choice.sel {
    border-color: var(--primary);
    background: var(--surface);
  }
  .choice input {
    margin-top: 2px;
    flex-shrink: 0;
  }
  .choice-body {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .choice-label {
    font-size: 13.5px;
    font-weight: 600;
    color: var(--text);
  }
  .choice-label.danger {
    color: var(--danger);
  }
  .choice-desc {
    font-size: 12.5px;
    color: var(--text-3);
    white-space: pre-wrap;
  }
  /* 输入模式：输入框与按钮之间留足间距，避免拥挤 */
  .modal-actions {
    margin-top: var(--s4);
  }
</style>
