// One journey through a fresh workspace with sample data: the steps build on each other.
import { test, expect, type Page } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

const OWNER = { email: 'owner@example.com', password: 'password123' }

async function a11y(page: Page, include?: string) {
  let b = new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'])
  if (include) b = b.include(include)
  const r = await b.analyze()
  const serious = r.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical')
  expect(serious.map((v) => `${v.id}: ${v.nodes.slice(0, 3).map((n) => n.target.join(' ')).join(' | ')}`)).toEqual([])
}

test.describe.configure({ mode: 'serial' })

test('setup with sample data', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Welcome to TaskCraft' })).toBeVisible()
  await page.getByLabel('Workspace name').fill('Acme')
  await page.getByLabel('Your name').fill('Olivia Owner')
  await page.getByLabel('E-mail').fill(OWNER.email)
  await page.getByLabel('Password').fill(OWNER.password)
  await page.getByRole('button', { name: 'Create workspace' }).click()
  await expect(page.getByRole('heading', { name: /Olivia/ })).toBeVisible({ timeout: 60_000 })
  await expect(page.getByRole('link', { name: /Web Platform/ }).first()).toBeVisible()
  await a11y(page)
})

async function login(page: Page) {
  await page.goto('/login')
  await page.getByLabel('E-mail').fill(OWNER.email)
  await page.getByLabel('Password').fill(OWNER.password)
  await page.getByRole('button', { name: 'Log in' }).click()
  await expect(page.getByRole('heading', { name: /Olivia/ })).toBeVisible()
}

test('board: drag a card to another column', async ({ page }) => {
  await login(page)
  await page.goto('/projects/WEB/board')
  const card = page.locator('.issue-card', { hasText: 'Profile page: saved addresses' })
  await expect(card).toBeVisible()
  const review = page.locator('.column', { has: page.locator('.column-head', { hasText: 'In review' }) }).locator('.column-body')
  const saved = page.waitForResponse((r) => r.url().includes('/move') && r.request().method() === 'POST')
  await card.dragTo(review)
  await expect(review.locator('.issue-card', { hasText: 'Profile page: saved addresses' })).toBeVisible()
  expect((await saved).ok()).toBe(true)
  await page.reload()
  await expect(page.locator('.column', { has: page.locator('.column-head', { hasText: 'In review' }) }).locator('.issue-card', { hasText: 'Profile page' })).toBeVisible()
  await a11y(page, '.main')
})

test('create an issue, then edit it in the side panel', async ({ page }) => {
  await login(page)
  await page.goto('/projects/WEB/list')
  await expect(page.locator('.grid-table')).toBeVisible()
  await page.keyboard.press('c')
  await expect(page.getByRole('dialog', { name: /Create issue/ })).toBeVisible()
  await page.getByLabel('Type').selectOption('Bug')
  await page.getByLabel('Title').fill('Checkout button misaligned on tablets')
  await page.getByRole('dialog').getByRole('button', { name: 'Create', exact: true }).click()
  await page.locator('.toast').getByRole('button', { name: 'Open' }).click()
  const panel = page.locator('.issue-panel')
  await expect(panel.getByRole('textbox', { name: 'Title' })).toHaveValue('Checkout button misaligned on tablets')
  // the sample automation labels new bugs
  await expect(panel.locator('.label-chip', { hasText: 'triage' })).toBeVisible()
  // priority
  await panel.getByRole('button', { name: 'Priority' }).click()
  await page.getByRole('option', { name: /Urgent/ }).click()
  await expect(panel.getByRole('button', { name: 'Priority' })).toContainText('Urgent')
  // a comment
  await panel.locator('.rte.boxed .ProseMirror').click()
  await page.keyboard.type('Reproduced on an iPad in portrait mode.')
  await panel.getByRole('button', { name: 'Comment', exact: true }).click()
  await expect(panel.locator('.comment-box', { hasText: 'Reproduced on an iPad' })).toBeVisible()
  // time
  await panel.getByRole('tab', { name: /Time/ }).click()
  await panel.getByLabel('Time spent').first().fill('45m')
  await panel.getByRole('button', { name: 'Log time' }).click()
  await expect(panel.getByText('45m in total')).toBeVisible()
  // history
  await panel.getByRole('tab', { name: /History/ }).click()
  await expect(panel.getByText(/changed the priority from Medium to Urgent/)).toBeVisible()
  await a11y(page, '.issue-panel')
})

test('backlog: sprints, ranking and velocity hint', async ({ page }) => {
  await login(page)
  await page.goto('/projects/WEB/backlog')
  await expect(page.getByRole('heading', { name: 'Backlog', level: 1 })).toBeVisible()
  await expect(page.locator('.sprint-box', { hasText: 'WEB Sprint 2' })).toContainText('active')
  await expect(page.locator('.sprint-box', { hasText: 'WEB Sprint 3' })).toContainText(/of ~\d+ pts/)
  // drag a backlog issue into the planned sprint
  const row = page.locator('.issue-row', { hasText: 'Delete my account' })
  const moved = page.waitForResponse((r) => r.url().includes('/move') && r.request().method() === 'POST')
  await row.dragTo(page.locator('.sprint-box', { hasText: 'WEB Sprint 3' }).locator('.issue-rows'))
  await expect(page.locator('.sprint-box', { hasText: 'WEB Sprint 3' }).locator('.issue-row', { hasText: 'Delete my account' })).toBeVisible()
  expect((await moved).ok()).toBe(true)
  await a11y(page, '.main')
})

test('list: query language, saved filter', async ({ page }) => {
  await login(page)
  await page.goto('/search')
  const q = page.getByRole('textbox', { name: 'Query' })
  await q.fill('type = Bug and category != done order by priority desc')
  await q.press('Enter')
  await expect(page.locator('.grid-table tbody tr').first()).toContainText(/WEB-|SUP-/)
  await q.fill('status > Done')
  await expect(page.getByText(/can be compared with/)).toBeVisible()
  await q.fill('type = Bug')
  await q.press('Enter')
  await page.getByRole('button', { name: 'Save filter' }).click()
  await page.getByLabel('Name').fill('All bugs')
  await page.getByRole('dialog').getByRole('button', { name: 'Save' }).click()
  await expect(page.locator('.sidebar').getByRole('link', { name: 'All bugs' })).toBeVisible()
})

test('reports and timeline render', async ({ page }) => {
  await login(page)
  await page.goto('/projects/WEB/reports')
  await expect(page.locator('main svg').first()).toBeVisible()
  await page.goto('/projects/MKT/timeline')
  await expect(page.locator('main').getByText('Photo shoot').first()).toBeVisible()
  await page.goto('/projects/WEB/calendar')
  await expect(page.locator('main')).toBeVisible()
})

test('dashboards and portfolio', async ({ page }) => {
  await login(page)
  await page.goto('/dashboards')
  await page.getByLabel('New dashboard name').fill('Journey dashboard')
  await page.getByLabel('New dashboard name').press('Enter')
  await expect(page.getByRole('heading', { name: 'Journey dashboard' })).toBeVisible()
  await expect(page.locator('.dash-widget').first()).toBeVisible()
  await a11y(page, 'main')
  await page.goto('/portfolio')
  await expect(page.getByRole('table', { name: 'Portfolio timeline' })).toBeVisible()
  await expect(page.getByRole('button', { name: /Checkout redesign/ }).first()).toBeVisible()
  await a11y(page, 'main')
})

/** The sign-in link the mail sink (mail-sink.mjs) got for `email`. */
async function signInLink(email: string): Promise<string> {
  const sink = `http://127.0.0.1:${process.env.E2E_MAIL_PORT ?? '8797'}/messages`
  for (let i = 0; i < 60; i++) {
    const mails: { to: string[]; text: string }[] = await (await fetch(sink)).json()
    const link = mails
      .filter((m) => m.to.includes(email))
      .map((m) => /https?:\/\/\S+\/portal\/sign-in\?token=[\w-]+(&request=[\w-]+)?/.exec(m.text)?.[0])
      .filter(Boolean)
      .pop()
    if (link) return link
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error(`no sign-in link for ${email}`)
}

test('service desk: a public request form, the portal, answers both ways', async ({ page, browser, baseURL }) => {
  test.setTimeout(120_000)
  // an admin publishes a request form for the support project
  await login(page)
  await page.goto('/projects/SUP/settings/service-desk')
  await page.getByLabel(/Publish a request form for/).check()
  await page.getByLabel('Title', { exact: true }).fill('Ask the support team')
  await page.getByLabel('Description', { exact: true }).selectOption('required')
  await page.getByLabel('Requesters can mark their requests as resolved').check()
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.locator('.toast', { hasText: 'Saved' })).toBeVisible()
  await a11y(page, '.ps-content')

  // someone without an account sends it
  const visitor = await browser.newContext({ baseURL, locale: 'en-US' })
  const p = await visitor.newPage()
  const email = `jane.${Date.now()}@example.org`
  await p.goto('/portal/forms/sup')
  await expect(p.getByRole('heading', { name: 'Ask the support team', level: 1 })).toBeVisible()
  await a11y(p)
  await p.getByLabel('Your e-mail address').fill(email)
  await p.getByLabel('Your name (optional)').fill('Jane Doe')
  await p.getByLabel('Summary').fill('The invoice shows the wrong address')
  await p.getByLabel('Description', { exact: true }).fill('Invoice 4711 has our old address.')
  await p.getByRole('button', { name: 'Send request' }).click()
  await expect(p.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  await a11y(p)

  // she signs in with a link e-mailed to her
  await p.goto('/portal')
  await expect(p.getByRole('heading', { name: 'Follow your requests', level: 1 })).toBeVisible()
  await p.keyboard.press('Tab')
  await expect(p.getByRole('link', { name: 'Skip to content' })).toBeFocused()
  await p.getByLabel('Your e-mail address').fill(email)
  await p.getByRole('button', { name: 'E-mail me a sign-in link' }).click()
  await expect(p.getByText('Check your e-mail.')).toBeVisible()
  await a11y(p)
  await p.goto(await signInLink(email))
  await p.getByRole('button', { name: 'Continue' }).click()
  await expect(p.getByRole('heading', { name: 'Your requests', level: 1 })).toBeVisible()
  await a11y(p)
  await p.getByRole('link', { name: /The invoice shows the wrong address/ }).click()
  await expect(p.getByRole('heading', { name: 'The invoice shows the wrong address', level: 1 })).toBeVisible()
  await expect(p.getByText('Invoice 4711 has our old address.')).toBeVisible()
  const key = new URL(p.url()).pathname.split('/').pop()!

  // she answers
  await p.getByLabel('Your message').fill('The new address is Main Street 1.')
  await p.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(p.locator('.portal-message.mine', { hasText: 'Main Street 1' })).toBeVisible()
  await a11y(p)

  // the team sees where it came from, notes something internal and answers her
  await page.goto(`/issues/${key}`)
  await expect(page.getByText('via the request form')).toBeVisible()
  await expect(page.locator('.comment-box', { hasText: 'Main Street 1' })).toBeVisible()
  const box = page.locator('.rte.boxed .ProseMirror')
  await box.click()
  await page.keyboard.type('Internal: check the billing export.')
  await page.getByRole('button', { name: 'Comment', exact: true }).click()
  await expect(page.locator('.comment-box', { hasText: 'billing export' })).toBeVisible()
  await box.click()
  await page.keyboard.type('Thanks, we fixed the address.')
  await page.getByLabel(/Visible to the requester/).check()
  await page.getByRole('button', { name: 'Send reply' }).click()
  await expect(page.locator('.comment-box', { hasText: 'we fixed the address' })).toBeVisible()

  // she sees the answer, not the internal note, and marks the request resolved
  await p.reload()
  await expect(p.locator('.portal-message', { hasText: 'we fixed the address' })).toBeVisible()
  await expect(p.getByText('billing export')).toHaveCount(0)
  await p.getByRole('button', { name: 'Mark as resolved' }).click()
  await expect(p.locator('.portal-news')).toContainText('Marked as resolved')
  await expect(p.locator('.portal-meta .portal-state')).toHaveText('Resolved')
  await a11y(p)
  await visitor.close()
})

test('command palette finds issues', async ({ page }) => {
  await login(page)
  await page.keyboard.press('Control+k')
  await page.getByPlaceholder(/Search issues by key or words/).fill('cart')
  await page.getByRole('button', { name: /Save the cart for signed-in shoppers/ }).click()
  await expect(page.locator('.issue-panel').getByRole('textbox', { name: 'Title' })).toHaveValue('Save the cart for signed-in shoppers')
})
