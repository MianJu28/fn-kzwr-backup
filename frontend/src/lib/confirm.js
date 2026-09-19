/**
 * 全局确认弹窗（替代原生 window.confirm，风格统一且可键盘操作）
 *
 * 用法：
 *   if (await confirmDialog({ title: '删除配置', message: '...', danger: true })) { ... }
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
 * @returns {Promise<boolean>}
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
  });
  return new Promise((resolve) => {
    resolver = resolve;
  });
}

/** 由 <ConfirmDialog /> 调用 */
export function answerConfirm(value) {
  confirmState.set(null);
  const r = resolver;
  resolver = null;
  if (r) r(!!value);
}
