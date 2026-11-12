import { defineConfig, devices } from '@playwright/test'

const port = process.env.E2E_PORT ?? '8798'
export const baseURL = process.env.E2E_BASE_URL ?? `http://127.0.0.1:${port}`

export default defineConfig({
  testDir: './tests',
  // one journey through a fresh workspace: the steps build on each other
  workers: 1,
  fullyParallel: false,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  retries: 0,
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : 'list',
  use: { baseURL, trace: 'retain-on-failure', screenshot: 'only-on-failure' },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: process.env.E2E_BASE_URL
    ? undefined
    : {
        command: '../scripts/e2e-server.sh',
        url: `${baseURL}/api/health`,
        timeout: 600_000,
        reuseExistingServer: !process.env.CI,
        env: { E2E_PORT: port },
        stdout: 'pipe',
      },
})
