// The workspace's accent colour (Administration → Overview). One colour is turned into the
// light- and dark-theme accent tokens, darkened or lightened until text on it and links in it
// keep WCAG AA contrast. The high-contrast theme keeps its own colours.

type Rgb = [number, number, number]

const WHITE: Rgb = [255, 255, 255]
const BLACK: Rgb = [0, 0, 0]
const DARK_BG: Rgb = [0x15, 0x17, 0x1c]

function parse(hex: string): Rgb | null {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim())
  return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : null
}

function hex(c: Rgb): string {
  return '#' + c.map((v) => Math.round(Math.min(255, Math.max(0, v))).toString(16).padStart(2, '0')).join('')
}

/** `t` of the way from `a` to `b`. */
function mix(a: Rgb, b: Rgb, t: number): Rgb {
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

function luminance(c: Rgb): number {
  const [r, g, b] = c.map((v) => {
    const s = v / 255
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * r + 0.7152 * g + 0.0722 * b
}

export function contrast(a: Rgb, b: Rgb): number {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p)
  return (x + 0.05) / (y + 0.05)
}

/** Moves `c` towards `to` until it has `ratio` contrast against `against`. */
function until(c: Rgb, to: Rgb, against: Rgb, ratio: number): Rgb {
  let out = c
  for (let i = 0; i < 30 && contrast(out, against) < ratio; i++) out = mix(out, to, 0.08)
  return out
}

/** The CSS for one accent colour (exported for tests). */
export function accentCss(color: string): string | null {
  const c = parse(color)
  if (!c) return null
  // light: white button text and links on white both need 4.5:1
  const light = until(c, BLACK, WHITE, 4.5)
  const lightSoft = mix(light, WHITE, 0.92)
  const lightText = until(mix(light, BLACK, 0.25), BLACK, lightSoft, 7)
  const lightActive = mix(light, WHITE, 0.88)
  // dark: links on the dark background need 4.5:1
  const dark = until(c, WHITE, DARK_BG, 4.5)
  const darkSoft = mix(dark, DARK_BG, 0.82)
  const darkText = until(mix(dark, WHITE, 0.55), WHITE, darkSoft, 7)
  const darkActive = mix(dark, DARK_BG, 0.8)
  // button text on a light accent (dark theme) turns dark
  const darkOn = contrast(dark, WHITE) >= 4.5 ? '#ffffff' : '#111114'
  const L = `--accent: ${hex(light)}; --accent-soft: ${hex(lightSoft)}; --accent-text: ${hex(lightText)}; --bg-active: ${hex(lightActive)}; --on-accent: #ffffff;`
  const D = `--accent: ${hex(dark)}; --accent-soft: ${hex(darkSoft)}; --accent-text: ${hex(darkText)}; --bg-active: ${hex(darkActive)}; --on-accent: ${darkOn};`
  return [
    `:root[data-theme='light'], :root:not([data-theme]) { ${L} }`,
    `@media (prefers-color-scheme: dark) { :root:not([data-theme]) { ${D} } }`,
    `:root[data-theme='dark'] { ${D} }`,
    // the system's "more contrast" setting keeps the high-contrast colours
    `@media (prefers-contrast: more) and (prefers-color-scheme: light) { :root:not([data-theme]) { --accent: #2b1db0; --accent-soft: #e2e2ff; --accent-text: #1b127a; --bg-active: #dcdcff; --on-accent: #ffffff; } }`,
  ].join('\n')
}

/** Applies the workspace accent colour (null = the built-in one). */
export function applyAccent(color: string | null | undefined) {
  const css = color ? accentCss(color) : null
  let el = document.getElementById('workspace-accent') as HTMLStyleElement | null
  if (!css) {
    el?.remove()
    return
  }
  if (!el) {
    el = document.createElement('style')
    el.id = 'workspace-accent'
    document.head.appendChild(el)
  }
  if (el.textContent !== css) el.textContent = css
}
