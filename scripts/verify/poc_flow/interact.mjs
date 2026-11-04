/**
 * POC-FLOW 交互验证：三个 demo × Chromium/WebKit
 *   ① 拖节点（应只动 1 个节点，位移 ≈ 鼠标位移）
 *   ② 滚轮缩放（zoom 应变化）
 *   ③ 空白处拖动平移（所有节点同向移动）
 *   ④ 自研 demo 的「旧写法 <g transform>」在 WebKit 下的塌陷复现
 *
 * 用法：node scripts/verify/poc_flow/interact.mjs [baseUrl]
 * 输出：docs/evidence/20261104-poc-flow/shots-local/interact.json + *-interact.png / *-legacy.png
 */
import fs from 'fs'

const PW = process.env.PLAYWRIGHT_MODULE || 'playwright-core'
const { chromium, webkit } = await import(PW)

const BASE = process.argv[2] || 'http://127.0.0.1:8099/demo/flow-poc'
const OUT = 'docs/evidence/20261104-poc-flow/shots-local'
fs.mkdirSync(OUT, { recursive: true })

const PAGES = [
  ['native', 'demo-native.html'],
  ['vue-flow', 'demo-vue-flow.html'],
  ['logicflow', 'demo-logicflow.html'],
]

async function blankPoint(page) {
  for (const [x, y] of [[980, 640], [60, 660], [1180, 640], [980, 200], [1000, 620]]) {
    const tag = await page.evaluate(([px, py]) => {
      const el = document.elementFromPoint(px, py)
      if (!el) return 'none'
      if (el.closest('.flow-node') || el.closest('.lf-node') || el.closest('.poc-note') || el.closest('.vue-flow__controls')) return 'hit'
      return el.tagName
    }, [x, y])
    if (tag !== 'hit') return [x, y]
  }
  return [40, 40]
}

const out = {}
for (const [ename, engine] of [['chromium', chromium], ['webkit', webkit]]) {
  const browser = await engine.launch()
  out[ename] = {}
  for (const [name, file] of PAGES) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 760 } })
    const errors = []
    page.on('pageerror', (e) => errors.push(String(e).split('\n')[0]))
    await page.goto(`${BASE}/${file}`, { waitUntil: 'load' })
    await page.waitForTimeout(1200)

    const nodes0 = await page.evaluate(() => window.__poc.nodes())
    const zoom0 = await page.evaluate(() => window.__poc.zoom())

    const n0 = nodes0[0]
    const cx = n0.x + n0.w / 2
    const cy = n0.y + n0.h / 2
    await page.mouse.move(cx, cy)
    await page.mouse.down()
    await page.mouse.move(cx + 100, cy + 40, { steps: 10 })
    await page.mouse.up()
    await page.waitForTimeout(300)
    const nodesDrag = await page.evaluate(() => window.__poc.nodes())
    const moved = nodesDrag.filter((n) => {
      const b = nodes0.find((m) => m.id === n.id)
      return b && (Math.abs(n.x - b.x) > 4 || Math.abs(n.y - b.y) > 4)
    })
    const maxMove = Math.max(0, ...moved.map((n) => {
      const b = nodes0.find((m) => m.id === n.id)
      return Math.hypot(n.x - b.x, n.y - b.y)
    }))

    await page.evaluate(() => window.__poc.fit())
    await page.waitForTimeout(250)
    const [bx, by] = await blankPoint(page)
    await page.mouse.move(bx, by)
    await page.mouse.wheel(0, -400)
    await page.waitForTimeout(350)
    const zoom1 = await page.evaluate(() => window.__poc.zoom())

    await page.evaluate(() => window.__poc.fit())
    await page.waitForTimeout(250)
    const panStart = await page.evaluate(() => window.__poc.nodes())
    const [px, py] = await blankPoint(page)
    await page.mouse.move(px, py)
    await page.mouse.down()
    await page.mouse.move(px - 90, py, { steps: 8 })
    await page.mouse.up()
    await page.waitForTimeout(300)
    const panEnd = await page.evaluate(() => window.__poc.nodes())

    out[ename][name] = {
      drag: { nodesMoved: moved.length, maxMove: Math.round(maxMove) },
      zoom: { before: +zoom0.toFixed(2), after: +zoom1.toFixed(2) },
      pan: Math.round(panEnd[0].x - panStart[0].x),
      errors,
    }
    await page.evaluate(() => window.__poc.fit())
    await page.waitForTimeout(250)
    await page.screenshot({ path: `${OUT}/${ename}-${name}-interact.png` })
    await page.close()
  }
  await browser.close()
}

for (const [ename, engine] of [['chromium', chromium], ['webkit', webkit]]) {
  const browser = await engine.launch()
  const page = await browser.newPage({ viewport: { width: 1280, height: 760 } })
  await page.goto(`${BASE}/demo-native.html`, { waitUntil: 'load' })
  await page.waitForTimeout(1000)
  await page.evaluate(() => window.__poc.setLegacy(true))
  await page.waitForTimeout(600)
  const rects = await page.evaluate(() => window.__poc.nodes())
  await page.screenshot({ path: `${OUT}/${ename}-native-legacy.png` })
  out[ename].legacy = {
    layoutRects: rects.map((r) => [r.x, r.y]),
    distinct: new Set(rects.map((r) => `${r.x},${r.y}`)).size,
  }
  await page.close()
  await browser.close()
}

fs.writeFileSync(`${OUT}/interact.json`, JSON.stringify(out, null, 2))
for (const [eng, v] of Object.entries(out)) {
  for (const [k, r] of Object.entries(v)) {
    if (k === 'legacy') {
      console.log(`${eng.padEnd(9)} legacy      layoutRects distinct=${r.distinct}（布局仍正确 = 经典「布局对/绘制错」）`)
      continue
    }
    console.log(`${eng.padEnd(9)} ${k.padEnd(10)} drag: ${r.drag.nodesMoved} 个节点 / ${r.drag.maxMove}px  ` +
      `zoom ${r.zoom.before}->${r.zoom.after}  pan=${r.pan}px  报错=${r.errors.length}`)
  }
}
