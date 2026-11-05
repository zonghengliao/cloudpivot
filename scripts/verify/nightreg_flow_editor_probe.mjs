#!/usr/bin/env node
/**
 * 链路⑥ 补充证据：绕开「列表行内 t-link 点不动」的既有坑（见 SKILL 的 e2e 陷阱），
 * 用深链直接进已发布流程的编辑器，验证：画布能加载（节点数 > 0）、只读横幅在、
 * 并截图存档。
 *
 * 用法：PLAYWRIGHT_MODULE=<...> node scripts/verify/nightreg_flow_editor_probe.mjs [baseUrl] [defId] [outDir]
 */
import fs from 'node:fs'
import path from 'node:path'

const PW = process.env.PLAYWRIGHT_MODULE || 'playwright-core'
const { chromium } = await import(PW)

const BASE = process.argv[2] || 'http://127.0.0.1:5273'
const DEF = process.argv[3] || '20000000-0000-4000-8000-0000000a0182'
const OUT = process.argv[4] || 'docs/evidence/20261104-nightreg/shots/flow-editor'
const USER = process.env.NR_ADMIN_USER || 'admin'
const PASS = process.env.NR_ADMIN_PASS || 'Admin@123456'

fs.mkdirSync(OUT, { recursive: true })

const resp = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PASS }),
})
const login = await resp.json()
const access = login?.data?.access_token
if (!access) {
  console.error('登录失败', JSON.stringify(login).slice(0, 200))
  process.exit(1)
}

const browser = await chromium.launch()
const ctx = await browser.newContext({ viewport: { width: 1600, height: 950 } })
await ctx.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a)
    localStorage.setItem('x_refresh_token', r || '')
    localStorage.setItem('cp-onboarding-done', '1')
  },
  [access, login?.data?.refresh_token || ''],
)
const page = await ctx.newPage()
const url = `${BASE}/flow/def/${DEF}/edit`
await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 60000 })
await page.waitForSelector('[data-node-key], .vue-flow__node', { timeout: 45000 }).catch(() => {})
await page.waitForTimeout(2500)
for (const t of ['知道了', '我知道了', '关闭']) {
  const btn = page.locator(`.t-dialog button:has-text("${t}")`).first()
  if (await btn.count()) await btn.click({ timeout: 2000 }).catch(() => {})
}
await page.waitForTimeout(800)

const nodes = await page.locator('[data-node-key]').count()
const vfNodes = await page.locator('.vue-flow__node').count()
const edges = await page.locator('.vue-flow__edge').count()
const bodyText = await page.locator('body').innerText()
const readonly = /已发布/.test(bodyText)
const shot = path.join(OUT, 'chain6-01-published-canvas.png')
await page.screenshot({ path: shot, fullPage: false })

console.log(`深链 ${url}`)
console.log(`  data-node-key=${nodes}  .vue-flow__node=${vfNodes}  edges=${edges}  只读横幅=${readonly}`)
console.log(`  截图 ${shot}`)
await browser.close()
process.exit(nodes > 0 && readonly ? 0 : 1)
