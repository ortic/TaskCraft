// Project settings → Service desk: the public request form (fields, texts, address) and what
// requesters may do in the portal.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ClipboardList, Copy, ExternalLink, LifeBuoy, TriangleAlert } from 'lucide-react'
import { api, type Project } from '../api'
import { ErrorBox, Loading, useToast } from '../ui'
import './PortalSettings.css'

type Show = 'off' | 'optional' | 'required'
interface FormSettings {
  enabled: boolean
  slug?: string | null
  listed: boolean
  title: string
  intro: string
  success: string
  description: Show
  name: Show
  types: string[]
  typeId?: string | null
  priority: boolean
  custom: { id: string; required: boolean }[]
}
interface Settings {
  resolve: boolean
  resolveStatus?: string | null
  form: FormSettings
}
interface Overview {
  settings: Settings
  formUrl: string
  portalUrl: string
  mail: boolean
}

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

function CopyLink({ url, label }: { url: string; label: string }) {
  const toast = useToast()
  return (
    <span className="row portal-set-url">
      <code title={url}>{url}</code>
      <button
        type="button"
        className="icon-btn icon-btn-sm"
        title={t('Copy')}
        aria-label={label}
        onClick={() => void navigator.clipboard?.writeText(url).then(() => toast(t('Copied')), () => toast(t('Copying didn’t work: select the text instead')))}
      >
        <Copy size={13} />
      </button>
      <a className="icon-btn icon-btn-sm" href={url} target="_blank" rel="noreferrer" title={t('Open')} aria-label={t('Open {page} in a new tab', { page: label })}>
        <ExternalLink size={13} />
      </a>
    </span>
  )
}

export default function PortalSettings({ project, onSaved }: { project: Project; onSaved: () => void }) {
  const qc = useQueryClient()
  const qk = ['portal-settings', project.key]
  const q = useQuery({ queryKey: qk, queryFn: () => api.get<Overview>(`/projects/${project.key}/portal`) })
  if (q.isLoading) return <Loading />
  if (q.error || !q.data) return <ErrorBox error={q.error} />
  return (
    <SettingsForm
      key={JSON.stringify(q.data.settings)}
      project={project}
      data={q.data}
      onSaved={(d) => {
        qc.setQueryData(qk, d)
        onSaved()
      }}
    />
  )
}

function ShowSelect({ id, label, value, onChange }: { id: string; label: string; value: Show; onChange: (v: Show) => void }) {
  return (
    <div className="field portal-set-show">
      <label htmlFor={id}>{label}</label>
      <select id={id} className="select" value={value} onChange={(e) => onChange(e.target.value as Show)}>
        <option value="off">{t('Not asked')}</option>
        <option value="optional">{t('Optional')}</option>
        <option value="required">{t('Required [[form field]]')}</option>
      </select>
    </div>
  )
}

function SettingsForm({ project, data, onSaved }: { project: Project; data: Overview; onSaved: (d: Overview) => void }) {
  const toast = useToast()
  const [s, setS] = useState<Settings>(data.settings)
  const [busy, setBusy] = useState(false)
  const form = s.form
  const setForm = (patch: Partial<FormSettings>) => setS((cur) => ({ ...cur, form: { ...cur.form, ...patch } }))
  const dirty = JSON.stringify(s) !== JSON.stringify(data.settings)
  const slug = (form.slug ?? '').trim().toLowerCase()
  const validSlug = !slug || /^[a-z0-9][a-z0-9-]{0,38}[a-z0-9]$/.test(slug)
  const formUrl = `${data.portalUrl}/forms/${slug || project.key.toLowerCase()}`
  const standardTypes = project.types.filter((ty) => ty.level === 0)
  const fields = project.fields.filter((f) => f.kind !== 'user')
  const doneStatuses = project.statuses.filter((st) => st.category === 'done')
  const save = async () => {
    setBusy(true)
    try {
      onSaved(await api.put<Overview>(`/projects/${project.key}/portal`, { ...s, form: { ...form, slug: slug || null } }))
      toast(t('Saved'))
    } catch (e) {
      toast(errText(e))
    } finally {
      setBusy(false)
    }
  }
  const custom = (id: string) => form.custom.find((c) => c.id === id)
  const setCustom = (id: string, show: boolean, required: boolean) =>
    setForm({ custom: show ? [...form.custom.filter((c) => c.id !== id), { id, required }] : form.custom.filter((c) => c.id !== id) })
  return (
    <>
      <section className="ps-section">
        <div className="ps-section-head">
          <div className="ps-section-text">
            <h2><span className="row"><ClipboardList size={17} aria-hidden /> {t('Request form')}</span></h2>
            <p className="ps-desc">
              {t('A public page where anyone can ask for help, without an account: each request becomes an issue here, and whoever sent it becomes its requester. They get a confirmation by e-mail and follow the request in the portal.')}
            </p>
          </div>
        </div>
        {!data.mail && (
          <div className="ps-warn">
            <TriangleAlert size={18} aria-hidden />
            <div>{t('Outgoing e-mail isn’t set up (TASKCRAFT_SMTP_URL): requesters get no confirmation and can’t sign in to the portal.')}</div>
          </div>
        )}
        <label className="ps-check portal-set-enable">
          <input type="checkbox" checked={form.enabled} onChange={(e) => setForm({ enabled: e.target.checked })} />
          <span className="ps-strong">{t('Publish a request form for {name}', { name: project.name })}</span>
        </label>

        <fieldset className="portal-set-group" disabled={!form.enabled}>
          <div className="field">
            <span className="field-label">{t('Address of the form')}</span>
            <CopyLink url={formUrl} label={t('the request form')} />
          </div>
          <div className="field">
            <label htmlFor="portal-slug">{t('Short name in the address')}</label>
            <input
              id="portal-slug"
              className="input portal-set-slug"
              value={form.slug ?? ''}
              placeholder={project.key.toLowerCase()}
              aria-invalid={!validSlug || undefined}
              aria-describedby="portal-slug-hint"
              onChange={(e) => setForm({ slug: e.target.value })}
            />
            <span className="field-hint" id="portal-slug-hint">{t('Letters (a–z), digits and dashes, like it-help. Left empty, the project key is used.')}</span>
          </div>
          <label className="ps-check portal-set-row">
            <input type="checkbox" checked={form.listed} onChange={(e) => setForm({ listed: e.target.checked })} />
            {t('List it on the portal’s start page')}
          </label>
          <div className="field">
            <label htmlFor="portal-title">{t('Title')}</label>
            <input id="portal-title" className="input" maxLength={200} value={form.title} placeholder={t('Submit a request')} onChange={(e) => setForm({ title: e.target.value })} />
          </div>
          <div className="field">
            <label htmlFor="portal-intro">{t('Introduction')}</label>
            <textarea id="portal-intro" className="textarea" rows={3} maxLength={5000} value={form.intro} aria-describedby="portal-intro-hint" onChange={(e) => setForm({ intro: e.target.value })} />
            <span className="field-hint" id="portal-intro-hint">{t('Shown above the fields: what to ask here, what happens next.')}</span>
          </div>

          <h3 className="portal-set-h">{t('Fields')}</h3>
          <p className="faint small portal-set-p">{t('A summary and an e-mail address are always asked for. Signed-in requesters don’t need to give their address.')}</p>
          <div className="ps-grid-2">
            <ShowSelect id="portal-description" label={t('Description')} value={form.description} onChange={(v) => setForm({ description: v })} />
            <ShowSelect id="portal-name" label={t('Name')} value={form.name} onChange={(v) => setForm({ name: v })} />
          </div>
          <label className="ps-check portal-set-row">
            <input type="checkbox" checked={form.priority} onChange={(e) => setForm({ priority: e.target.checked })} />
            {t('Let them choose how urgent it is (priority)')}
          </label>
          <fieldset className="portal-set-types">
            <legend className="field-label">{t('Request types')}</legend>
            <div className="ps-checks">
              {standardTypes.map((ty) => (
                <label key={ty.id} className="ps-check">
                  <input
                    type="checkbox"
                    checked={form.types.includes(ty.id)}
                    onChange={(e) => setForm({ types: e.target.checked ? [...form.types, ty.id] : form.types.filter((x) => x !== ty.id) })}
                  />
                  {ty.name}
                </label>
              ))}
            </div>
            <span className="field-hint">{t('With two or more ticked, they choose one; with one, every request gets that type; with none, the project’s default type.')}</span>
          </fieldset>
          {fields.length > 0 && (
            <table className="list portal-set-fields">
              <caption className="sr-only">{t('Fields of this project on the form')}</caption>
              <thead>
                <tr>
                  <th scope="col">{t('Field')}</th>
                  <th scope="col">{t('On the form')}</th>
                  <th scope="col">{t('Required [[form field]]')}</th>
                </tr>
              </thead>
              <tbody>
                {fields.map((f) => {
                  const c = custom(f.id)
                  return (
                    <tr key={f.id}>
                      <th scope="row" className="portal-set-field">{f.name}</th>
                      <td>
                        <input type="checkbox" aria-label={t('Show {field} on the form', { field: f.name })} checked={!!c} onChange={(e) => setCustom(f.id, e.target.checked, c?.required ?? false)} />
                      </td>
                      <td>
                        <input type="checkbox" aria-label={t('{field} is required', { field: f.name })} disabled={!c} checked={!!c?.required} onChange={(e) => setCustom(f.id, true, e.target.checked)} />
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          )}
          {project.fields.some((f) => f.kind === 'user') && <p className="faint small portal-set-p">{t('Fields that pick a person aren’t offered: the form doesn’t show who works here.')}</p>}

          <div className="field">
            <label htmlFor="portal-success">{t('Message after sending')}</label>
            <textarea
              id="portal-success"
              className="textarea"
              rows={2}
              maxLength={2000}
              value={form.success}
              placeholder={t('Thanks! We’ve received your request and sent you a confirmation by e-mail.')}
              onChange={(e) => setForm({ success: e.target.value })}
            />
          </div>
          <p className="faint small portal-set-p">
            {t('Against spam: a hidden field bots fill in, a few seconds people need to fill the form, and limits per sender and e-mail address. No outside service is involved.')}
          </p>
        </fieldset>
      </section>

      <section className="ps-section">
        <div className="ps-section-head">
          <div className="ps-section-text">
            <h2><span className="row"><LifeBuoy size={17} aria-hidden /> {t('Portal')}</span></h2>
            <p className="ps-desc">
              {t('Requesters — people who wrote in by e-mail or used the request form — sign in to the portal with a link e-mailed to them. They see their own requests with the description, their files and the comments marked “Visible to the requester”, never internal comments, and can answer and add files.')}
            </p>
          </div>
        </div>
        <div className="field">
          <span className="field-label">{t('Address of the portal')}</span>
          <CopyLink url={data.portalUrl} label={t('the portal')} />
        </div>
        <label className="ps-check portal-set-row">
          <input type="checkbox" checked={s.resolve} onChange={(e) => setS({ ...s, resolve: e.target.checked })} />
          {t('Requesters can mark their requests as resolved')}
        </label>
        {s.resolve && doneStatuses.length > 1 && (
          <div className="field portal-set-show">
            <label htmlFor="portal-resolve-status">{t('Resolved requests move to')}</label>
            <select id="portal-resolve-status" className="select" value={s.resolveStatus ?? ''} onChange={(e) => setS({ ...s, resolveStatus: e.target.value || null })}>
              <option value="">{doneStatuses[0].name}</option>
              {doneStatuses.slice(1).map((st) => <option key={st.id} value={st.id}>{st.name}</option>)}
            </select>
          </div>
        )}
      </section>

      <div className="ps-savebar">
        <button className="btn btn-primary" disabled={!dirty || !validSlug || busy} onClick={save}>{t('Save')}</button>
        {!validSlug && <span className="error-text">{t('The short name takes letters (a–z), digits and dashes.')}</span>}
        {dirty && validSlug && <span className="ps-savebar-text">{t('Unsaved changes')}</span>}
      </div>
    </>
  )
}
