// Screenshots for the website (site/screenshots), taken from a running TaskCraft with the sample
// data. Usage, from e2e/ after `npm ci && npx playwright install chromium`:
//
//   node screenshots.mjs <base URL> <e-mail> <password> <dashboard id> [out dir]
//
// Start a fresh instance, set it up with the sample data and a shared dashboard first (see
// site/README.md).
import { chromium } from '@playwright/test'
import { mkdirSync } from 'node:fs'

const [base, email, password, dashboard, out = '../site/screenshots'] = process.argv.slice(2)
if (!base || !email || !password || !dashboard) {
  console.error('usage: node screenshots.mjs <base URL> <e-mail> <password> <dashboard id> [out dir]')
  process.exit(2)
}
mkdirSync(out, { recursive: true })

const SHOTS = [
  { name: 'board', path: '/projects/WEB/board', ready: (p) => p.locator('.issue-card').first() },
  { name: 'backlog', path: '/projects/WEB/backlog', ready: (p) => p.locator('.issue-key').first() },
  { name: 'issue', path: '/projects/WEB/board?issue=WEB-13', ready: (p) => p.locator('.issue-panel').getByText('Save the cart for signed-in shoppers').first() },
  { name: 'timeline', path: '/projects/MKT/timeline', ready: (p) => p.locator('main').getByText('Photo shoot').first() },
  { name: 'reports', path: '/projects/WEB/reports', ready: (p) => p.locator('main svg').first(), then: (p) => p.getByRole('tab', { name: 'Burndown' }).click() },
  { name: 'dashboard', path: `/dashboards/${dashboard}`, ready: (p) => p.locator('.dash-widget').first() },
  { name: 'portfolio', path: '/portfolio?projects=WEB,MKT', ready: (p) => p.locator('.pf-chart') },
]

const browser = await chromium.launch()
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1.5, colorScheme: 'light', locale: 'en-US' })
const page = await context.newPage()
await page.goto(`${base}/login`)
await page.getByLabel('E-mail').fill(email)
await page.getByLabel('Password').fill(password)
await page.getByRole('button', { name: 'Log in' }).click()
await page.waitForURL((u) => !u.pathname.startsWith('/login'))
for (const s of SHOTS) {
  await page.goto(base + s.path)
  await s.ready(page).waitFor({ timeout: 15000 })
  if (s.then) await s.then(page)
  // charts animate in and avatars load: let the page settle (live updates keep a request open,
  // so there is no network-idle moment to wait for)
  await page.waitForTimeout(1500)
  await page.mouse.move(0, 899)
  await page.screenshot({ path: `${out}/${s.name}.png` })
  console.log(`${s.name}.png`)
}
await browser.close()
