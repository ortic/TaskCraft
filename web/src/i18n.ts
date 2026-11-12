// Interface translations. The English text is the key: `t('Save')`, `t('Delete {name}?', { name })`.
// Text without a translation stays English, so a catalog can grow one string at a time.
// Catalogs live in `locales/<code>.json`: `{"Save": "Speichern"}`; plural forms are objects keyed
// by Intl.PluralRules categories: `{"{count} results": {"one": "1 Ergebnis", "other": "{count} Ergebnisse"}}`.
// The language is chosen before the app loads (so module-level labels are translated too):
// the person's profile, else the workspace default, else the browser's languages.

export interface Language {
  code: string
  /** the language's own name */
  name: string
  rtl?: boolean
}

export const LANGUAGES: Language[] = [
  { code: 'en', name: 'English' },
  { code: 'de', name: 'Deutsch' },
  { code: 'fr', name: 'Français' },
  { code: 'es', name: 'Español' },
  { code: 'pt', name: 'Português' },
  { code: 'it', name: 'Italiano' },
  { code: 'nl', name: 'Nederlands' },
  { code: 'pl', name: 'Polski' },
  { code: 'ja', name: '日本語' },
  { code: 'zh', name: '中文' },
  { code: 'ko', name: '한국어' },
  { code: 'ar', name: 'العربية', rtl: true },
  { code: 'he', name: 'עברית', rtl: true },
]

type Entry = string | Partial<Record<Intl.LDMLPluralRule, string>>

const loaders = import.meta.glob<{ default: Record<string, Entry> }>('./locales/[a-z][a-z].json')

/** The languages people can pick: English and the ones with a catalog. */
export const AVAILABLE: Language[] = LANGUAGES.filter((l) => l.code === 'en' || `./locales/${l.code}.json` in loaders)

let catalog: Record<string, Entry> = {}
let current = 'en'
let plurals = new Intl.PluralRules('en')

const STORAGE_KEY = 'tc-lang'

/** The interface language (a code of {@link LANGUAGES}). */
export function locale(): string {
  return current
}

/** Translates `key` (English text) and fills `{name}` placeholders from `vars`; `vars.count` picks the plural form.
 *  A key may end in a note for translators, `'Open [[task filter]]'`, when the same English word
 *  needs different translations; it isn't shown. */
export function t(key: string, vars?: Record<string, unknown>): string {
  const entry = catalog[key]
  let msg: string
  if (entry === undefined) msg = key.replace(/ \[\[[^\]]*\]\]$/, '')
  else if (typeof entry === 'string') msg = entry
  else {
    // the plural form follows `count`, else the first number among the values
    const n = vars?.count ?? Object.values(vars ?? {}).find((v) => typeof v === 'number')
    msg = entry[plurals.select(Number(n ?? 0))] ?? entry.other ?? key
  }
  if (!vars) return msg
  return msg.replace(/\{(\w+)\}/g, (m, name: string) => (name in vars ? String(vars[name] ?? '') : m))
}

/** A supported language code for `tag` (`de-CH` → `de`), or undefined. */
export function supported(tag: string | null | undefined): string | undefined {
  if (!tag) return undefined
  const base = tag.toLowerCase().split(/[-_]/)[0]
  return AVAILABLE.some((l) => l.code === base) ? base : undefined
}

/** The language to use: profile, workspace default, the browser's preferences, English. */
export function chooseLanguage(profile?: string | null, workspace?: string | null): string {
  return supported(profile) ?? supported(workspace) ?? (navigator.languages ?? [navigator.language]).map(supported).find(Boolean) ?? 'en'
}

/** The language picked last time (until /me says otherwise). */
export function rememberedLanguage(): string | undefined {
  try {
    return supported(localStorage.getItem(STORAGE_KEY))
  } catch {
    return undefined
  }
}

export function rememberLanguage(code: string) {
  try {
    localStorage.setItem(STORAGE_KEY, code)
  } catch {
    /* storage unavailable */
  }
}

/** Loads a language's catalog and applies it (document language and direction too). */
export async function loadLanguage(code: string): Promise<void> {
  const lang = LANGUAGES.find((l) => l.code === code) ?? LANGUAGES[0]
  const loader = loaders[`./locales/${lang.code}.json`]
  try {
    catalog = loader ? (await loader()).default : {}
  } catch {
    catalog = {}
  }
  current = lang.code
  plurals = new Intl.PluralRules(lang.code)
  document.documentElement.lang = lang.code
  document.documentElement.dir = lang.rtl ? 'rtl' : 'ltr'
}
