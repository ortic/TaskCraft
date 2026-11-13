import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { QueryClientProvider } from '@tanstack/react-query'
import { queryClient } from './queryClient'
import './styles.css'
import { chooseLanguage, loadLanguage, rememberedLanguage } from './i18n'


try {
  const theme = localStorage.getItem('tc-theme')
  if (theme) document.documentElement.dataset.theme = theme
} catch {
  /* storage unavailable */
}

// the language first, then the app: labels defined when modules load are translated too. The
// service desk portal (/portal) is a small app of its own for people without an account, in their
// browser's language.
async function start() {
  const portal = location.pathname === '/portal' || location.pathname.startsWith('/portal/')
  // shared pages (/share/…): read-only pages a project shares by link, also for people without an account
  const shared = location.pathname.startsWith('/share/')
  await loadLanguage(portal || shared ? chooseLanguage() : (rememberedLanguage() ?? chooseLanguage()))
  const { default: App } = portal ? await import('./portal/Portal') : shared ? await import('./share/Share') : await import('./App')
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </QueryClientProvider>
    </StrictMode>,
  )
}
void start()
