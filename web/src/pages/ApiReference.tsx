// The API reference: the server's OpenAPI description (/api/openapi.json) as a page — operations
// by section, with their parameters, request and response shapes and a curl example.
import { t } from '../i18n'
import { Fragment, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { BookOpen, Check, Copy, Download, Globe, KeyRound, Search } from 'lucide-react'
import { api } from '../api'
import { ErrorBox, Loading, useMe } from '../ui'
import './ApiReference.css'

interface Schema {
  $ref?: string
  type?: string | string[]
  format?: string
  description?: string
  enum?: unknown[]
  items?: Schema
  properties?: Record<string, Schema>
  required?: string[]
  additionalProperties?: boolean | Schema
  oneOf?: Schema[]
  anyOf?: Schema[]
  allOf?: Schema[]
}
interface Param { name: string; in: string; required?: boolean; description?: string; schema?: Schema }
interface Content { schema?: Schema }
interface Op {
  tags?: string[]
  summary?: string
  description?: string
  operationId?: string
  parameters?: Param[]
  requestBody?: { required?: boolean; description?: string; content?: Record<string, Content> }
  responses?: Record<string, { description?: string; content?: Record<string, Content> }>
  security?: Record<string, unknown>[]
}
interface Spec {
  info: { title: string; version: string; description?: string }
  tags?: { name: string; description?: string }[]
  paths: Record<string, Record<string, Op>>
  components?: { schemas?: Record<string, Schema> }
}

const METHODS = ['get', 'post', 'put', 'patch', 'delete'] as const
const slug = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
const refName = (ref: string) => ref.split('/').pop() ?? ref

/** `code`, [links](url) and paragraphs — enough for the descriptions in the document. */
function Prose({ text }: { text?: string }) {
  if (!text) return null
  return (
    <>
      {text.split(/\n{2,}/).map((para, i) => (
        <p key={i}>
          {para.split(/(`[^`]+`|\[[^\]]+\]\([^)\s]+\))/g).map((part, j) => {
            if (part.startsWith('`') && part.endsWith('`')) return <code key={j}>{part.slice(1, -1)}</code>
            const link = /^\[([^\]]+)\]\(([^)\s]+)\)$/.exec(part)
            if (link) return <a key={j} href={link[2]} target={link[2].startsWith('http') ? '_blank' : undefined} rel="noreferrer">{link[1]}</a>
            return <Fragment key={j}>{part}</Fragment>
          })}
        </p>
      ))}
    </>
  )
}

/** A short type label: `string (uuid)`, `array of Issue`, `"todo" | "done"`, `string | null`. */
function typeLabel(s: Schema | undefined): string {
  if (!s) return t('any value')
  if (s.$ref) return refName(s.$ref)
  if (s.enum) return s.enum.map((v) => JSON.stringify(v)).join(' | ')
  const alt = s.oneOf ?? s.anyOf
  if (alt) return alt.map(typeLabel).join(' | ')
  if (s.allOf) return s.allOf.map(typeLabel).join(' & ')
  const types = Array.isArray(s.type) ? s.type : s.type ? [s.type] : []
  return types
    .map((ty) => {
      if (ty === 'array') return t('array of {type}', { type: typeLabel(s.items) })
      if (ty === 'object' && s.additionalProperties && typeof s.additionalProperties === 'object') return t('map of {type}', { type: typeLabel(s.additionalProperties) })
      return s.format && ty !== 'null' ? `${ty} (${s.format})` : ty
    })
    .join(' | ') || (s.properties ? 'object' : t('any value'))
}

/** The fields of an object (following references a few levels deep). */
function Fields({ schema, spec, depth, seen }: { schema?: Schema; spec: Spec; depth: number; seen: string[] }) {
  if (!schema) return null
  let s = schema
  let name: string | undefined
  if (s.$ref) {
    name = refName(s.$ref)
    if (seen.includes(name) || depth > 3) return null
    s = spec.components?.schemas?.[name] ?? {}
  }
  if (s.allOf) {
    return <>{s.allOf.map((x, i) => <Fields key={i} schema={x} spec={spec} depth={depth} seen={seen} />)}</>
  }
  const item = (Array.isArray(s.type) ? s.type.includes('array') : s.type === 'array') ? s.items : undefined
  if (item) return <Fields schema={item} spec={spec} depth={depth} seen={name ? [...seen, name] : seen} />
  if (!s.properties) return s.description && depth === 0 ? <div className="ar-desc"><Prose text={s.description} /></div> : null
  const required = new Set(s.required ?? [])
  return (
    <ul className="ar-fields">
      {Object.entries(s.properties).map(([k, v]) => (
        <li key={k}>
          <code className="ar-field">{k}</code> <span className="ar-type">{typeLabel(v)}</span>
          {required.has(k) && <span className="ar-req">{t('required')}</span>}
          {v.description && <div className="ar-field-desc"><Prose text={v.description} /></div>}
          {depth < 2 && <Fields schema={v} spec={spec} depth={depth + 1} seen={name ? [...seen, name] : seen} />}
        </li>
      ))}
    </ul>
  )
}

function SchemaBlock({ content, spec }: { content?: Record<string, Content>; spec: Spec }) {
  if (!content) return null
  return (
    <>
      {Object.entries(content).map(([type, c]) => (
        <div key={type} className="ar-schema">
          <div className="ar-schema-head">
            <span className="ar-type">{typeLabel(c.schema)}</span> <span className="faint small">{type}</span>
          </div>
          <Fields schema={c.schema} spec={spec} depth={0} seen={[]} />
        </div>
      ))}
    </>
  )
}

/** A plausible value of a schema for the curl example. */
function sample(s: Schema | undefined, spec: Spec, depth = 0): unknown {
  if (!s || depth > 2) return null
  if (s.$ref) return sample(spec.components?.schemas?.[refName(s.$ref)], spec, depth)
  if (s.enum) return s.enum[0]
  const ty = Array.isArray(s.type) ? s.type.find((x) => x !== 'null') : s.type
  if (ty === 'object' || s.properties) {
    const out: Record<string, unknown> = {}
    const keys = Object.keys(s.properties ?? {})
    const required = s.required?.length ? keys.filter((k) => s.required!.includes(k)) : keys.slice(0, 3)
    for (const k of required) out[k] = sample(s.properties![k], spec, depth + 1)
    return out
  }
  if (ty === 'array') return [sample(s.items, spec, depth + 1)].filter((x) => x !== null)
  if (ty === 'integer' || ty === 'number') return 0
  if (ty === 'boolean') return false
  if (s.format === 'uuid') return '01890000-0000-7000-8000-000000000000'
  if (s.format === 'date') return '2026-12-31'
  if (s.format === 'date-time') return '2026-12-31T09:00:00Z'
  return '…'
}

function curl(method: string, path: string, op: Op, base: string, spec: Spec): string {
  const lines = [`curl -X ${method.toUpperCase()} "${base}/api/v1${path}"`]
  if (op.security?.length !== 0 && !isPublic(op)) lines.push(`  -H "Authorization: Bearer $TASKCRAFT_TOKEN"`)
  const content = op.requestBody?.content
  if (content?.['application/json']) {
    lines.push('  -H "Content-Type: application/json"')
    lines.push(`  -d '${JSON.stringify(sample(content['application/json'].schema, spec) ?? {})}'`)
  } else if (content?.['multipart/form-data']) {
    lines.push('  -F "file=@./file.pdf"')
  }
  return lines.join(' \\\n')
}

/** No sign-in needed: an empty requirement among the alternatives. */
function isPublic(op: Op): boolean {
  return !!op.security?.some((r) => Object.keys(r).length === 0)
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <button
      type="button"
      className="icon-btn icon-btn-sm ar-copy"
      aria-label={copied ? t('Copied') : t('Copy')}
      title={copied ? t('Copied') : t('Copy')}
      onClick={() => {
        navigator.clipboard?.writeText(text).then(() => {
          setCopied(true)
          setTimeout(() => setCopied(false), 1500)
        }, () => {})
      }}
    >
      {copied ? <Check size={14} /> : <Copy size={14} />}
    </button>
  )
}

function Operation({ method, path, op, spec, base }: { method: string; path: string; op: Op; spec: Spec; base: string }) {
  const id = op.operationId ?? `${method}-${slug(path)}`
  const params = op.parameters ?? []
  const example = curl(method, path, op, base, spec)
  return (
    <details className="ar-op" id={id}>
      <summary>
        <span className={`ar-method ar-${method}`}>{method.toUpperCase()}</span>
        <code className="ar-path">{path}</code>
        <span className="ar-summary">{op.summary}</span>
        {isPublic(op) && <span className="badge" title={t('No sign-in needed')}><Globe size={11} aria-hidden /> {t('public')}</span>}
      </summary>
      <div className="ar-body">
        {op.description && <div className="ar-desc"><Prose text={op.description} /></div>}
        {params.length > 0 && (
          <>
            <h4>{t('Parameters')}</h4>
            <table className="list ar-params">
              <thead>
                <tr>
                  <th>{t('Name')}</th>
                  <th>{t('In')}</th>
                  <th>{t('Type')}</th>
                  <th>{t('Description')}</th>
                </tr>
              </thead>
              <tbody>
                {params.map((p) => (
                  <tr key={`${p.in}-${p.name}`}>
                    <td><code>{p.name}</code> {p.required && <span className="ar-req">{t('required')}</span>}</td>
                    <td className="faint">{p.in}</td>
                    <td><span className="ar-type">{typeLabel(p.schema)}</span></td>
                    <td><Prose text={p.description} /></td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}
        {op.requestBody && (
          <>
            <h4>{t('Request body')} {op.requestBody.required && <span className="ar-req">{t('required')}</span>}</h4>
            {op.requestBody.description && <Prose text={op.requestBody.description} />}
            <SchemaBlock content={op.requestBody.content} spec={spec} />
          </>
        )}
        {op.responses && (
          <>
            <h4>{t('Responses')}</h4>
            <dl className="ar-responses">
              {Object.entries(op.responses).map(([code, r]) => (
                <div key={code} className="ar-response">
                  <dt><span className={`ar-code ar-code-${code[0]}`}>{code}</span> {r.description}</dt>
                  <dd><SchemaBlock content={r.content} spec={spec} /></dd>
                </div>
              ))}
            </dl>
          </>
        )}
        <h4>{t('Example')}</h4>
        <div className="ar-example">
          <pre><code>{example}</code></pre>
          <CopyButton text={example} />
        </div>
      </div>
    </details>
  )
}

export default function ApiReference() {
  const me = useMe().data
  const base = me?.baseUrl ?? window.location.origin
  const spec = useQuery({ queryKey: ['openapi'], queryFn: () => api.get<Spec>('/openapi.json'), staleTime: Infinity })
  const [filter, setFilter] = useState('')
  // operations by section, in the document's section order, filtered
  const sections = useMemo(() => {
    const s = spec.data
    if (!s) return []
    const needle = filter.trim().toLowerCase()
    const byTag = new Map<string, { method: string; path: string; op: Op }[]>()
    for (const tag of s.tags ?? []) byTag.set(tag.name, [])
    for (const [path, ops] of Object.entries(s.paths)) {
      for (const method of METHODS) {
        const op = ops[method]
        if (!op) continue
        const hay = `${method} ${path} ${op.summary ?? ''} ${op.description ?? ''} ${(op.tags ?? []).join(' ')}`.toLowerCase()
        if (needle && !needle.split(/\s+/).every((w) => hay.includes(w))) continue
        const tag = op.tags?.[0] ?? t('Other')
        if (!byTag.has(tag)) byTag.set(tag, [])
        byTag.get(tag)!.push({ method, path, op })
      }
    }
    return [...byTag.entries()].filter(([, ops]) => ops.length > 0).map(([name, ops]) => ({ name, ops, desc: s.tags?.find((x) => x.name === name)?.description }))
  }, [spec.data, filter])
  const total = sections.reduce((n, s) => n + s.ops.length, 0)
  return (
    <div className="page-wide ar">
      <div className="page-head">
        <h1><BookOpen size={22} /> {t('API reference')}</h1>
        <span className="spacer" />
        <a className="btn btn-sm" href="/api/openapi.yaml" download><Download size={14} /> OpenAPI (YAML)</a>
        <a className="btn btn-sm" href="/api/openapi.json" download><Download size={14} /> OpenAPI (JSON)</a>
        <Link className="btn btn-sm" to="/settings#tokens"><KeyRound size={14} /> {t('Create a token')}</Link>
      </div>
      {spec.isLoading && <Loading />}
      {spec.error && <ErrorBox error={spec.error} />}
      {spec.data && (
        <div className="ar-layout">
          <nav className="ar-nav" aria-label={t('Sections')}>
            <label className="ar-filter">
              <Search size={14} aria-hidden />
              <input className="input input-sm" type="search" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={t('Filter operations')} aria-label={t('Filter operations')} />
            </label>
            <p className="faint small" aria-live="polite">{t('{count} operations', { count: total })}</p>
            <ul>
              {sections.map((s) => (
                <li key={s.name}>
                  <a href={`#tag-${slug(s.name)}`}>{s.name} <span className="faint">{s.ops.length}</span></a>
                </li>
              ))}
            </ul>
          </nav>
          <div className="ar-main">
            <section className="ar-intro" aria-label={spec.data.info.title}>
              <p className="faint small">{spec.data.info.title} · {spec.data.info.version} · <code>{base}/api/v1</code></p>
              <Prose text={spec.data.info.description} />
            </section>
            {sections.length === 0 && <p className="muted">{t('No operations match.')}</p>}
            {sections.map((s) => (
              <section key={s.name} id={`tag-${slug(s.name)}`} className="ar-section" aria-labelledby={`tag-${slug(s.name)}-h`}>
                <h2 id={`tag-${slug(s.name)}-h`}>{s.name}</h2>
                {s.desc && <div className="ar-desc"><Prose text={s.desc} /></div>}
                {s.ops.map((o) => <Operation key={`${o.method} ${o.path}`} method={o.method} path={o.path} op={o.op} spec={spec.data!} base={base} />)}
              </section>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

