/**
 * POC-FLOW 冒烟：三个 demo × Chromium/WebKit，抓控制台报错、节点/边数量、节点屏幕坐标、截图
 *
 * 用法：
 *   node scripts/verify/poc_flow/shoot.mjs [baseUrl] [tag]
 *   默认 baseUrl = http://127.0.0.1:8099/demo/flow-poc，tag = local
 *   截图输出到 docs/evidence/20261104-poc-flow/shots-<tag>/
 *
 * 依赖 playwright-core：默认用项目里 pnpm 装的那份，可用 PLAYWRIGHT_MODULE 覆盖绝对路径。
 */
import fs from 'fs'
import path from 'path'

const PW = process.env.PLAYWRIGHT_MODULE || 'playwright-core'
const { chromium, webkit } = await import(PW)

const BASE = process.argv[2] || 'http://127.0.0.1:8099/demo/flow-poc'
const TAG = process.argv[3] || 'local'
const OUT = `docs/evidence/20261104-poc-flow/shots-${TAG}`
fs.mkdirSync(OUT, { recursive: true })

const PAGES = [
  ['native', 'demo-native.html'],
  ['vue-flow', 'demo-vue-flow.html'],
  ['logicflow', 'demo-logicflow.html'],
  ['index', 'index.html'],
]

const report = {}
for (const [ename, engine] of [['chromium', chromium], ['webkit', webkit]]) {
  let browser
  try {
    browser = await engine.launch()
  } catch (e) {
    report[ename] = { launchError: String(e).split('\n')[0] }
    continue
  }
  report[ename] = {}
  for (const [name, file] of PAGES) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 760 }, deviceScaleFactor: 1 })
    const errors = []
    page.on('console', (m) => {
      if (m.type() === 'error') errors.push(m.text().slice(0, 300))
    })
    page.on('pageerror', (e) => errors.push('pageerror: ' + String(e).split('\n')[0].slice(0, 300)))
    await page.goto(`${BASE}/${file}`, { waitUntil: 'load' })
    await page.waitForTimeout(1800)
    const info = await page.evaluate(() => {
      const p = window.__poc
      if (!p) return { noPoc: true }
      return {
        engine: p.engine,
        ready: p.ready,
        zoom: p.zoom(),
        edges: p.edges(),
        nodes: p.nodes ? p.nodes() : [],
        usesFO: p.usesForeignObject ? p.usesForeignObject() : null,
        cards: document.querySelectorAll('.node-card').length,
      }
    })
    await page.screenshot({ path: path.join(OUT, `${ename}-${name}.png`) })
    const spread = info.nodes && info.nodes.length
      ? { count: info.nodes.length, distinctPositions: new Set(info.nodes.map((n) => `${n.x},${n.y}`)).size }
      : null
    report[ename][name] = { url: `${BASE}/${file}`, errors, ...info, spread }
    await page.close()
  }
  await browser.close()
}

fs.writeFileSync(path.join(OUT, 'report.json'), JSON.stringify(report, null, 2))
for (const [eng, pages] of Object.entries(report)) {
  if (pages.launchError) { console.log(`[${eng}] LAUNCH FAILED: ${pages.launchError}`); continue }
  for (const [name, d] of Object.entries(pages)) {
    if (d.noPoc) { console.log(`[${eng}] ${name} no __poc, errors=${d.errors.length}`); continue }
    const sp = d.spread || {}
    console.log(`[${eng}] ${name.padEnd(10)} zoom=${d.zoom.toFixed(2)} edges=${d.edges} cards=${d.cards} ` +
      `pos=${sp.distinctPositions}/${sp.count} errors=${d.errors.length}`)
  }
}
