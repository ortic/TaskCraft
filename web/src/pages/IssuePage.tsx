import { useEffect } from 'react'
import { useParams, useSearchParams } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import IssueView from '../components/IssueView'

export default function IssuePage() {
  const { key } = useParams()
  // `?print=1`: a printable page (light, without the app around it), printed once it's loaded
  const [params] = useSearchParams()
  const qc = useQueryClient()
  const printing = params.get('print') === '1'
  useEffect(() => {
    if (!printing) return
    const root = document.documentElement
    const theme = root.dataset.theme
    root.dataset.theme = 'light'
    document.body.classList.add('print-view')
    let done = false
    const timer = window.setInterval(() => {
      // print when the issue and its comments are there
      if (!done && qc.isFetching() === 0 && document.querySelector('.issue-title')) {
        done = true
        window.clearInterval(timer)
        window.setTimeout(() => window.print(), 300)
      }
    }, 200)
    return () => {
      window.clearInterval(timer)
      document.body.classList.remove('print-view')
      if (theme) root.dataset.theme = theme
      else delete root.dataset.theme
    }
  }, [printing, qc])
  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      <IssueView key={key} issueKey={key ?? ''} />
    </div>
  )
}
