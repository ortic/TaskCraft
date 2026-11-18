// Administration → Extensions: sandboxed scripts admins install — custom automation actions,
// event handlers, scheduled jobs and API routes — with the capabilities each one was granted,
// its secrets, its logs and a switch.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { KeyRound, Pencil, Play, Plus, Puzzle, ScrollText, Trash } from 'lucide-react'
import { api, ago, dateTime, type Named } from '../api'
import { Confirm, Empty, ErrorBox, Loading, Modal, usePeople, useToast } from '../ui'
import { PersonPicker } from '../components/fields'

interface Manifest {
  name: string
  title?: string
  version?: string
  description?: string
  capabilities?: { issues?: 'read' | 'write'; http?: string[]; state?: boolean; secrets?: string[] }
  actions?: { name: string; label: string }[]
  events?: string[]
  schedule?: unknown
  routes?: { method: string; path: string }[]
}

interface Extension {
  id: string
  name: string
  title: string
  version: string
  description: string | null
  manifest: Manifest
  script: string
  enabled: boolean
  runAs: Named | null
  secrets: { name: string; set: boolean }[]
  nextRunAt: string | null
  lastRunAt: string | null
  runs: number
  lastError: string | null
  lastErrorAt: string | null
}

const EXAMPLE_MANIFEST = `{
  "name": "hello",
  "title": "Hello",
  "version": "1.0.0",
  "apiVersion": 1,
  "capabilities": {},
  "actions": [{ "name": "greet", "label": "Say hello on the issue" }]
}`
const EXAMPLE_SCRIPT = `fn action_greet(issue, params) {
    comment(\`Hello from an extension, \${issue.reporter.name}!\`);
}
`

/** What an extension may do, as chips. */
function Capabilities({ m }: { m: Manifest }) {
  const c = m.capabilities ?? {}
  const chips: string[] = []
  if (c.issues) chips.push(c.issues === 'write' ? t('reads and changes issues') : t('reads issues'))
  for (const h of c.http ?? []) chips.push(t('calls {host}', { host: h }))
  if (c.state) chips.push(t('keeps state'))
  if (c.secrets?.length) chips.push(t('{count} secrets', { count: c.secrets.length }))
  if (chips.length === 0) chips.push(t('no special capabilities'))
  const hooks: string[] = []
  if (m.actions?.length) hooks.push(t('{count} rule actions', { count: m.actions.length }))
  if (m.events?.length) hooks.push(t('listens to events'))
  if (m.schedule) hooks.push(t('scheduled job'))
  if (m.routes?.length) hooks.push(t('{count} routes', { count: m.routes.length }))
  return (
    <div className="adm-hook-events">
      {chips.map((x) => <span key={x} className="label-chip">{x}</span>)}
      {hooks.map((x) => <span key={x} className="badge">{x}</span>)}
    </div>
  )
}

export default function ExtensionsTab() {
  const qc = useQueryClient()
  const toast = useToast()
  const q = useQuery({ queryKey: ['admin', 'extensions'], queryFn: () => api.get<{ apiVersion: number; extensions: Extension[] }>('/admin/extensions') })
  const [editing, setEditing] = useState<Extension | 'new' | null>(null)
  const [secrets, setSecrets] = useState<Extension | null>(null)
  const [logs, setLogs] = useState<Extension | null>(null)
  const [removing, setRemoving] = useState<Extension | null>(null)
  const refresh = () => qc.invalidateQueries({ queryKey: ['admin', 'extensions'] })
  const list = q.data?.extensions ?? []
  const change = async (x: Extension, body: Record<string, unknown>) => {
    try {
      await api.patch(`/admin/extensions/${x.id}`, body)
    } catch (e) {
      toast((e as Error).message)
    }
    refresh()
  }
  const run = async (x: Extension) => {
    try {
      const r = await api.post<{ ok: boolean; error?: string }>(`/admin/extensions/${x.id}/run`)
      toast(r.ok ? t('The scheduled job ran') : t('The job failed: {error}', { error: r.error ?? '' }))
    } catch (e) {
      toast((e as Error).message)
    }
    refresh()
  }
  return (
    <>
      <div className="adm-toolbar">
        <p className="small muted adm-intro">
          {t('Extensions are small sandboxed scripts for what’s special about your organization: custom actions for automation rules, handlers for issue events, scheduled jobs and API routes of their own. Each one can do only what its manifest asks for: read or change issues as the person it runs as, call the hosts named, keep a little state, use the secrets you give it.')}
        </p>
        <span className="spacer" />
        <button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('Install an extension')}</button>
      </div>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {q.data && list.length === 0 && (
        <Empty icon={<Puzzle size={32} />} title={t('No extensions installed')}>{t('Paste a manifest and a script, or load an extension file. The admin guide describes what scripts can call.')}</Empty>
      )}
      <div className="adm-hooks">
        {list.map((x) => (
          <div key={x.id} className={`adm-hook ${x.enabled ? '' : 'adm-off'}`}>
            <div className="adm-hook-head">
              <label className="ps-switch" title={x.enabled ? t('On') : t('Off')}>
                <input type="checkbox" checked={x.enabled} onChange={() => change(x, { enabled: !x.enabled })} aria-label={t('{name} is on', { name: x.title })} />
                <span aria-hidden />
              </label>
              <strong className="adm-hook-url">{x.title}</strong>
              <span className="faint small">{x.name} · {x.version}</span>
              {!x.enabled && <span className="badge badge-warning">{t('Off')}</span>}
            </div>
            {x.description && <div className="small muted">{x.description}</div>}
            <Capabilities m={x.manifest} />
            <div className="adm-hook-foot">
              <span className="small faint">
                {x.runAs ? t('Runs as {name}', { name: x.runAs.name }) : t('Runs as nobody')}
                {' · '}
                {x.lastRunAt ? t('ran {count} times, last {when}', { count: x.runs, when: ago(x.lastRunAt) }) : t('hasn’t run yet')}
                {x.nextRunAt && <> · {t('next run {when}', { when: dateTime(x.nextRunAt) })}</>}
              </span>
              {x.lastError && <span className="small adm-bad" title={x.lastErrorAt ? dateTime(x.lastErrorAt) : undefined}>{t('Last error {when}: {error}', { when: ago(x.lastErrorAt), error: x.lastError })}</span>}
              {x.secrets.some((s) => !s.set) && <span className="small adm-bad">{t('A secret isn’t set')}</span>}
              <span className="spacer" />
              {!!x.manifest.schedule && <button className="btn btn-sm" onClick={() => run(x)}><Play size={13} /> {t('Run now')}</button>}
              {x.secrets.length > 0 && <button className="btn btn-sm" onClick={() => setSecrets(x)}><KeyRound size={13} /> {t('Secrets')}</button>}
              <button className="btn btn-sm" onClick={() => setLogs(x)}><ScrollText size={13} /> {t('Log')}</button>
              <button className="btn btn-sm" onClick={() => setEditing(x)}><Pencil size={13} /> {t('Edit')}</button>
              <button className="icon-btn icon-btn-sm" title={t('Remove')} aria-label={t('Remove {name}', { name: x.title })} onClick={() => setRemoving(x)}><Trash size={14} /></button>
            </div>
          </div>
        ))}
      </div>
      {editing && <EditDialog ext={editing === 'new' ? null : editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); refresh() }} />}
      {secrets && <SecretsDialog ext={secrets} onClose={() => setSecrets(null)} onSaved={() => { setSecrets(null); refresh() }} />}
      {logs && <LogsDialog ext={logs} onClose={() => setLogs(null)} />}
      {removing && (
        <Confirm
          title={t('Remove {name}?', { name: removing.title })}
          body={<p>{t('Its state, secrets and log go with it. Automation rules that use its actions fail until it’s installed again.')}</p>}
          confirmLabel={t('Remove')}
          danger
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            try {
              await api.del(`/admin/extensions/${removing.id}`)
            } catch (e) {
              toast((e as Error).message)
            }
            setRemoving(null)
            refresh()
          }}
        />
      )}
    </>
  )
}

function EditDialog({ ext, onClose, onSaved }: { ext: Extension | null; onClose: () => void; onSaved: () => void }) {
  const [manifest, setManifest] = useState(ext ? JSON.stringify(ext.manifest, null, 2) : EXAMPLE_MANIFEST)
  const [script, setScript] = useState(ext?.script ?? EXAMPLE_SCRIPT)
  const [runAs, setRunAs] = useState<Named | null | undefined>(ext ? ext.runAs : undefined)
  const people = usePeople().data ?? []
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const load = async (file: File | undefined) => {
    if (!file) return
    try {
      const v = JSON.parse(await file.text())
      if (!v.manifest || typeof v.script !== 'string') throw new Error(t('An extension file is JSON with “manifest” and “script”.'))
      setManifest(JSON.stringify(v.manifest, null, 2))
      setScript(v.script)
      setErr('')
    } catch (e) {
      setErr((e as Error).message)
    }
  }
  const save = async () => {
    setBusy(true)
    setErr('')
    try {
      let m: unknown
      try {
        m = JSON.parse(manifest)
      } catch {
        throw new Error(t('The manifest isn’t valid JSON.'))
      }
      const body: Record<string, unknown> = { manifest: m, script }
      if (runAs !== undefined) body.runAs = runAs?.id ?? null
      if (ext) await api.patch(`/admin/extensions/${ext.id}`, body)
      else await api.post('/admin/extensions', body)
      onSaved()
    } catch (e) {
      setErr((e as Error).message)
      setBusy(false)
    }
  }
  return (
    <Modal
      title={ext ? t('Edit {name}', { name: ext.title }) : t('Install an extension')}
      onClose={onClose}
      width={760}
      footer={
        <>
          {!ext && (
            <label className="btn">
              {t('Load a file…')}
              <input type="file" accept="application/json,.json" hidden onChange={(e) => void load(e.target.files?.[0])} />
            </label>
          )}
          <span className="spacer" />
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !manifest.trim() || !script.trim()} onClick={save}>{ext ? t('Save') : t('Install')}</button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="ext-manifest">{t('Manifest (JSON): name, version, the capabilities it asks for, its hooks')}</label>
        <textarea id="ext-manifest" className="textarea ps-script-input" rows={9} spellCheck={false} value={manifest} onChange={(e) => setManifest(e.target.value)} />
      </div>
      <div className="field">
        <label htmlFor="ext-script">{t('Script')}</label>
        <textarea id="ext-script" className="textarea ps-script-input" rows={12} spellCheck={false} value={script} onChange={(e) => setScript(e.target.value)} />
      </div>
      <div className="field">
        <span className="field-label" id="ext-runas">{t('Reads and changes issues as')}</span>
        <PersonPicker className="btn ps-person-btn" label={t('Reads and changes issues as')} value={runAs ?? null} onChange={(id) => setRunAs(id ? { id, name: people.find((p) => p.id === id)?.name ?? '' } : null)} none={runAs === undefined ? t('You (the installing admin)') : t('Nobody')} />
        <span className="field-hint">{t('With the “issues” capability, the extension sees and changes what this person may.')}</span>
      </div>
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

function SecretsDialog({ ext, onClose, onSaved }: { ext: Extension; onClose: () => void; onSaved: () => void }) {
  const [values, setValues] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const save = async () => {
    setBusy(true)
    try {
      await api.patch(`/admin/extensions/${ext.id}`, { secrets: Object.fromEntries(Object.entries(values).filter(([, v]) => v !== '')) })
      onSaved()
    } catch (e) {
      setErr((e as Error).message)
      setBusy(false)
    }
  }
  return (
    <Modal
      title={t('Secrets of {name}', { name: ext.title })}
      onClose={onClose}
      width={520}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || Object.values(values).every((v) => v === '')} onClick={save}>{t('Save')}</button>
        </>
      }
    >
      <p className="muted small" style={{ marginTop: 0 }}>{t('Kept for this extension only and never shown again. Leave a field empty to keep what’s there.')}</p>
      {ext.secrets.map((s) => (
        <div className="field" key={s.name}>
          <label htmlFor={`ext-secret-${s.name}`}>{s.name} {s.set ? <span className="badge badge-success">{t('set')}</span> : <span className="badge badge-warning">{t('not set')}</span>}</label>
          <input id={`ext-secret-${s.name}`} className="input" type="password" autoComplete="off" value={values[s.name] ?? ''} onChange={(e) => setValues({ ...values, [s.name]: e.target.value })} />
        </div>
      ))}
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

function LogsDialog({ ext, onClose }: { ext: Extension; onClose: () => void }) {
  const q = useQuery({ queryKey: ['admin', 'extension-logs', ext.id], queryFn: () => api.get<{ at: string; level: 'info' | 'error'; hook: string; message: string }[]>(`/admin/extensions/${ext.id}/logs`) })
  return (
    <Modal title={t('Log of {name}', { name: ext.title })} onClose={onClose} width={720} footer={<button className="btn btn-primary" onClick={onClose}>{t('Close')}</button>}>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {q.data?.length === 0 && <p className="faint">{t('Nothing logged yet. Scripts write here with log(…), and errors land here too.')}</p>}
      <div className="ext-logs">
        {q.data?.map((l, i) => (
          <div key={i} className={`ext-log ${l.level === 'error' ? 'adm-bad' : ''}`}>
            <span className="faint small" title={dateTime(l.at)}>{ago(l.at)}</span>
            <span className="badge">{l.hook}</span>
            <span className="ext-log-text">{l.message}</span>
          </div>
        ))}
      </div>
    </Modal>
  )
}
