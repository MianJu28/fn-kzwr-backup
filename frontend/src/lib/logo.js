/**
 * Logo 图形库
 *
 * 设计语言（与飞牛 fnOS / 酷族 kzwr 一致）：蓝色圆角方块 + 白色单色几何符号，
 * 无渐变叠加、无内外描边特效，保证 16px 下仍可辨识。
 *
 * 共 5 套方案，切换只需改 `LOGO_VARIANT`：
 *   a 盾牌 + 上升箭头  —— 加密防护 + 备份上传
 *   b 云 + 上升箭头    —— 备份到云端
 *   c 折角双箭头       —— 增量、逐级上传
 *   d 阶梯 + 宽箭头    —— 箭头横跨三级台阶、与台阶留小幅间隙 ★ 当前启用
 *   e 阶梯 + 宽箭头    —— 同上，箭头再抬高一点、间隙更大
 */

/** 当前启用的方案 */
export const LOGO_VARIANT = 'd';

/** 蓝色底（与飞牛同族色，浅→深轻微渐变，保持扁平观感） */
export const LOGO_TILE = (id) => `
  <defs>
    <linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#3b7bf0"/>
      <stop offset="100%" stop-color="#2a5cd8"/>
    </linearGradient>
  </defs>
  <rect width="48" height="48" rx="11" fill="url(#${id})"/>`;

/** 白色符号（viewBox 48×48，安全区 8~40） */
export const LOGO_MARKS = {
  a: `
    <path d="M24 9.6 36.2 14.4V25.4C36.2 32.6 30.7 37.9 24 40.2 17.3 37.9 11.8 32.6 11.8 25.4V14.4Z"
          fill="none" stroke="#fff" stroke-width="3.4" stroke-linejoin="round"/>
    <path d="M24 32.2V19.6" stroke="#fff" stroke-width="3.4" stroke-linecap="round"/>
    <path d="M18.6 25 24 19.6 29.4 25" fill="none" stroke="#fff" stroke-width="3.4"
          stroke-linecap="round" stroke-linejoin="round"/>`,
  b: `
    <path d="M17.6 34.4h13.8a7.3 7.3 0 0 0 1.1-14.5 10.4 10.4 0 0 0-19.7-1.5 7.8 7.8 0 0 0 4.8 16z"
          fill="none" stroke="#fff" stroke-width="3.1" stroke-linejoin="round"/>
    <path d="M24 31.8V21.4" stroke="#fff" stroke-width="3.1" stroke-linecap="round"/>
    <path d="M19.7 25.7 24 21.4 28.3 25.7" fill="none" stroke="#fff" stroke-width="3.1"
          stroke-linecap="round" stroke-linejoin="round"/>`,
  c: `
    <path d="M13.8 27.6 24 17.6l10.2 10" fill="none" stroke="#fff" stroke-width="3.7"
          stroke-linecap="round" stroke-linejoin="round"/>
    <path d="M24 17.6V34.4" stroke="#fff" stroke-width="3.7" stroke-linecap="round"/>
    <path d="M16.8 37.8 24 30.8l7.2 7" fill="none" stroke="#fff" stroke-width="3.7"
          stroke-linecap="round" stroke-linejoin="round"/>`,
  d: `
    <path d="M13.8 36H20.4V29.4H27V22.8H33.6" fill="none" stroke="#fff" stroke-width="3"
          stroke-linecap="round" stroke-linejoin="round"/>
    <path d="M14.2 17.8 24 11.6 33.8 17.8" fill="none" stroke="#fff" stroke-width="3.4"
          stroke-linecap="round" stroke-linejoin="round"/>`,
  e: `
    <path d="M13.8 36H20.4V29.4H27V22.8H33.6" fill="none" stroke="#fff" stroke-width="3"
          stroke-linecap="round" stroke-linejoin="round"/>
    <path d="M14.2 16.4 24 10.2 33.8 16.4" fill="none" stroke="#fff" stroke-width="3.4"
          stroke-linecap="round" stroke-linejoin="round"/>`,
};

/**
 * 生成完整 logo 的 SVG 内部标记。
 * @param {string} [variant] 方案 id（a/b/c/d），默认取 LOGO_VARIANT
 * @param {string} [gradientId] 渐变 id（同一页面多个实例需唯一）
 */
export function logoMarkup(variant = LOGO_VARIANT, gradientId = 'logo-grad') {
  const mark = LOGO_MARKS[variant] || LOGO_MARKS[LOGO_VARIANT];
  return `${LOGO_TILE(gradientId)}${mark}`;
}

/** 生成独立 SVG 文件内容（用于 favicon / 应用图标导出） */
export function logoSvgDocument(variant = LOGO_VARIANT, size = 48) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 48 48">
${logoMarkup(variant, 'g')}
</svg>`;
}
