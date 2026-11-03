// D3 前端按钮联动截图（临时脚本，用完即删）：注入 token → 出库单详情抽屉 → 截「动作按钮」区域。
import { chromium } from './node_modules/.pnpm/playwright@1.64.1/node_modules/playwright/index.mjs';
import { readFileSync } from 'node:fs';

const BASE = process.env.FE_BASE || 'http://127.0.0.1:5180';
const OUT = process.env.SHOT || '/root/d3-shot.png';
const tokens = JSON.parse(readFileSync('/root/d3_token.json', 'utf8'));

const browser = await chromium.launch({ args: ['--no-sandbox'] });
const page = await browser.newPage({ viewport: { width: 1440, height: 960 } });
page.on('console', (msg) => {
  if (msg.type() === 'error') console.log('[console.error]', msg.text().slice(0, 160));
});
page.on('response', (res) => {
  if (res.status() >= 400) console.log('[http]', res.status(), res.url().slice(0, 170));
});

await page.goto(BASE, { waitUntil: 'domcontentloaded' });
await page.evaluate(
  ([access, refresh]) => {
    localStorage.setItem('x_access_token', access);
    localStorage.setItem('x_refresh_token', refresh);
  },
  [tokens.access_token, tokens.refresh_token],
);

await page.goto(`${BASE}/business/erp/stock-out`, { waitUntil: 'networkidle' });
await page.waitForTimeout(4000);
console.log('url:', page.url(), '| 查看数量 =', await page.getByText('查看', { exact: true }).count());
console.log('---- 页面文本 ----');
console.log((await page.locator('body').innerText()).replace(/\n{2,}/g, '\n').slice(0, 1200));

// 关掉「新功能引导」浮层（有就点，没有就跳过）
const know = page.getByRole('button', { name: '知道了' });
if (await know.count()) {
  await know.first().click();
  await page.waitForTimeout(400);
}

await page.getByText('查看', { exact: true }).first().click();
await page.waitForTimeout(2000);

const drawer = page.locator('.t-drawer--open').last();
const text = await drawer.innerText().catch(() => '');
console.log('---- 抽屉文本（前 2000 字）----');
console.log(text.slice(0, 2000));
console.log('---- 点击后整页文本（前 2000 字）----');
console.log((await page.locator('body').innerText()).replace(/\n{2,}/g, '\n').slice(0, 2000));
console.log(
  'drawer 候选：',
  await page.evaluate(() =>
    Array.from(document.querySelectorAll('[class*="drawer"],[class*="detail"],[class*="dialog"]'))
      .slice(0, 10)
      .map((el) => `${el.tagName}.${el.className}`.slice(0, 90)),
  ),
);

const box = await drawer.boundingBox();
if (box) {
  await page.screenshot({
    path: OUT,
    clip: {
      x: Math.max(box.x - 8, 0),
      y: Math.max(box.y - 8, 0),
      width: Math.min(box.width + 16, 1440),
      height: Math.min(box.height + 16, 960),
    },
  });
} else {
  await page.screenshot({ path: OUT, fullPage: true });
}
console.log('screenshot ->', OUT);
await browser.close();
