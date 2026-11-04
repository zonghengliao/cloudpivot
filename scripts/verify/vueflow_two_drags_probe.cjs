#!/usr/bin/env node
/**
 * 连续两次拉线：第二次的 `onConnectMove` 还会不会跑？
 * （WebKit 上「第一次能落点、第二次就不行了」的定位探针）
 */
'use strict';
const path = require('node:path');
const { loginToken } = require('./vueflow_token.cjs');

function loadPlaywright() {
  for (const candidate of ['@playwright/test', path.join(__dirname, '..', '..', 'frontend', 'node_modules', '@playwright/test'), '/opt/cloudpivot/frontend/node_modules/@playwright/test']) {
    try {
      return require(candidate);
    } catch {
      /* next */
    }
  }
  throw new Error('no playwright');
}

const BASE = 'http://127.0.0.1:5188';
const DEF = '20000000-0000-4000-8000-0000000a0182';
const BROWSER = process.argv[2] || 'webkit';
const OUT = process.argv[3] || '';

(async () => {
  const pw = loadPlaywright();
  const token = await loginToken(BASE);
  const browser = await pw[BROWSER].launch();
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await context.addInitScript((v) => {
    localStorage.setItem('x_access_token', v);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, token);
  const page = await context.newPage();
  await page.goto(`${BASE}/flow/def/${DEF}/edit?canvas=vueflow`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.vue-flow__node', { timeout: 30000 });
  await page.waitForTimeout(1500);

  const geo = await page.evaluate(() => {
    const outs = [...document.querySelectorAll('.flow-node__port--out')].map((el) => el.getBoundingClientRect());
    const cards = [...document.querySelectorAll('.flow-node__card')].map((el) => el.getBoundingClientRect());
    return {
      outs: outs.slice(0, 2).map((r) => ({ x: r.x + r.width / 2, y: r.y + r.height / 2 })),
      cards: cards.map((r) => ({ x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width })),
    };
  });

  const report = [];
  for (let i = 0; i < 2; i += 1) {
    // 手势之间把鼠标挪到空白处（真实用户一定会移动；WebKit 自动化下贴着上一次落点连续按下会丢事件）
    await page.mouse.move(60, 950);
    await page.waitForTimeout(150);
    const from = geo.outs[0];
    // 第 i 次拖到一个下方/右侧的卡片，避免重复边干扰（重复边也会显示高亮）
    const target = geo.cards.filter((c) => c.x > from.x + 60)[i + 1] ?? geo.cards.filter((c) => c.x > from.x + 60)[0];
    await page.evaluate(() => {
      window.__vfMove = { count: 0 };
      window.__vfEnd = null;
    });
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.waitForTimeout(80);
    await page.mouse.move(from.x + 6, from.y + 3, { steps: 3 });
    await page.mouse.move(target.x, target.y, { steps: 16 });
    await page.waitForTimeout(200);
    const mid = await page.evaluate(() => ({
      move: window.__vfMove,
      highlighted: document.querySelectorAll('.is-connect-target').length,
      connection: Boolean(document.querySelector('.vue-flow__connection')),
      wrap: document.querySelector('.flow-canvas').className,
    }));
    await page.mouse.up();
    await page.waitForTimeout(400);
    const end = await page.evaluate(() => ({
      end: window.__vfEnd,
      edges: document.querySelectorAll('.vue-flow__edge').length,
      connectionAfterUp: Boolean(document.querySelector('.vue-flow__connection')),
      wrap: document.querySelector('.flow-canvas').className,
    }));
    report.push({ i, target, mid, end });
  }
  console.log(BROWSER, JSON.stringify(report, null, 2));
  if (OUT) {
    require('node:fs').writeFileSync(OUT, JSON.stringify({ browser: BROWSER, at: new Date().toISOString(), report }, null, 2));
    console.log('证据：' + OUT);
  }
  await browser.close();
})();
