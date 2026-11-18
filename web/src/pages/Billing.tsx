// Administration → Time tracking → billing: the currency, the day up to which time is closed
// (billed), and the hourly rates that turn billable time into amounts in the time report.
import { t } from '../i18n'
import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Trash } from 'lucide-react'
import { api, type Named, type TimeCategory } from '../api'
import { ErrorBox, Loading, useCustomers, usePeople, useToast } from '../ui'

interface RatesPage {
  currency: string | null
  closedUntil: string | null
  rates: { id: string; customer: Named | null; category: Named | null; user: Named | null; rate: number }[]
}

interface Row {
  customerId: string
  categoryId: string
  userId: string
  rate: string
}

export default function BillingSettings() {
  const qc = useQueryClient()
  const toast = useToast()
  const q = useQuery({ queryKey: ['time-rates'], queryFn: () => api.get<RatesPage>('/time-rates') })
  const customers = useCustomers().data ?? []
  const people = usePeople().data ?? []
  const categories = useQuery({ queryKey: ['time-categories'], queryFn: () => api.get<TimeCategory[]>('/time-categories') }).data ?? []
  const [rows, setRows] = useState<Row[]>([])
  const [currency, setCurrency] = useState('')
  const [closed, setClosed] = useState('')
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    if (!q.data) return
    setRows(q.data.rates.map((r) => ({ customerId: r.customer?.id ?? '', categoryId: r.category?.id ?? '', userId: r.user?.id ?? '', rate: String(r.rate) })))
    setCurrency(q.data.currency ?? '')
    setClosed(q.data.closedUntil ?? '')
  }, [q.data])
  if (q.isLoading) return <Loading />
  if (q.error) return <ErrorBox error={q.error} />
  const set = (i: number, patch: Partial<Row>) => setRows((all) => all.map((r, n) => (n === i ? { ...r, ...patch } : r)))
  const save = async () => {
    setBusy(true)
    try {
      await api.put('/admin/workspace', { currency: currency.trim() || null, timeClosedUntil: closed || null })
      await api.put('/time-rates', {
        rates: rows.filter((r) => r.rate.trim() !== '').map((r) => ({ customerId: r.customerId || null, categoryId: r.categoryId || null, userId: r.userId || null, rate: Number(r.rate.replace(',', '.')) })),
      })
      toast(t('Saved'))
      qc.invalidateQueries({ queryKey: ['time-rates'] })
      qc.invalidateQueries({ queryKey: ['me'] })
      qc.invalidateQueries({ queryKey: ['time-report'] })
    } catch (e) {
      toast((e as Error).message)
    }
    setBusy(false)
  }
  return (
    <div className="adm-card" style={{ marginTop: 18 }}>
      <div className="adm-card-head">
        <h2>{t('Billing')}</h2>
        <p className="small muted">{t('Hourly rates turn billable time into amounts in the time report and its CSV, for workspace admins. The most specific rate counts: a customer before a person before a category.')}</p>
      </div>
      <div className="adm-card-body">
        <div className="row-wrap" style={{ gap: 16, alignItems: 'flex-start' }}>
          <div className="field">
            <label htmlFor="bill-currency">{t('Currency')}</label>
            <input id="bill-currency" className="input" style={{ width: 110 }} maxLength={3} placeholder="CHF" value={currency} onChange={(e) => setCurrency(e.target.value.toUpperCase())} />
          </div>
          <div className="field">
            <label htmlFor="bill-closed">{t('Time is closed up to')}</label>
            <input id="bill-closed" className="input" type="date" value={closed} onChange={(e) => setClosed(e.target.value)} />
            <span className="field-hint">{t('Once a period is billed: time logs up to this day can’t be added, changed or deleted any more, except by admins.')}</span>
          </div>
        </div>
        <table className="list" style={{ marginTop: 6 }}>
          <thead>
            <tr><th>{t('Customer')}</th><th>{t('Person')}</th><th>{t('Category')}</th><th>{t('Rate per hour')}</th><th><span className="sr-only">{t('Remove')}</span></th></tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={i}>
                <td>
                  <select className="select input-sm" aria-label={t('Customer')} value={r.customerId} onChange={(e) => set(i, { customerId: e.target.value })}>
                    <option value="">{t('Any customer')}</option>
                    {customers.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
                  </select>
                </td>
                <td>
                  <select className="select input-sm" aria-label={t('Person')} value={r.userId} onChange={(e) => set(i, { userId: e.target.value })}>
                    <option value="">{t('Anyone')}</option>
                    {people.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
                  </select>
                </td>
                <td>
                  <select className="select input-sm" aria-label={t('Category')} value={r.categoryId} onChange={(e) => set(i, { categoryId: e.target.value })}>
                    <option value="">{t('Any category')}</option>
                    {categories.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
                  </select>
                </td>
                <td><input className="input input-sm" style={{ width: 110 }} inputMode="decimal" aria-label={t('Rate per hour')} value={r.rate} onChange={(e) => set(i, { rate: e.target.value })} /></td>
                <td><button className="icon-btn icon-btn-sm" title={t('Remove')} aria-label={t('Remove this rate')} onClick={() => setRows((all) => all.filter((_, n) => n !== i))}><Trash size={14} /></button></td>
              </tr>
            ))}
            {rows.length === 0 && <tr><td colSpan={5} className="faint small">{t('No rates: the time report shows hours only.')}</td></tr>}
          </tbody>
        </table>
      </div>
      <div className="adm-card-foot">
        <button className="btn btn-sm" onClick={() => setRows((all) => [...all, { customerId: '', categoryId: '', userId: '', rate: '' }])}><Plus size={14} /> {t('Add a rate')}</button>
        <span className="spacer" />
        <button className="btn btn-primary" disabled={busy} onClick={save}>{t('Save billing settings')}</button>
      </div>
    </div>
  )
}
