#!/usr/bin/env node
/**
 * 复核：流程列表里「编辑」这一行内链接到底能不能点（vueflow 回归的 B2 用 click() 30s 超时）。
 * 判据不看 click()，只看几何：可见性 / boundingBox / elementFromPoint 命中的是不是它本身。
 */
import fs from 'node:fs'

const PW = process.env.PLAYWRIGHT_MODULE || 'playwright-core'
const { chromium } = await import(PW)
const BASE = process.argv[2] || 'http://127.0.0.1:5273'
const PASS = process.env.NR_ADMIN_PASS || 'Admin@123456'

const resp = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: PASS }),
})
const login = await resp.json()
const access = login?.data?.access_token
const browser = await chromium.launch()
const ctx = await browser.newContext({ viewport: { width: 1600, height: 950 } })
await ctx.addInitScript((a) => {
  localStorage.setItem('x_access_token', a)
  localStorage.setItem('cp-onboarding-done', '1')
}, access)
const page = await ctx.newPage()
await page.goto(`${BASE}/flow/def`, { waitUntil: 'domcontentloaded' })
await page.waitForSelector('.t-table tbody tr', { timeout: 30000 })
await page.waitForTimeout(2000)
await page.keyboard.press('Escape').catch(() => {})

const report = await page.evaluate(() => {
  const out = []
  const rows = Array.from(document.querySelectorAll('.t-table tbody tr')).slice(0, 4)
  rows.forEach((tr, i) => {
    const links = Array.from(tr.querySelectorAll('a, .t-link, button')).map((el) => {
      const r = el.getBoundingClientRect()
      const cx = r.left + r.width / 2
      const cy = r.top + r.height / 2
      const hit = document.elementFromPoint(cx, cy)
      return {
        text: (el.innerText || '').trim().slice(0, 12),
        tag: el.tagName,
        w: Math.round(r.width),
        h: Math.round(r.height),
        hitIsSelf: !!hit && (el === hit || el.contains(hit)),
        hitTag: hit ? hit.tagName + '.' + (hit.className || '').toString().slice(0, 40) : null,
      }
    })
    out.push({ row: i, name: (tr.innerText || '').split('\n')[0].slice(0, 24), links })
  })
  return out
})
console.log(JSON.stringify(report, null, 1))
await browser.close()
