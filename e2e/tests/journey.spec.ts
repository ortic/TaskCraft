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

test('command palette finds issues', async ({ page }) => {
  await login(page)
  await page.keyboard.press('Control+k')
  await page.getByPlaceholder(/Search issues by key or words/).fill('cart')
  await page.getByRole('button', { name: /Save the cart for signed-in shoppers/ }).click()
  await expect(page.locator('.issue-panel').getByRole('textbox', { name: 'Title' })).toHaveValue('Save the cart for signed-in shoppers')
})
