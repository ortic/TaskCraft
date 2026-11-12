// Interface messages: every `t('…')` in src (also when imported as `tr` / `_t`).
//   node scripts/i18n.mjs extract        writes src/locales/messages.json (the sorted keys)
//   node scripts/i18n.mjs check [code…]  lists keys missing from (and unused in) catalogs; exit 1 if any
// Catalogs are src/locales/<code>.json; English needs entries only for plural forms.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'src')
const localesDir = path.join(root, 'locales')

function files(dir, out = []) {
  for (const f of fs.readdirSync(dir)) {
    const p = path.join(dir, f)
    if (fs.statSync(p).isDirectory()) files(p, out)
    else if (/\.tsx?$/.test(p) && !p.endsWith('.d.ts')) out.push(p)
  }
  return out
}

/** The keys of one file: first arguments (string literals) of the names bound to i18n's `t`. */
function keysOf(file) {
  const sf = ts.createSourceFile(file, fs.readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true, file.endsWith('x') ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  const names = new Set()
  for (const s of sf.statements) {
    if (ts.isImportDeclaration(s) && /(^|\/)i18n$/.test(s.moduleSpecifier.text) && s.importClause?.namedBindings && ts.isNamedImports(s.importClause.namedBindings)) {
      for (const el of s.importClause.namedBindings.elements) if ((el.propertyName ?? el.name).text === 't') names.add(el.name.text)
    }
  }
  if (file.endsWith(`${path.sep}i18n.ts`)) return []
  const out = []
  const visit = (n) => {
    if (ts.isCallExpression(n) && ts.isIdentifier(n.expression) && names.has(n.expression.text) && n.arguments[0]) {
      const a = n.arguments[0]
      if (ts.isStringLiteral(a) || ts.isNoSubstitutionTemplateLiteral(a)) out.push(a.text)
      else console.warn(`${path.relative(root, file)}: t() with a computed key (${a.getText(sf).slice(0, 60)})`)
    }
    ts.forEachChild(n, visit)
  }
  visit(sf)
  return out
}

const keys = [...new Set(files(root).flatMap(keysOf))].sort((a, b) => a.localeCompare(b))
const [cmd, ...codes] = process.argv.slice(2)

if (cmd === 'extract') {
  fs.writeFileSync(path.join(localesDir, 'messages.json'), JSON.stringify(keys, null, 1) + '\n')
  console.log(`${keys.length} messages`)
} else if (cmd === 'check') {
  const langs = codes.length ? codes : fs.readdirSync(localesDir).filter((f) => /^[a-z]{2}\.json$/.test(f) && f !== 'en.json').map((f) => f.slice(0, 2))
  let bad = false
  for (const code of langs) {
    const cat = JSON.parse(fs.readFileSync(path.join(localesDir, `${code}.json`), 'utf8'))
    const missing = keys.filter((k) => !(k in cat))
    const unused = Object.keys(cat).filter((k) => !keys.includes(k))
    // every placeholder must survive translation (the `one` form may spell out its number), and none may be invented
    const broken = keys.filter((k) => k in cat).filter((k) => {
      const want = new Set(k.match(/\{\w+\}/g) ?? [])
      const forms = typeof cat[k] === 'string' ? { other: cat[k] } : cat[k]
      return Object.entries(forms).some(([form, v]) => {
        const have = new Set(v.match(/\{\w+\}/g) ?? [])
        return [...have].some((x) => !want.has(x)) || (form !== 'one' && [...want].some((x) => !have.has(x)))
      })
    })
    console.log(`${code}: ${keys.length - missing.length}/${keys.length} translated, ${unused.length} unused, ${broken.length} with lost placeholders`)
    for (const k of missing.slice(0, 20)) console.log(`  missing: ${k}`)
    for (const k of broken.slice(0, 20)) console.log(`  placeholders: ${k}`)
    if (missing.length || broken.length) bad = true
  }
  process.exit(bad ? 1 : 0)
} else {
  console.log('usage: node scripts/i18n.mjs extract | check [code…]')
}
