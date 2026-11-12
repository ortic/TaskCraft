import { t } from './i18n'
import { lazy, Suspense } from 'react'
import { Navigate, Route, Routes, useLocation, useParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api } from './api'
import { chooseLanguage, locale, rememberLanguage } from './i18n'
import { Loading, ToastProvider, useMe, useProject } from './ui'
import { useLiveUpdates } from './live'
import { applyAccent } from './look'
import Layout from './components/Layout'
import { Login, Setup, Welcome } from './pages/Auth'
import Home from './pages/Home'
import Projects from './pages/Projects'
import Board from './pages/Board'
import Backlog from './pages/Backlog'
import IssueList from './pages/IssueList'
import IssuePage from './pages/IssuePage'

const Timeline = lazy(() => import('./pages/Timeline'))
const Calendar = lazy(() => import('./pages/Calendar'))
const Reports = lazy(() => import('./pages/Reports'))
const Releases = lazy(() => import('./pages/Releases'))
const ProjectDocs = lazy(() => import('./pages/ProjectDocs'))
const ProjectSettings = lazy(() => import('./pages/ProjectSettings'))
const Admin = lazy(() => import('./pages/Admin'))
const Settings = lazy(() => import('./pages/Settings'))
const Timesheet = lazy(() => import('./pages/Timesheet'))
const People = lazy(() => import('./pages/People'))
const Queue = lazy(() => import('./pages/Queue'))

export default function App() {
  const setup = useQuery({ queryKey: ['setup'], queryFn: () => api.get<{ needsSetup: boolean }>('/setup') })
  const me = useMe()
  const loc = useLocation()
  const user = me.data?.user
  useLiveUpdates(!!user)
  if (setup.isLoading || me.isLoading) return <Loading />
  if (setup.data?.needsSetup) return <Setup />
  const lang = chooseLanguage(me.data?.profile?.locale, me.data?.workspace?.defaultLanguage)
  if (user && lang !== locale()) {
    rememberLanguage(lang)
    window.location.reload()
    return null
  }
  applyAccent(me.data?.workspace?.accentColor)
  if (loc.pathname === '/welcome') return <Welcome />
  if (!user) {
    if (loc.pathname === '/login') return <Login />
    return <Navigate to={`/login?next=${encodeURIComponent(loc.pathname + loc.search)}`} replace />
  }
  return (
    <ToastProvider>
      <Suspense fallback={<Loading />}>
        <Routes>
          <Route path="/login" element={<Navigate to="/" replace />} />
          <Route element={<Layout />}>
            <Route path="/" element={<Home />} />
            <Route path="/projects" element={<Projects />} />
            <Route path="/projects/:key" element={<ProjectStart />} />
            <Route path="/projects/:key/board" element={<Board />} />
            <Route path="/projects/:key/backlog" element={<Backlog />} />
            <Route path="/projects/:key/list" element={<IssueList />} />
            <Route path="/projects/:key/timeline" element={<Timeline />} />
            <Route path="/projects/:key/calendar" element={<Calendar />} />
            <Route path="/projects/:key/reports" element={<Reports />} />
            <Route path="/projects/:key/releases" element={<Releases />} />
            <Route path="/projects/:key/docs" element={<ProjectDocs />} />
            <Route path="/projects/:key/queue" element={<Queue />} />
            <Route path="/projects/:key/settings/*" element={<ProjectSettings />} />
            <Route path="/issues/:key" element={<IssuePage />} />
            <Route path="/search" element={<IssueList />} />
            <Route path="/timesheet" element={<Timesheet />} />
            <Route path="/people" element={<People />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/admin/*" element={<Admin />} />
            <Route path="*" element={<div className="empty">{t('Not found')}</div>} />
          </Route>
        </Routes>
      </Suspense>
    </ToastProvider>
  )
}

/** A project's start page: the board for scrum and kanban projects, the list for trackers. */
function ProjectStart() {
  const { key } = useParams()
  const p = useProject(key)
  if (p.isLoading) return <Loading />
  if (!p.data) return <div className="empty">{t('Not found')}</div>
  return <Navigate to={`/projects/${p.data.key}/${p.data.kind === 'tracker' ? 'list' : p.data.kind === 'scrum' ? 'backlog' : 'board'}`} replace />
}
