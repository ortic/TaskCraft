// Docs: the project's WikiCraft space — recent pages, search and a link to the space.
import { t } from '../i18n'
import { useEffect, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { BookOpen, ExternalLink, FileText, Plug, Search, Settings, X } from 'lucide-react'
import { ago, api, qs, type WikiPage } from '../api'
import { Empty, ErrorBox, Loading, Spinner, useMe, useProject } from '../ui'
import './ProjectDocs.css'

interface ProjectWiki {
  connected: boolean
  space: string | null
  spaceUrl?: string | null
  pages: WikiPage[]
}

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value)
  useEffect(() => {
    const h = setTimeout(() => setV(value), ms)
    return () => clearTimeout(h)
  }, [value, ms])
  return v
}

export default function ProjectDocs() {
  const { key } = useParams()
  const project = useProject(key)
  const me = useMe().data
  const wiki = useQuery({
    queryKey: ['project-wiki', key?.toUpperCase()],
    queryFn: () => api.get<ProjectWiki>(`/projects/${encodeURIComponent(key!)}/wiki`),
    enabled: !!key,
  })
  const [q, setQ] = useState('')
  const query = useDebounced(q.trim(), 250)
  const space = wiki.data?.space ?? null
  const results = useQuery({
    queryKey: ['wiki-search', space, query],
    queryFn: () => api.get<WikiPage[]>(`/integrations/wikicraft/search${qs({ q: query, space })}`),
    enabled: !!wiki.data?.connected && !!space && query.length > 0,
  })

  if (project.isLoading || wiki.isLoading) return <Loading />
  if (project.error) return <div className="page"><ErrorBox error={project.error} /></div>
  if (!project.data) return null
  const p = project.data
  const isAdmin = p.role === 'admin'

  const head = (
    <div className="page-head">
      <h1><BookOpen size={22} aria-hidden /> {t('Docs')}</h1>
      <span className="spacer" />
      {wiki.data?.connected && space && wiki.data.spaceUrl && (
        <a className="btn" href={wiki.data.spaceUrl} target="_blank" rel="noreferrer noopener">
          <ExternalLink size={15} /> {t('Open space')}
        </a>
      )}
      {isAdmin && (
        <Link className="btn btn-ghost" to={`/projects/${p.key}/settings/integrations`} title={t('Wiki settings')}>
          <Settings size={15} /> <span className="doc-hide-sm">{t('Wiki settings')}</span>
        </Link>
      )}
    </div>
  )

  if (wiki.error) {
    return (
      <div className="page doc-page">
        {head}
        <ErrorBox error={wiki.error} />
      </div>
    )
  }

  if (!wiki.data?.connected) {
    // a server address without a shared token: people connect with their own token
    const known = !!me?.wikicraft?.url
    return (
      <div className="page doc-page">
        {head}
        <Empty icon={<Plug size={34} />} title={t('WikiCraft isn’t connected')}>
          <p>
            {known
              ? t('This server knows WikiCraft but has no shared access token. Add your personal WikiCraft token under Profile & settings (or ask an administrator to set TASKCRAFT_WIKICRAFT_TOKEN) to see this project’s documentation here.')
              : t('A server administrator connects WikiCraft by setting TASKCRAFT_WIKICRAFT_URL and TASKCRAFT_WIKICRAFT_TOKEN. Once the address is set, people can also connect with a personal token under Profile & settings.')}
          </p>
          {known && (
            <div className="doc-empty-actions">
              <Link className="btn" to="/settings">{t('Open Profile & settings')}</Link>
            </div>
          )}
        </Empty>
      </div>
    )
  }

  if (!space) {
    return (
      <div className="page doc-page">
        {head}
        <Empty icon={<BookOpen size={34} />} title={t('No wiki space chosen')}>
          <p>{t('Pick the WikiCraft space that holds this project’s documentation, release notes and sprint reviews.')}</p>
          {isAdmin ? (
            <div className="doc-empty-actions">
              <Link className="btn btn-primary" to={`/projects/${p.key}/settings/integrations`}>
                <Settings size={15} /> {t('Choose a wiki space')}
              </Link>
            </div>
          ) : (
            <p className="faint">{t('Ask a project admin to choose one in the project settings.')}</p>
          )}
        </Empty>
      </div>
    )
  }

  const searching = query.length > 0
  const list = searching ? results.data ?? [] : wiki.data.pages

  return (
    <div className="page doc-page">
      {head}
      <div className="doc-bar">
        <div className="doc-space">
          <span className="faint small">{t('Space')}</span>
          <span className="doc-space-key">{wiki.data.pages[0]?.spaceName || space}</span>
          {wiki.data.pages[0]?.spaceName && wiki.data.pages[0].spaceName !== space && <span className="issue-key">{space}</span>}
        </div>
        <div className="doc-search">
          <Search size={15} className="doc-search-icon" aria-hidden />
          <input
            className="input"
            type="search"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => e.key === 'Escape' && q && (e.stopPropagation(), setQ(''))}
            placeholder={t('Search this space…')}
            aria-label={t('Search this space')}
          />
          {searching && results.isFetching && <span className="doc-search-spin"><Spinner size={14} /></span>}
          {q && !results.isFetching && (
            <button className="icon-btn icon-btn-sm doc-search-clear" onClick={() => setQ('')} aria-label={t('Clear search')} title={t('Clear search')}>
              <X size={14} />
            </button>
          )}
        </div>
      </div>

      <h2 className="doc-list-title">{searching ? t('Search results') : t('Recently updated')}</h2>

      {searching && results.error ? (
        <ErrorBox error={results.error} />
      ) : searching && results.isLoading ? (
        <Loading />
      ) : list.length === 0 ? (
        <Empty icon={<FileText size={30} />} title={searching ? t('No pages match “{q}”', { q: query }) : t('No pages yet')}>
          {searching
            ? t('Try other words, or open the space to browse it.')
            : t('Pages written in the {space} space show up here, newest first.', { space })}
        </Empty>
      ) : (
        <ul className="doc-list">
          {list.map((pg) => (
            <li key={pg.id}>
              <a className="doc-page-item" href={pg.url} target="_blank" rel="noreferrer noopener">
                <FileText size={18} className="doc-page-icon" aria-hidden />
                <span className="doc-page-main">
                  <span className="doc-page-title">
                    {pg.title || t('Untitled')}
                    <ExternalLink size={12} className="doc-ext" aria-hidden />
                  </span>
                  {pg.excerpt && <span className="doc-excerpt">{pg.excerpt}</span>}
                  <span className="doc-meta">
                    {pg.spaceName && pg.spaceKey !== space && <span>{pg.spaceName}</span>}
                    {pg.updatedAt && <span>{t('Updated {when}', { when: ago(pg.updatedAt) })}</span>}
                  </span>
                </span>
              </a>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
