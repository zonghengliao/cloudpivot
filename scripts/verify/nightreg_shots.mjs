#!/usr/bin/env node
/**
 * NIGHT-REGRESSION 真机截图：5 条业务链路各自的页面证据（链路⑥ 的流程编辑器由
 * `vueflow3_full_regression.cjs` 出图，见 docs/evidence/20261104-nightreg/shots/flow-editor/）。
 *
 * 用法（前端 dev :5173 代理 /api → 隔离实例 :8023）：
 *   node scripts/verify/nightreg_shots.mjs [baseUrl] [outDir]
 *   默认 baseUrl=http://127.0.0.1:5173，outDir=docs/evidence/20261104-nightreg/shots
 *
 * 口令取 NIK_ADMIN_PASSWORD / NR_ADMIN_PASS（默认 Admin@123456）。
 * 退出码：0 = 全部截图成功；1 = 有页面没出来。
 */
import fs from 'node:fs'
import path from 'node:path'

const PW = process.env.PLAYWRIGHT_MODULE || 'playwright-core'
const { chromium } = await import(PW)

const BASE = process.argv[2] || process.env.NR_UI_BASE || 'http://127.0.0.1:5173'
const OUT = process.argv[3] || 'docs/evidence/20261104-nightreg/shots'
const USER = process.env.NR_ADMIN_USER || 'admin'
const PASS = process.env.NR_ADMIN_PASS || process.env.NIK_ADMIN_PASSWORD || 'Admin@123456'

fs.mkdirSync(OUT, { recursive: true })

const pages = [
  ['chain1-01-contract', '/business/erp/contract', '合同列表（链路①：合同已签订）'],
  ['chain1-02-purchase-order', '/business/erp/purchase-order', '采购单列表（链路①：合同采购单已审批/已收货）'],
  ['chain1-03-stock-in', '/business/erp/stock-in', '收货单列表（链路①：已过账 / 链路②：已作废）'],
  ['chain1-04-payable', '/business/erp/payable', '应付单列表（链路①：已结清）'],
  ['chain3-01-stock-out', '/business/erp/stock-out', '出库单列表（链路③：领用已过账 + 成本结转凭证）'],
  ['chain4-01-transfer', '/business/erp/transfer', '调拨单列表（链路④：在途 / 已完成）'],
  ['chain5-01-todo', '/flow/task', '审批中心待办（链路⑤：金额分级 + 超信用额度分支）'],
  ['chain6-00-flow-list', '/flow/def', '流程定义列表（链路⑥：进入编辑器的入口）'],
]

const report = []
let failed = 0

const resp = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PASS }),
})
if (!resp.ok) {
  console.error(`登录失败：${resp.status} ${await resp.text()}`)
  process.exit(1)
}
const login = await resp.json()
const access = login?.data?.access_token
const refresh = login?.data?.refresh_token
if (!access) {
  console.error(`登录响应没有 access_token：${JSON.stringify(login).slice(0, 300)}`)
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
  [access, refresh],
)
const page = await ctx.newPage()

for (const [name, route, desc] of pages) {
  try {
    await page.goto(BASE + route, { waitUntil: 'domcontentloaded', timeout: 60000 })
    // 等表格/骨架屏落地：出现 t-table 或 空态文案（SSE 长连接会让 networkidle 永不触发）
    await page.waitForSelector('.t-table, .t-table__empty, .t-empty', { timeout: 30000 }).catch(() => {})
    await page.waitForTimeout(2500)
    // 关掉「你有 N 条待处理」/ 引导弹窗，否则会盖住表格
    for (const text of ['知道了', '我知道了', '跳过', '关闭']) {
      const btn = page.locator(`.t-dialog button:has-text("${text}")`).first()
      if (await btn.count()) {
        await btn.click({ timeout: 3000 }).catch(() => {})
        await page.waitForTimeout(600)
      }
    }
    await page.keyboard.press('Escape').catch(() => {})
    await page.waitForTimeout(600)
    const file = path.join(OUT, `${name}.png`)
    await page.screenshot({ path: file, fullPage: false })
    const rows = await page.locator('.t-table tbody tr').count()
    report.push({ name, route, desc, file, rows, ok: true })
    console.log(`  截图 ${file}  （${route}，表格行 ${rows}）`)
  } catch (err) {
    failed += 1
    report.push({ name, route, desc, ok: false, error: String(err).slice(0, 300) })
    console.log(`  [FAIL] ${route}：${String(err).slice(0, 200)}`)
  }
}

await browser.close()
fs.writeFileSync(path.join(OUT, 'report.json'), JSON.stringify(report, null, 2))
console.log(`\n汇总：${report.length - failed}/${report.length} 张截图成功 → ${OUT}`)
process.exit(failed === 0 ? 0 : 1)
