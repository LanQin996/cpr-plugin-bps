import { defineConfig } from '@playwright/test'
export default defineConfig({
  testDir: './tests',
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:5177', viewport: { width: 1200, height: 800 }, headless: true },
  webServer: { command: 'pnpm dev --port 5177 --strictPort', url: 'http://127.0.0.1:5177', reuseExistingServer: !process.env.CI },
})
