/**
 * P2-4 库位做完整的页面证据（真页面 · 真接口 · 截图 ≥3 张）
 *
 * 用法：
 *   BACKEND_ORIGIN=http://127.0.0.1:18096 pnpm dev      # 前端 dev :5173，代理到隔离实例
 *   node scripts/verify/p24_shots.mjs [baseUrl] [tag]
 *   默认 baseUrl = http://127.0.0.1:5173，tag = local
 *   截图输出到 docs/evidence/20261104-p24-location/shots-<tag>/
 *
 * 抓的四张（卡面要求 ≥3）：
 *   1) 收货表单带库位列（含默认库位自动带出 + 批量设库位）
 *   2) 收货单详情：明细的库位列 + 库位提示告警条
 *   3) 库位管理：默认库位设置（is_default）
 *   4) 库位账明细（库位库存 Tab：A/B/默认/未分配 各桶数量）
 *
 * 依赖 playwright-core（项目 pnpm 装的那份），可用 PLAYWRIGHT_MODULE 覆盖。
 */
import fs from 'fs'

const PW = process.env.PLAYWRIGHT_MODULE || 'playwright-core'
const { chromium } = await import(PW)

const BASE = process.argv[2] || 'http://127.0.0.1:5173'
const TAG = process.argv[3] || 'local'
const OUT = `docs/evidence/20261104-p24-location/shots-${TAG}`
fs.mkdirSync(OUT, { recursive: true })

const USERNAME = process.env.P24_USER || 'admin'
const PASSWORDS = [process.env.P24_PASS || 'Admin@1234567', 'Admin@123456']
const WAREHOUSE_HINT = process.env.P24_WAREHOUSE || 'P24E2E'

const report = {}
const problems = []

async function shot(page, name) {
  const file = `${OUT}/${name}.png`
  await page.screenshot({ path: file, fullPage: false })
  report[name] = file
  console.log(`  截图 ${file}`)
}

async function login(page) {
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1200)
  for (const password of PASSWORDS) {
    const inputs = page.locator('input')
    await inputs.nth(0).fill(USERNAME)
    await inputs.nth(1).fill(password)
    await page.locator('button[type="submit"], button:has-text("登 录"), button:has-text("登录")').first().click()
    try {
      await page.waitForURL((url) => !url.pathname.startsWith('/login'), { timeout: 8000 })
      // 登录后会弹「未读通知」对话框，挡住整页点击 —— 先关掉
      await page.waitForTimeout(1200)
      const close = page.locator('[data-testid="login-unread-dialog"] button').first()
      if (await close.count()) {
        await close.click({ force: true }).catch(() => {})
      }
      await page.keyboard.press('Escape').catch(() => {})
      await page.waitForTimeout(500)
      console.log('  登录成功')
      return true
    } catch {
      await page.waitForTimeout(800)
    }
  }
  throw new Error('登录失败：两个默认口令都不通')
}

async function main() {
  const browser = await chromium.launch()
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 })
  const consoleErrors = []
  const failedRequests = []
  page.on('console', (msg) => {
    // 「Failed to load resource: 401」是站内通知 SSE（/api/business/notice/stream）在隔离实例上的
    // 固定噪声（没有 Redis 推送），按 URL 分类到 failedRequests，不算页面错误。
    if (msg.type() === 'error' && !msg.text().includes('Failed to load resource')) {
      consoleErrors.push(msg.text())
    }
  })
  page.on('pageerror', (err) => consoleErrors.push(String(err)))
  page.on('response', (resp) => {
    if (resp.status() >= 400 && !resp.url().includes('notice/stream')) {
      failedRequests.push(`${resp.status()} ${resp.url()}`)
    }
  })

  await login(page)

  // ---------- ① 收货表单：库位列 + 默认库位带出 + 批量设库位 ----------
  await page.goto(`${BASE}/business/erp/stockin`, { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1500)
  await page.locator('[data-testid="btn-stockin-create"], button:has-text("新建")').first().click()
  await page.waitForTimeout(900)

  // 选仓库（有库位数据的那一个）→ 触发库位下拉加载 + 默认库位带出
  const warehouse = page.locator('[data-testid="stockin-warehouse"]')
  await warehouse.click()
  await page.waitForTimeout(500)
  const option = page.locator(`.t-select-option:has-text("${WAREHOUSE_HINT}")`).first()
  if (await option.count()) {
    await option.click()
  } else {
    await page.locator('.t-select-option').first().click()
  }
  await page.waitForTimeout(1500)

  // 断言：库位下拉有选项、第一行带出了默认库位
  const locationCell = page.locator('[data-testid="stockin-item-location-0"]')
  const locationText = (await locationCell.innerText().catch(() => '')).trim()
  const locationInput = await locationCell.locator('input').first().inputValue().catch(() => '')
  report.form_location_text = locationText || locationInput
  report.batch_control = await page.locator('[data-testid="stockin-batch-location"]').count()
  if (!report.batch_control) {
    problems.push('收货表单没有「批量设库位」控件')
  }
  if (!report.form_location_text) {
    problems.push('收货表单第一行没有带出默认库位（库位列是空的）')
  }
  await shot(page, '01-stockin-form-location')

  // ---------- ② 收货单详情：库位列 + 库位提示 ----------
  await page.keyboard.press('Escape')
  await page.waitForTimeout(400)
  await page.goto(`${BASE}/business/erp/stockin`, { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1500)
  const viewLinks = page.locator('.t-link:has-text("查看")')
  const viewCount = await viewLinks.count()
  let shotDetail = false
  let shotWarn = false
  for (let i = 0; i < Math.min(viewCount, 6) && !(shotDetail && shotWarn); i += 1) {
    await viewLinks.nth(i).click()
    await page.waitForTimeout(1500)
    const warn = page.locator('[data-testid="stockin-detail-location-warnings"]')
    const hasWarn = (await warn.count()) > 0
    if (!shotDetail) {
      await shot(page, '02-stockin-detail-location')
      report.detail_location_cells = await page.locator('[data-testid^="stockin-detail-location-"]').count()
      shotDetail = true
    }
    if (hasWarn && !shotWarn) {
      report.detail_warnings = (await warn.innerText()).slice(0, 240)
      await shot(page, '02b-stockin-detail-unassigned-warning')
      shotWarn = true
    }
    // 关抽屉（右上角关闭按钮）
    const closeBtn = page.locator('.t-drawer__close-btn, .t-drawer button:has(.t-icon-close)').first()
    if (await closeBtn.count()) {
      await closeBtn.click().catch(() => {})
    } else {
      await page.keyboard.press('Escape')
    }
    await page.waitForTimeout(700)
  }
  if (!shotDetail) {
    problems.push('收货单列表没有详情入口，第 2 张截图跳过')
  }
  if (!shotWarn) {
    problems.push('没找到带「未分配」提示的收货单（02b 截图缺失）')
  }

  // ---------- ③ 库位管理：默认库位 ----------
  await page.goto(`${BASE}/business/erp/location`, { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1800)
  await shot(page, '03-location-default')
  report.location_rows = await page.locator('.t-table tbody tr').count()

  // ---------- ④ 库位账明细（库位库存 Tab） ----------
  await page.goto(`${BASE}/business/erp/inventory`, { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1600)
  const tab = page.locator('.t-tabs__nav-item:has-text("库位库存"), button:has-text("库位库存")').first()
  if (await tab.count()) {
    await tab.click()
    await page.waitForTimeout(1400)
  } else {
    problems.push('库存页没找到「库位库存」Tab')
  }
  await shot(page, '04-inventory-location-ledger')
  report.ledger_rows = await page.locator('.t-table tbody tr').count()

  report.console_errors = consoleErrors
  report.failed_requests = failedRequests
  await browser.close()

  report.problems = problems
  fs.writeFileSync(`${OUT}/report.json`, JSON.stringify(report, null, 2))
  console.log('\n---- 结果 ----')
  console.log(JSON.stringify(report, null, 2))
  if (consoleErrors.length) {
    console.log(`⚠️ 控制台错误 ${consoleErrors.length} 条（已过滤 notice/stream）`)
  }
  if (problems.length) {
    console.log(`❌ 页面问题 ${problems.length} 条`)
    process.exitCode = 1
  }
}

await main()
