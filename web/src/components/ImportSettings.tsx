// Project settings → Import: a CSV file, its columns mapped to fields, a check, then the import.
import { t } from '../i18n'
import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Upload, CircleCheck, TriangleAlert } from 'lucide-react'
import { api, type Project } from '../api'
import { useToast } from '../ui'
import { refreshIssues } from './IssueView'

interface Preview {
  headers: string[]
  rows: string[][]
  total: number
  mapping: Record<string, string>
  fields: string[]
}

interface Result {
  dryRun: boolean
  rows: number
  created: number
  errors: { row: number; message: string }[]
  warnings: { row: number; message: string }[]
  keys: string[]
}

const FIELD_LABELS: Record<string, () => string> = {
  key: () => t('Key (from the other tool)'),
  id: () => t('ID (from the other tool)'),
  title: () => t('Title'),
  description: () => t('Description'),
  type: () => t('Type'),
  status: () => t('Status'),
  priority: () => t('Priority'),
  assignee: () => t('Assignee'),
  reporter: () => t('Reporter'),
  labels: () => t('Labels'),
  sprint: () => t('Sprint'),
  version: () => t('Release'),
  component: () => t('Component'),
  parent: () => t('Parent (key)'),
  points: () => t('Story points'),
  estimate: () => t('Estimate (hours)'),
  estimateSeconds: () => t('Estimate (seconds)'),
  remaining: () => t('Remaining estimate (hours)'),
  remainingSeconds: () => t('Remaining estimate (seconds)'),
  spent: () => t('Time spent (hours)'),
  spentSeconds: () => t('Time spent (seconds)'),
  start: () => t('Start date'),
  due: () => t('Due date'),
  created: () => t('Created'),
  resolved: () => t('Resolved'),
  resolution: () => t('Resolution'),
}

export function ImportSection({ project }: { project: Project }) {
  const qc = useQueryClient()
  const toast = useToast()
  const [csv, setCsv] = useState('')
  const [fileName, setFileName] = useState('')
  const [preview, setPreview] = useState<Preview | null>(null)
  const [mapping, setMapping] = useState<Record<string, string>>({})
  const [createMissing, setCreateMissing] = useState(true)
  const [result, setResult] = useState<Result | null>(null)
  const [busy, setBusy] = useState(false)
  const load = async (file: File) => {
    const text = await file.text()
    setCsv(text)
    setFileName(file.name)
    setResult(null)
    try {
      const p = await api.post<Preview>(`/projects/${project.key}/import/preview`, { csv: text })
      setPreview(p)
      setMapping(p.mapping)
    } catch (e) {
      setPreview(null)
      toast((e as Error).message)
    }
  }
  const run = async (dryRun: boolean) => {
    setBusy(true)
    try {
      const r = await api.post<Result>(`/projects/${project.key}/import`, { csv, mapping, dryRun, createMissing })
      setResult(r)
      if (!dryRun) {
        refreshIssues(qc)
        qc.invalidateQueries({ queryKey: ['project', project.key] })
        qc.invalidateQueries({ queryKey: ['projects'] })
        toast(t('{count} issues imported', { count: r.created }))
      }
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const options = [
    ...(preview?.fields ?? []).map((f) => ({ id: f, label: FIELD_LABELS[f]?.() ?? f })),
    ...project.fields.map((f) => ({ id: `custom:${f.id}`, label: f.name })),
  ]
  const hasTitle = Object.values(mapping).includes('title')
  return (
    <section className="ps-section">
      <div className="ps-section-head">
        <div className="ps-section-text">
          <h2>{t('Import')}</h2>
          <p className="ps-desc">
            {t('Bring issues in from a CSV file (most tools export one). Columns are matched to fields by their names; check the matching, test the import, then run it. Statuses, types and people are matched by name — invite people first, also those who have left, so their issues and time stay theirs. Keys from the other tool keep working as links. Nobody gets notified.')}
          </p>
        </div>
      </div>
      <label className="dropzone" style={{ display: 'block', cursor: 'pointer' }}>
        <Upload size={18} /> {fileName ? t('{name} · choose another file', { name: fileName }) : t('Choose a CSV file')}
        <input type="file" accept=".csv,text/csv,text/plain" hidden onChange={(e) => e.target.files?.[0] && load(e.target.files[0])} />
      </label>
      {preview && (
        <>
          <p className="muted small">{t('{count} rows found. Match the columns:', { count: preview.total })}</p>
          <div style={{ overflowX: 'auto', border: '1px solid var(--border)', borderRadius: 8 }}>
            <table className="list" style={{ minWidth: 600 }}>
              <thead>
                <tr>
                  {preview.headers.map((h, j) => (
                    <th key={j} style={{ verticalAlign: 'top' }}>
                      <div style={{ textTransform: 'none', color: 'var(--text)', marginBottom: 4 }}>{h}</div>
                      <select className="select input-sm" style={{ minWidth: 140 }} aria-label={t('Field for column {name}', { name: h })} value={mapping[h] ?? 'ignore'} onChange={(e) => setMapping({ ...mapping, [h]: e.target.value })}>
                        <option value="ignore">{t('— ignore —')}</option>
                        {options.map((o) => <option key={o.id} value={o.id}>{o.label}</option>)}
                      </select>
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {preview.rows.map((r, i) => (
                  <tr key={i}>
                    {preview.headers.map((h, j) => (
                      <td key={j} className={mapping[h] === 'ignore' ? 'faint' : ''} style={{ maxWidth: 220, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{r[j]}</td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <label className="row small" style={{ margin: '10px 0' }}>
            <input type="checkbox" checked={createMissing} onChange={(e) => setCreateMissing(e.target.checked)} /> {t('Create statuses, sprints, releases and components that don’t exist yet')}
          </label>
          {!hasTitle && <p className="error-text">{t('Match one column to the title.')}</p>}
          <div className="row">
            <button className="btn" disabled={busy || !hasTitle} onClick={() => run(true)}>{t('Check')}</button>
            <button className="btn btn-primary" disabled={busy || !hasTitle} onClick={() => run(false)}><Upload size={14} /> {t('Import {count} issues', { count: preview.total })}</button>
          </div>
        </>
      )}
      {result && (
        <div className="card" style={{ marginTop: 12 }}>
          <div className="row" style={{ fontWeight: 600 }}>
            {result.errors.length ? <TriangleAlert size={16} color="var(--warning)" /> : <CircleCheck size={16} color="var(--success)" />}
            {result.dryRun ? t('Check: {count} of {rows} rows can be imported.', { count: result.created, rows: result.rows }) : t('{count} issues imported.', { count: result.created })}
          </div>
          {result.keys.length > 0 && <p className="small muted">{result.keys.slice(0, 12).join(', ')}{result.keys.length > 12 ? ' …' : ''}</p>}
          {[...result.errors.map((e) => ({ ...e, error: true })), ...result.warnings.map((w) => ({ ...w, error: false }))].slice(0, 60).map((m, i) => (
            <div key={i} className="small" style={{ color: m.error ? 'var(--danger)' : 'var(--text-soft)' }}>
              {t('Row {row}', { row: m.row })}: {m.message}
            </div>
          ))}
        </div>
      )}
    </section>
  )
}
