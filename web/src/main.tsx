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

// the language first, then the app: labels defined when modules load are translated too
async function start() {
  await loadLanguage(rememberedLanguage() ?? chooseLanguage())
  const { default: App } = await import('./App')
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
