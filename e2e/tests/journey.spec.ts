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

/** A zip archive (stored, not compressed) of the given files. */
function zip(files: Record<string, string | Buffer>): Buffer {
  const crc32 = (b: Buffer) => {
    let crc = 0xffffffff
    for (const byte of b) {
      crc ^= byte
      for (let k = 0; k < 8; k++) crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1
    }
    return (crc ^ 0xffffffff) >>> 0
  }
  const locals: Buffer[] = []
  const central: Buffer[] = []
  let offset = 0
  for (const [name, content] of Object.entries(files)) {
    const data = Buffer.isBuffer(content) ? content : Buffer.from(content)
    const n = Buffer.from(name)
    const crc = crc32(data)
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(0x21, 12)
    local.writeUInt32LE(crc, 14)
    local.writeUInt32LE(data.length, 18)
    local.writeUInt32LE(data.length, 22)
    local.writeUInt16LE(n.length, 26)
    const dir = Buffer.alloc(46)
    dir.writeUInt32LE(0x02014b50, 0)
    dir.writeUInt16LE(20, 4)
    dir.writeUInt16LE(20, 6)
    dir.writeUInt16LE(0x21, 14)
    dir.writeUInt32LE(crc, 16)
    dir.writeUInt32LE(data.length, 20)
    dir.writeUInt32LE(data.length, 24)
    dir.writeUInt16LE(n.length, 28)
    dir.writeUInt32LE(offset, 42)
    locals.push(local, n, data)
    central.push(dir, n)
    offset += local.length + n.length + data.length
  }
  const size = central.reduce((s, b) => s + b.length, 0)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(central.length / 2, 8)
  end.writeUInt16LE(central.length / 2, 10)
  end.writeUInt32LE(size, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...locals, ...central, end])
}

test('import a backup from another tracker', async ({ page }) => {
  // a small synthetic XML site backup
  const entities = `<?xml version="1.0" encoding="UTF-8"?>
<entity-engine-xml>
  <User id="1" userName="lee" lowerUserName="lee" active="1" displayName="Lee Legacy" emailAddress="lee@example.com"/>
  <ApplicationUser id="1" userKey="u-lee" lowerUserName="lee"/>
  <Project id="10" key="LEG" name="Legacy tracker" lead="u-lee" projecttype="software"/>
  <IssueType id="1" name="Task"/>
  <Status id="1" name="Open" statuscategory="2"/>
  <Status id="2" name="Closed" statuscategory="3"/>
  <Priority id="1" name="High" sequence="1"/>
  <Issue id="100" project="10" number="1" type="1" status="2" priority="1" summary="Move the wiki" reporter="u-lee" created="2026-01-05 09:00:00.0" updated="2026-01-09 17:00:00.0" resolution="1" resolutiondate="2026-01-09 17:00:00.0"/>
  <Issue id="101" project="10" number="2" type="1" status="1" priority="1" summary="Archive old releases" reporter="u-lee" assignee="u-lee" created="2026-01-06 09:00:00.0" updated="2026-01-06 09:00:00.0"><description>h2. Why
Keep *only* what we use.</description></Issue>
  <Action id="1" issue="101" author="u-lee" type="comment" created="2026-01-07 10:00:00.0" body="Started on it."/>
  <FileAttachment id="7" issue="101" filename="list.txt" mimetype="text/plain" filesize="5" author="u-lee" created="2026-01-07 10:00:00.0"/>
</entity-engine-xml>`
  const backup = zip({ 'entities.xml': entities, 'data/attachments/LEG/10000/LEG-2/7': 'a\nb\nc' })
  await login(page)
  await page.goto('/projects')
  await page.getByRole('link', { name: 'Import', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Import a backup from another tracker' })).toBeVisible()
  await a11y(page, 'main')
  await page.getByLabel('Choose the .zip file or drop it here').setInputFiles({ name: 'site-backup.zip', mimeType: 'application/zip', buffer: backup })
  await page.getByRole('button', { name: 'Upload and check' }).click()
  await expect(page.getByRole('heading', { name: 'What the backup holds' })).toBeVisible()
  const row = page.getByRole('row', { name: /Legacy tracker/ })
  await expect(row).toContainText('LEG')
  await expect(page.getByLabel('Key of the new project for LEG')).toHaveValue('LEG')
  await page.getByRole('radio', { name: /Keep them as deactivated accounts/ }).check()
  await a11y(page, 'main')
  await page.getByRole('button', { name: 'Import 2 issues' }).click()
  await expect(page.getByRole('heading', { name: 'Import finished' })).toBeVisible()
  // a reload keeps the result
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Import finished' })).toBeVisible()
  await a11y(page, 'main')
  await page.getByRole('link', { name: 'LEG · Legacy tracker' }).click()
  await page.goto('/issues/LEG-2')
  await expect(page.getByRole('textbox', { name: 'Title' })).toHaveValue('Archive old releases')
  await expect(page.locator('main').getByText('list.txt')).toBeVisible()
  await expect(page.locator('main').getByText('Started on it.')).toBeVisible()
})

test('command palette finds issues', async ({ page }) => {
  await login(page)
  await page.keyboard.press('Control+k')
  await page.getByPlaceholder(/Search issues by key or words/).fill('cart')
  await page.getByRole('button', { name: /Save the cart for signed-in shoppers/ }).click()
  await expect(page.locator('.issue-panel').getByRole('textbox', { name: 'Title' })).toHaveValue('Save the cart for signed-in shoppers')
})
