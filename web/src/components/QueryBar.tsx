// TQL input with autocompletion (fields, operators, values) and live validation.
import { t } from '../i18n'
import { useEffect, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Search, CircleQuestionMark } from 'lucide-react'
import { api } from '../api'
import { PopButton } from '../ui'

const OPERATORS = ['=', '!=', '~', 'in', 'not in', '>', '>=', '<', '<=', 'is empty', 'is not empty']

/** What to suggest at the cursor: a field, an operator after a field, or a value after an operator. */
function context(text: string): { kind: 'field' | 'op' | 'value'; field?: string; prefix: string; start: number } {
  const before = text
  const m = /("[^"]*"?|[^\s(),=<>!~]+)?$/.exec(before)
  const prefix = (m?.[0] ?? '').replace(/^"/, '')
  const start = before.length - (m?.[0]?.length ?? 0)
  const head = before.slice(0, start).trimEnd()
  const tokens = head.match(/"[^"]*"|!=|>=|<=|[=<>~]|!~|[^\s(),=<>!~]+|[(),]/g) ?? []
  const last = tokens[tokens.length - 1]?.toLowerCase()
  const prev = tokens[tokens.length - 2]?.toLowerCase()
  const isOp = (s?: string) => !!s && ['=', '!=', '~', '!~', '>', '>=', '<', '<=', 'in', '('].includes(s)
  if (isOp(last) || (last === ',' && (tokens as string[]).includes('('))) {
    let i = tokens.length - 1
    while (i >= 0 && !['=', '!=', '~', '!~', '>', '>=', '<', '<='].includes(tokens[i]) && tokens[i].toLowerCase() !== 'in') i--
    const f = tokens[i - 1] === 'not' ? tokens[i - 2] : tokens[i - 1]
    return { kind: 'value', field: f?.replace(/"/g, ''), prefix, start }
  }
  if (last && !['and', 'or', 'not', '(', 'by', ','].includes(last) && !isOp(prev) && prev !== 'is' && last !== 'order') {
    if (!/^(and|or|not)$/i.test(last) && tokens.length && !isOp(last)) return { kind: 'op', field: last, prefix, start }
  }
  return { kind: 'field', prefix, start }
}

export default function QueryBar({ value, onChange, placeholder }: { value: string; onChange: (q: string) => void; placeholder?: string }) {
  const [text, setText] = useState(value)
  const [focus, setFocus] = useState(false)
  const [sel, setSel] = useState(0)
  const [cursor, setCursor] = useState(value.length)
  const ref = useRef<HTMLInputElement>(null)
  useEffect(() => setText(value), [value])
  const ctx = context(text.slice(0, cursor))
  const fields = useQuery({ queryKey: ['tql-fields'], queryFn: () => api.get<string[]>('/query/suggest'), staleTime: 300_000, enabled: focus })
  const values = useQuery({
    queryKey: ['tql-values', ctx.field],
    queryFn: () => api.get<string[]>(`/query/suggest?field=${encodeURIComponent(ctx.field ?? '')}`),
    enabled: focus && ctx.kind === 'value' && !!ctx.field,
    staleTime: 60_000,
  })
  const [debounced, setDebounced] = useState(text)
  useEffect(() => {
    const h = setTimeout(() => setDebounced(text), 350)
    return () => clearTimeout(h)
  }, [text])
  const valid = useQuery({ queryKey: ['tql-valid', debounced], queryFn: () => api.post<{ ok: boolean; message?: string }>('/query/validate', { q: debounced }), enabled: debounced.trim().length > 0, staleTime: 60_000 })
  const pool = ctx.kind === 'field' ? (fields.data ?? []) : ctx.kind === 'op' ? OPERATORS : (values.data ?? [])
  const p = ctx.prefix.toLowerCase()
  const suggestions = pool.filter((s) => s.toLowerCase().replace(/^"/, '').startsWith(p) && s.toLowerCase() !== p).slice(0, 12)
  useEffect(() => setSel(0), [ctx.kind, ctx.prefix])
  const apply = (s: string) => {
    const before = text.slice(0, ctx.start)
    const after = text.slice(cursor)
    const insert = s + ' '
    const next = before + insert + after.trimStart()
    setText(next)
    const pos = (before + insert).length
    setCursor(pos)
    requestAnimationFrame(() => {
      ref.current?.focus()
      ref.current?.setSelectionRange(pos, pos)
    })
  }
  const invalid = valid.data && !valid.data.ok && debounced === text
  return (
    <div style={{ flex: 1, minWidth: 260 }}>
      <div className="query-bar">
        <div style={{ position: 'relative', flex: 1 }}>
          <Search size={14} className="faint" style={{ position: 'absolute', left: 10, top: 10 }} />
          <input
            ref={ref}
            className={`input query-input ${invalid ? 'invalid' : ''}`}
            style={{ paddingLeft: 30 }}
            value={text}
            spellCheck={false}
            placeholder={placeholder ?? t('Search with words or TQL, e.g. status != Done and assignee = me() order by priority desc')}
            aria-label={t('Query')}
            aria-invalid={!!invalid}
            onFocus={() => setFocus(true)}
            onBlur={() => setTimeout(() => setFocus(false), 150)}
            onChange={(e) => {
              setText(e.target.value)
              setCursor(e.target.selectionStart ?? e.target.value.length)
            }}
            onSelect={(e) => setCursor((e.target as HTMLInputElement).selectionStart ?? text.length)}
            onKeyDown={(e) => {
              if (focus && suggestions.length && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
                e.preventDefault()
                setSel((s) => (e.key === 'ArrowDown' ? Math.min(s + 1, suggestions.length - 1) : Math.max(s - 1, 0)))
              } else if (focus && suggestions.length && e.key === 'Tab') {
                e.preventDefault()
                apply(suggestions[sel])
              } else if (e.key === 'Enter') {
                e.preventDefault()
                setFocus(false)
                onChange(text)
              } else if (e.key === 'Escape') setFocus(false)
            }}
          />
          {focus && suggestions.length > 0 && (
            <div className="suggest" role="listbox">
              {suggestions.map((s, i) => (
                <button key={s} type="button" className={`menu-item ${i === sel ? 'hover' : ''}`} onMouseDown={(e) => { e.preventDefault(); apply(s) }} onMouseEnter={() => setSel(i)} style={{ fontFamily: 'var(--mono)', fontSize: 12.5 }}>
                  {s}
                </button>
              ))}
              <div className="faint small" style={{ padding: '4px 8px' }}>{t('Tab to insert, Enter to search')}</div>
            </div>
          )}
        </div>
        <button className="btn" onClick={() => onChange(text)}>{t('Search')}</button>
        <PopButton className="icon-btn" title={t('Query help')} label={<CircleQuestionMark size={16} />} align="end" width={420}>
          {() => <TqlHelp />}
        </PopButton>
      </div>
      {invalid && <div className="query-error">{valid.data?.message}</div>}
    </div>
  )
}

function TqlHelp() {
  const ex = [
    ['assignee = me() and category != done', t('my open issues')],
    ['project = WEB and sprint in openSprints()', t('the running sprint')],
    ['type = Bug and priority >= high order by created desc', t('important bugs, newest first')],
    ['due < endOfWeek() and category != done', t('due this week or late')],
    ['labels is empty or assignee is empty', t('issues needing triage')],
    ['updated > -7d and watcher = me()', t('what changed on issues I watch')],
    ['"Customer tier" = Gold', t('a custom field (quote names with spaces)')],
    ['login timeout', t('bare words search titles, descriptions and the text in files')],
  ]
  return (
    <div style={{ padding: 8 }} className="small">
      <p style={{ marginTop: 0 }}>{t('TQL combines conditions with and / or / not and parentheses. Operators: = != ~ (contains) > >= < <= in, is empty. Dates: 2026-11-01, -7d, +2w, today(), startOfWeek(), endOfMonth(). People: me() or a name.')}</p>
      <table className="list">
        <tbody>
          {ex.map(([q, d]) => (
            <tr key={q}>
              <td><code>{q}</code></td>
              <td className="muted">{d}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
