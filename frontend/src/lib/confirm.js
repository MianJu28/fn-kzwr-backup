/**
 * 全局确认弹窗（替代原生 window.confirm，风格统一且可键盘操作）
 *
 * 用法：
 *   if (await confirmDialog({ title: '删除配置', message: '...', danger: true })) { ... }
 *   // 带口令输入：确认时 resolve 输入的字符串（取消 resolve false）
 *   const pass = await confirmDialog({ title: '...', input: true, placeholder: '管理员口令' });
 *   if (pass === false) return;
 * 渲染由 <ConfirmDialog /> 负责（挂在 App 根部）。
 */
import { writable } from 'svelte/store';

/** null 表示未打开 */
export const confirmState = writable(null);

let resolver = null;

/**
 * @param {object} opts
 * @param {string} opts.title       标题
 * @param {string} opts.message     正文（支持换行）
 * @param {string} [opts.confirmText] 确认按钮文案
 * @param {string} [opts.cancelText]  取消按钮文案
 * @param {boolean} [opts.danger]     危险操作（红色确认按钮）
 * @param {boolean} [opts.input]      显示输入框（确认时 resolve 输入字符串，取消 resolve false）
 * @param {string} [opts.placeholder] 输入框占位符
 * @param {string} [opts.inputType]   输入框类型（默认 password）
 * @returns {Promise<boolean|string>} 常规模式 resolve 布尔；input 模式 resolve 字符串或 false
 */
export function confirmDialog(opts = {}) {
  // 若已有弹窗未处理，先取消上一个，避免 promise 悬挂
  if (resolver) {
    resolver(false);
    resolver = null;
  }
  confirmState.set({
    title: opts.title || '请确认',
    message: opts.message || '',
    confirmText: opts.confirmText || '确定',
    cancelText: opts.cancelText || '取消',
    danger: !!opts.danger,
    input: !!opts.input,
    placeholder: opts.placeholder || '',
    inputType: opts.inputType || 'password',
  });
  return new Promise((resolve) => {
    resolver = resolve;
  });
}

/** 由 <ConfirmDialog /> 调用；value 为 true/false（常规）或输入字符串（input 模式） */
export function answerConfirm(value) {
  confirmState.set(null);
  const r = resolver;
  resolver = null;
  if (r) r(value === undefined ? false : value);
}
