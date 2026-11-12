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

test('API reference', async ({ page }) => {
  await login(page)
  await page.goto('/settings')
  await page.getByRole('link', { name: /API reference/ }).click()
  await expect(page.getByRole('heading', { name: 'API reference' })).toBeVisible()
  await page.getByLabel('Filter operations').fill('issues key')
  const op = page.locator('details.ar-op', { hasText: '/issues/{key}' }).first()
  await op.locator('summary').click()
  await expect(op.getByRole('heading', { name: 'Responses' })).toBeVisible()
  await expect(op.locator('pre')).toContainText('/api/v1/issues/{key}')
  await a11y(page, 'main')
})

test('workflow rules ask for what a status needs', async ({ page }) => {
  await login(page)
  // In review needs a due date and a comment
  await page.goto('/projects/WEB/settings/workflow')
  await page.getByRole('button', { name: 'Rules · In review' }).click()
  const rules = page.getByRole('dialog')
  await rules.getByLabel('Due date').check()
  await rules.getByRole('radio', { name: 'Required' }).click()
  await a11y(page, '[role=dialog]')
  await rules.getByRole('button', { name: 'Save' }).click()
  await expect(page.getByRole('button', { name: '2 rules · In review' })).toBeVisible()
  // dragging a card there asks for them
  await page.goto('/projects/WEB/board')
  const card = page.locator('.issue-card', { hasText: 'Write the merge rules' })
  const review = page.locator('.column', { has: page.locator('.column-head', { hasText: 'In review' }) }).locator('.column-body')
  await card.dragTo(review)
  const form = page.getByRole('dialog')
  await expect(form.getByText('In review needs these filled in:')).toBeVisible()
  await form.getByRole('button', { name: 'Move' }).click()
  await expect(form.getByRole('alert')).toContainText('Due date')
  await form.locator('input[type=date]').fill('2030-01-31')
  await form.getByLabel(/Comment/).fill('Ready for review.')
  await a11y(page, '[role=dialog]')
  const saved = page.waitForResponse((r) => r.url().includes('/move') && r.request().method() === 'POST')
  await form.getByRole('button', { name: 'Move' }).click()
  expect((await saved).ok()).toBe(true)
  await expect(review.locator('.issue-card', { hasText: 'Write the merge rules' })).toBeVisible()
  // no rules again for the steps after this one
  await page.evaluate(async () => {
    const p = await (await fetch('/api/projects/WEB')).json()
    const id = p.statuses.find((s: { name: string }) => s.name === 'In review').id
    await fetch(`/api/statuses/${id}`, { method: 'PATCH', headers: { 'content-type': 'application/json', 'x-taskcraft': '1' }, body: JSON.stringify({ rules: {} }) })
  })
})

test('private issues with security levels', async ({ page }) => {
  await login(page)
  await page.goto('/projects/WEB/settings/permissions')
  await page.getByRole('button', { name: 'Add level' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('Security team')
  await dialog.getByLabel('By project role').selectOption('')
  await a11y(page, '[role=dialog]')
  await dialog.getByRole('button', { name: 'Save' }).click()
  await expect(page.locator('.ps-level-row', { hasText: 'Security team' })).toContainText('project admins only')
  // make an issue private from its side panel
  const key = await page.evaluate(async () => (await (await fetch('/api/search?q=' + encodeURIComponent('project = WEB order by key'))).json()).issues[0].key as string)
  await page.goto(`/issues/${key}`)
  await page.getByRole('button', { name: 'Security level' }).click()
  await page.getByRole('option', { name: /Security team/ }).click()
  await expect(page.locator('.badge-private')).toContainText('Security team')
  await a11y(page)
  // public again, and the level gone, for the steps after this one
  await page.evaluate(async (key) => {
    const h = { 'content-type': 'application/json', 'x-taskcraft': '1' }
    const p = await (await fetch('/api/projects/WEB')).json()
    await fetch(`/api/issues/${key}`, { method: 'PATCH', headers: h, body: JSON.stringify({ securityLevelId: null }) })
    for (const l of p.securityLevels) await fetch(`/api/security-levels/${l.id}?replaceWith=none`, { method: 'DELETE', headers: h })
  }, key)
})

test('time categories, remaining estimate and the time report', async ({ page }) => {
  await login(page)
  await page.goto('/admin/time')
  await page.getByLabel('New category name').fill('Development')
  await page.getByLabel('Billable by default').check()
  await page.getByRole('button', { name: 'Add category' }).click()
  await expect(page.getByLabel('Name of Development')).toBeVisible()
  await a11y(page)
  // an issue without time logged, estimated at 10h
  const key = await page.evaluate(async () => {
    const found = (await (await fetch('/api/search?limit=200&q=' + encodeURIComponent('project = WEB order by key'))).json()).issues as { key: string; spentMinutes: number }[]
    const key = found.find((i) => i.spentMinutes === 0)!.key
    await fetch(`/api/issues/${key}`, { method: 'PATCH', headers: { 'content-type': 'application/json', 'x-taskcraft': '1' }, body: JSON.stringify({ estimateMinutes: 600 }) })
    return key
  })
  await page.goto(`/issues/${key}`)
  await page.getByRole('tab', { name: /Time/ }).click()
  await page.getByLabel('Time spent').first().fill('2h')
  await page.getByLabel('Time category').selectOption({ label: 'Development' })
  await expect(page.getByLabel('Billable')).toBeChecked()
  await page.getByRole('button', { name: 'Log time' }).click()
  await expect(page.locator('.badge', { hasText: 'Development' })).toBeVisible()
  await expect(page.getByText(/2h logged · 8h left/)).toBeVisible()
  await page.goto('/time-report?period=this-year&group=category')
  await expect(page.locator('.tr-table')).toContainText('Development')
  await expect(page.getByRole('link', { name: 'Download CSV' })).toHaveAttribute('href', /format=csv/)
  await a11y(page)
})

test('command palette finds issues', async ({ page }) => {
  await login(page)
  await page.keyboard.press('Control+k')
  await page.getByPlaceholder(/Search issues by key or words/).fill('cart')
  await page.getByRole('button', { name: /Save the cart for signed-in shoppers/ }).click()
  await expect(page.locator('.issue-panel').getByRole('textbox', { name: 'Title' })).toHaveValue('Save the cart for signed-in shoppers')
})
