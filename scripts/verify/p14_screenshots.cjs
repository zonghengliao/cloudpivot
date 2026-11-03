/**
 * P1-4 单据附件：前端截图（真实浏览器，dev server 5174 → 隔离实例 18098）。
 *
 *   cd frontend && node ../scripts/verify/p14_screenshots.cjs
 *
 * 产物写进 `docs/evidence/20261103-p14-doc-attachment/`：
 *   01-contract-attachments.png   合同详情：附件区（2 条，角色不同）
 *   02-contract-upload.png        合同详情：用界面上传第 3 个文件后（拖拽/选择同一个 input）
 *   03-purchase-attachments.png   采购单详情
 *   04-stockin-attachments.png    收货单详情
 *   05-payable-attachments.png    应付单（付款）详情
 *   ui-drawers.txt                四个抽屉的附件区文本（纯文本证据，便于 diff）
 *
 * 依赖：playwright（frontend/node_modules）。登录口令取 P14_PASS，默认是隔离实例
 * 首登改密后的值（P14@Passw0rd!）。
 */
const path = require('path');
const fs0 = require('fs');

// playwright 装在前端的 workspace 里（`@playwright/test` 自带 chromium 驱动），
// 本脚本在 scripts/verify/ 下运行时 CJS 只会从脚本目录向上找，所以显式解析。
const PW_CANDIDATES = [
  process.env.P14_PLAYWRIGHT,
  path.resolve(__dirname, '../../frontend/node_modules/@playwright/test'),
  path.resolve(__dirname, '../../frontend/node_modules/playwright'),
].filter(Boolean);
const PW_PATH = PW_CANDIDATES.find((candidate) => fs0.existsSync(candidate));
if (!PW_PATH) {
  console.error('找不到 playwright（试过：' + PW_CANDIDATES.join(', ') + '）');
  process.exit(2);
}
const { chromium } = require(PW_PATH);
const fs = require('fs');

const BASE = process.env.P14_BASE || 'http://127.0.0.1:5174';
const PASS = process.env.P14_PASS || 'P14@Passw0rd!';
const OUT = process.env.P14_EVIDENCE || '/opt/cloudpivot/docs/evidence/20261103-p14-doc-attachment';
// ⚠ /tmp 在这台机器上是 7.6G tmpfs 且经常被并行卡的 target 塞满（实测 ENOSPC），
//   所以样例文件写到 /root 下的临时目录。
const UPLOAD_FILE =
  process.env.P14_UPLOAD_FILE || '/root/p14-ui-upload.png';

const DOCS = [
  {
    name: 'contract',
    url: '/business/erp/contract',
    openTestId: 'btn-contract-view-P14-CT-0001',
    drawer: '[data-testid="contract-detail"]',
    attach: '[data-testid="contract-attach-item"]',
    drop: '[data-testid="contract-attach-drop"]',
    expect: 2,
    shot: '01-contract-attachments.png',
  },
  {
    name: 'purchase',
    url: '/business/erp/purchase-order',
    openTestId: 'btn-view-P14-PO-0001',
    drawer: '[data-testid="purchase-detail-drawer"]',
    attach: '[data-testid="purchase-attach-item"]',
    drop: '[data-testid="purchase-attach-drop"]',
    expect: 1,
    shot: '03-purchase-attachments.png',
  },
  {
    name: 'stockin',
    url: '/business/erp/stock-in',
    openTestId: 'btn-view-P14-IN-0001',
    drawer: '[data-testid="stockin-detail-drawer"]',
    attach: '[data-testid="stockin-attach-item"]',
    drop: '[data-testid="stockin-attach-drop"]',
    expect: 1,
    shot: '04-stockin-attachments.png',
  },
  {
    name: 'payable',
    url: '/business/erp/payable',
    openTestId: 'btn-payable-view-P14-AP-0001',
    drawer: '[data-testid="payable-detail"]',
    attach: '[data-testid="payable-attach-item"]',
    drop: '[data-testid="payable-attach-drop"]',
    expect: 1,
    shot: '05-payable-attachments.png',
  },
];

const findings = [];

function note(line) {
  findings.push(line);
  console.log(line);
}

async function login(page) {
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
  // 首登引导（`cp-onboarding-done`）会盖住列表行的「查看」链接 —— 提前打标记跳过，
  // 否则 Playwright 的点击会被 `.onboarding__actions` 拦截（实测 30s 超时）。
  await page.addInitScript(() => {
    try {
      window.localStorage.setItem('cp-onboarding-done', '1');
    } catch (error) {
      /* 隐私模式下写不进去也无所谓：加载后还会再过一次 dismissOnboarding */
    }
  });
  await page.reload({ waitUntil: 'domcontentloaded' });
  await page.waitForSelector("input[placeholder='请输入用户名']", { timeout: 30_000 });
  await page.fill("input[placeholder='请输入用户名']", 'admin');
  await page.fill("input[placeholder='请输入密码']", PASS);
  await page.click('button.t-button--theme-primary');
  await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 30_000 });
  note(`登录成功 → ${page.url()}`);
}

(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  // 上传用的样例图（与 e2e 里同一张 1x1 PNG + 界面标记）
  const png = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
    'base64',
  );
  fs.writeFileSync(UPLOAD_FILE, Buffer.concat([png, Buffer.from('\n#p14-ui-upload')]));

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1600, height: 1100 } });
  const page = await context.newPage();
  page.on('console', (message) => {
    if (message.type() === 'error') {
      note(`[console.error] ${message.text()}`);
    }
  });
  let failed = 0;

  try {
    await login(page);

    for (const doc of DOCS) {
      note(`\n=== ${doc.name} → ${doc.url} ===`);
      await page.goto(`${BASE}${doc.url}`, { waitUntil: 'domcontentloaded' });
      const openBtn = page.locator(`[data-testid="${doc.openTestId}"]`).first();
      try {
        await openBtn.waitFor({ state: 'visible', timeout: 30_000 });
      } catch (error) {
        note(`[FAIL] ${doc.name}: 列表里找不到 ${doc.openTestId}`);
        failed += 1;
        continue;
      }
      await openBtn.click();
      const drawer = page.locator(doc.drawer).first();
      await drawer.waitFor({ state: 'visible', timeout: 20_000 });
      // 等附件区加载出条目（后端 200 + list 非空）
      try {
        await page.locator(doc.attach).first().waitFor({ state: 'visible', timeout: 20_000 });
      } catch (error) {
        note(`[FAIL] ${doc.name}: 附件区没有渲染出条目`);
        failed += 1;
        continue;
      }
      const count = await page.locator(doc.attach).count();
      const ok = count === doc.expect;
      note(`附件条目 ${count} 条（期望 ${doc.expect}）${ok ? ' ✓' : ' ✗'}`);
      if (!ok) {
        failed += 1;
      }
      // 附件区文本（纯文本证据）
      const panel = page.locator(`${doc.drawer} .doc-attach`).first();
      note(await panel.innerText());
      await page.locator(doc.drawer).first().scrollIntoViewIfNeeded();
      await page.screenshot({ path: path.join(OUT, doc.shot) });
      note(`截图 → ${doc.shot}`);

      // 合同页额外做一次「界面上传」：直接给隐藏 input 塞文件（等价用户点选）
      if (doc.name === 'contract') {
        await page.setInputFiles(`${doc.drawer} .doc-attach__input`, UPLOAD_FILE);
        try {
          await page.waitForFunction(
            (selector) => document.querySelectorAll(selector).length === 3,
            doc.attach,
            { timeout: 30_000 },
          );
          note('界面上传成功：附件从 2 条变 3 条 ✓');
        } catch (error) {
          note('[FAIL] 界面上传后条目数没有变成 3');
          failed += 1;
        }
        const after = await page.locator(`${doc.drawer} .doc-attach`).first().innerText();
        note(after);
        await page.screenshot({ path: path.join(OUT, '02-contract-upload.png') });
        note('截图 → 02-contract-upload.png');
      }
      // 关抽屉，避免遮挡下一张
      await page.keyboard.press('Escape');
      await page.waitForTimeout(300);
    }
  } catch (error) {
    note(`[FATAL] ${error && error.message ? error.message : error}`);
    failed += 1;
  } finally {
    fs.writeFileSync(path.join(OUT, 'ui-drawers.txt'), findings.join('\n') + '\n');
    await browser.close();
  }

  console.log(`\n汇总：失败 ${failed} 项；文本证据 ${path.join(OUT, 'ui-drawers.txt')}`);
  process.exit(failed === 0 ? 0 : 1);
})();
