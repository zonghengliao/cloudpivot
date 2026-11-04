/**
 * P2-8-F1 真机验收：采购单「所属项目」全链路（真后端 127.0.0.1:18157 + vite dev 5183）
 *
 * 断言只认 DOM / 接口回读：
 *  ① 新建：表单里选「所属项目」→ 保存 → 列表「项目」列显示项目名 → 详情显示项目名；
 *  ② 归档项目在下拉里仍可选，且文案带「（已归档）」；
 *  ③ 编辑该单把项目清空 → 保存 → 列表/详情回到「—」，且 PUT 请求体里 project_id 是 null
 *     （证「整单覆盖」口径没把项目越改越乱）。
 */
import { chromium } from '@playwright/test';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';

const BASE = process.env.BASE ?? 'http://127.0.0.1:5183';
const TOKEN = readFileSync('/root/.hermes/profiles/frontend-developer/cache/scratch/token.txt', 'utf8').trim();
const REFRESH = readFileSync('/root/.hermes/profiles/frontend-developer/cache/scratch/refresh.txt', 'utf8').trim();
const OUT = process.env.OUT ?? '/root/.hermes/profiles/frontend-developer/cache/scratch/p28fe-out';
mkdirSync(OUT, { recursive: true });

const PROJECT_NAME = 'P28FE 采购挂项目';
const PROJECT_CODE = 'P28FE-PO-A';
const ARCHIVED_CODE = 'P28FE-PO-OLD';

/** 读 t-select 当前显示值：testid 落在 `.t-select__wrap` 上，真实文本在内部 input 的 value 里 */
async function selectValue(page, testid) {
  const input = page.locator(`[data-testid="${testid}"] input`).first();
  return (await input.inputValue()).trim();
}

const log = [];
function say(line) {
  log.push(line);
  console.log(line);
}

/** 下拉选项（teleport 到 body，取**可见**面板里的项；旧面板会假可见 ~200ms） */
async function pickOption(page, testid, optionText) {
  const wrap = page.locator(`[data-testid="${testid}"]`);
  await wrap.scrollIntoViewIfNeeded();
  const box = await wrap.boundingBox();
  // ★ el.click() 打不开 t-select，必须走真实鼠标坐标
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await page.waitForTimeout(150);
  const options = page.locator('.t-select__dropdown:visible .t-select-option');
  await options.first().waitFor({ state: 'visible', timeout: 10000 });
  const count = await options.count();
  const texts = [];
  for (let i = 0; i < count; i += 1) texts.push((await options.nth(i).innerText()).trim());
  const hit = texts.findIndex((t) => t.includes(optionText));
  if (hit < 0) throw new Error(`下拉 ${testid} 里找不到「${optionText}」，实际选项：${JSON.stringify(texts)}`);
  await options.nth(hit).click();
  await page.waitForTimeout(200);
  // 关掉面板，避免下一个下拉命中旧面板
  await page.mouse.click(5, 5);
  await page.waitForTimeout(150);
  return texts;
}

/** 打开下拉但只读选项、不选（用于验「归档项目也在列表里」） */
async function openOptions(page, testid) {
  const wrap = page.locator(`[data-testid="${testid}"]`);
  await wrap.scrollIntoViewIfNeeded();
  const box = await wrap.boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await page.waitForTimeout(150);
  const options = page.locator('.t-select__dropdown:visible .t-select-option');
  await options.first().waitFor({ state: 'visible', timeout: 10000 });
  const count = await options.count();
  const texts = [];
  for (let i = 0; i < count; i += 1) texts.push((await options.nth(i).innerText()).trim());
  return texts;
}

/** t-input-number：fill() 会跟受控组件打架，用键盘输入并复读 */
async function typeNumber(page, testid, value) {
  const input = page.locator(`[data-testid="${testid}"] input`).first();
  await input.click();
  await page.keyboard.press('Control+a');
  await page.keyboard.press('Backspace');
  await input.type(String(value), { delay: 20 });
  await page.keyboard.press('Tab');
  return input.inputValue();
}

async function closeDrawer(page) {
  const drawer = page.locator('.t-drawer');
  if ((await drawer.getAttribute('class'))?.includes('t-drawer--open')) {
    await page.locator('.t-drawer__mask').click({ position: { x: 5, y: 5 } });
  }
  await page.waitForFunction(
    () => !(document.querySelector('.t-drawer')?.getAttribute('class') ?? '').includes('t-drawer--open'),
    undefined,
    { timeout: 10000 },
  );
  await page.waitForTimeout(300);
}

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN' });
await context.addInitScript(
  ([token, refresh]) => {
    localStorage.setItem('x_access_token', token);
    localStorage.setItem('x_refresh_token', refresh);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [TOKEN, REFRESH],
);
const page = await context.newPage();
const posts = [];
page.on('request', (req) => {
  const url = req.url();
  if (!url.includes('/api/business/erp/purchase-order')) return;
  if (!['POST', 'PUT'].includes(req.method())) return;
  posts.push({ method: req.method(), url, body: req.postData() });
});
page.on('console', (msg) => {
  if (msg.type() === 'error') say(`[console.error] ${msg.text()}`);
});
page.on('pageerror', (err) => say(`[pageerror] ${err.message}`));

try {
  // ==================== ① 列表页就绪 ====================
  await page.goto(`${BASE}/business/erp/purchase`, { waitUntil: 'domcontentloaded' });
  await page.locator('[data-testid="purchase-table"]').waitFor({ state: 'visible', timeout: 30000 });
  say(`① 列表页就绪：${page.url()}`);

  // 表头要有「项目」列
  const headers = await page.locator('[data-testid="purchase-table"] thead th').allInnerTexts();
  say(`   表头：${JSON.stringify(headers.map((h) => h.trim()))}`);
  if (!headers.some((h) => h.trim() === '项目')) throw new Error('表头没有「项目」列');

  // ==================== ② 新建：选项目 → 保存 ====================
  await page.locator('[data-testid="btn-purchase-create"]').click();
  const dialog = page.locator('[data-testid="purchase-form-dialog"]');
  await dialog.waitFor({ state: 'visible', timeout: 10000 });
  await page.waitForTimeout(500);

  // 现货采购（合同采购要先选合同，与本卡无关）
  await page.locator('.t-radio', { hasText: '现货采购' }).first().click();
  await page.waitForTimeout(300);

  await pickOption(page, 'purchase-supplier', 'A4 二号供应商');
  await pickOption(page, 'purchase-warehouse', 'W1B 总仓');

  // ★ 项目下拉：先只读选项，确认归档项目没被筛掉、且带「（已归档）」后缀
  const projectTexts = await openOptions(page, 'purchase-project');
  say(`② 项目下拉选项：${JSON.stringify(projectTexts)}`);
  if (!projectTexts.some((t) => t.includes(PROJECT_NAME) && t.includes(PROJECT_CODE))) {
    throw new Error('项目下拉里没有 ' + PROJECT_NAME);
  }
  if (!projectTexts.some((t) => t.includes(ARCHIVED_CODE) && t.includes('（已归档）'))) {
    throw new Error('归档项目没出现在下拉里，或没带「（已归档）」后缀');
  }
  const placeholder = await page.locator('[data-testid="purchase-project"] input').first().getAttribute('placeholder');
  say(`   未选时提示文案：${JSON.stringify(placeholder)}`);
  // 选中目标项目（面板已经打开，直接点）
  {
    const options = page.locator('.t-select__dropdown:visible .t-select-option');
    const count = await options.count();
    for (let i = 0; i < count; i += 1) {
      if ((await options.nth(i).innerText()).includes(PROJECT_CODE)) {
        await options.nth(i).click();
        break;
      }
    }
    await page.mouse.click(5, 5);
    await page.waitForTimeout(200);
  }
  const selectedText = await selectValue(page, 'purchase-project');
  say(`   选中后选择器显示：${JSON.stringify(selectedText)}`);
  if (!selectedText.includes(PROJECT_CODE)) throw new Error('选择器没显示选中的项目');

  await pickOption(page, 'purchase-item-material-0', '原料A');
  const qty = await typeNumber(page, 'purchase-item-qty-0', 2);
  const price = await typeNumber(page, 'purchase-item-price-0', 100);
  say(`   明细：qty=${qty} price=${price}`);

  await page.screenshot({ path: `${OUT}/01-form-project-selected.png` });
  await page.locator('[data-testid="btn-purchase-save-draft"]').click();
  await page.waitForTimeout(2500);
  await page.waitForFunction(
    () => {
      const d = document.querySelector('[data-testid="purchase-form-dialog"]');
      return !d || d.offsetParent === null;
    },
    undefined,
    { timeout: 20000 },
  );

  const createReq = posts.find((p) => p.method === 'POST');
  if (!createReq) throw new Error('没有捕获到建单 POST 请求');
  const createBody = JSON.parse(createReq.body ?? '{}');
  say(`③ 建单请求体 project_id = ${JSON.stringify(createBody.project_id)}`);
  if (typeof createBody.project_id !== 'string' || createBody.project_id.length < 32) {
    throw new Error('建单请求体里的 project_id 不是 uuid：' + JSON.stringify(createBody.project_id));
  }
  const projectId = createBody.project_id;

  // ==================== ③ 列表「项目」列 ====================
  await page.waitForTimeout(1200);
  const orderNo = await page.evaluate(() => {
    const cell = document.querySelector('[data-testid^="purchase-no-"]');
    return cell?.getAttribute('data-testid')?.replace('purchase-no-', '') ?? '';
  });
  say(`④ 新单单号：${orderNo}`);
  const listCell = page.locator(`[data-testid="purchase-project-${orderNo}"]`);
  await listCell.waitFor({ state: 'visible', timeout: 15000 });
  const listText = (await listCell.innerText()).trim();
  say(`   列表「项目」列文本 = ${JSON.stringify(listText)}`);
  if (listText !== PROJECT_NAME) throw new Error(`列表项目列应为「${PROJECT_NAME}」，实际「${listText}」`);
  await page.screenshot({ path: `${OUT}/02-list-project-column.png` });

  // ==================== ④ 详情抽屉 ====================
  await page.locator(`[data-testid="btn-view-${orderNo}"]`).click();
  const detailCell = page.locator('[data-testid="purchase-detail-project"]');
  await detailCell.waitFor({ state: 'visible', timeout: 15000 });
  await page.waitForTimeout(800);
  const detailText = (await detailCell.innerText()).trim();
  say(`⑤ 详情「所属项目」= ${JSON.stringify(detailText)}`);
  if (detailText !== PROJECT_NAME) throw new Error(`详情项目应为「${PROJECT_NAME}」，实际「${detailText}」`);
  await page.screenshot({ path: `${OUT}/03-detail-project.png` });
  await closeDrawer(page);

  // ==================== ⑤ 编辑：清空项目 → 保存 ====================
  await page.locator(`[data-testid="btn-edit-${orderNo}"]`).click();
  await dialog.waitFor({ state: 'visible', timeout: 10000 });
  await page.waitForTimeout(1200);
  const editShown = await selectValue(page, 'purchase-project');
  say(`⑥ 编辑回填：选择器显示 ${JSON.stringify(editShown)}`);
  if (!editShown.includes(PROJECT_CODE)) throw new Error('编辑回填时项目没显示出来（会静默清空归属）');

  // 用 clearable 的清除按钮清空
  const wrap = page.locator('[data-testid="purchase-project"]');
  await wrap.hover();
  await page.waitForTimeout(300);
  const clearIcon = wrap.locator('.t-icon-close, .t-icon-close-circle-filled').first();
  if (await clearIcon.count()) {
    await clearIcon.click({ force: true });
  } else {
    const box = await wrap.boundingBox();
    await page.mouse.click(box.x + box.width - 18, box.y + box.height / 2);
  }
  await page.waitForTimeout(500);
  const afterClear = await selectValue(page, 'purchase-project');
  say(`   清空后选择器显示 ${JSON.stringify(afterClear)}`);

  posts.length = 0;
  await page.locator('[data-testid="btn-purchase-save-draft"]').click();
  await page.waitForTimeout(2500);
  await page.waitForFunction(
    () => {
      const d = document.querySelector('[data-testid="purchase-form-dialog"]');
      return !d || d.offsetParent === null;
    },
    undefined,
    { timeout: 20000 },
  );
  const putReq = posts.find((p) => p.method === 'PUT');
  if (!putReq) throw new Error('没有捕获到改单 PUT 请求');
  const putBody = JSON.parse(putReq.body ?? '{}');
  say(`⑦ 改单请求体：project_id = ${JSON.stringify(putBody.project_id)}，键存在=${Object.prototype.hasOwnProperty.call(putBody, 'project_id')}`);
  if (!Object.prototype.hasOwnProperty.call(putBody, 'project_id')) throw new Error('改单请求体里根本没有 project_id 键');
  if (putBody.project_id !== null) throw new Error('清空项目后应当发 project_id: null，实际 ' + JSON.stringify(putBody.project_id));

  await page.waitForTimeout(1500);
  const clearedList = (await page.locator(`[data-testid="purchase-project-${orderNo}"]`).innerText()).trim();
  say(`   清空后列表「项目」列 = ${JSON.stringify(clearedList)}`);
  if (clearedList !== '—') throw new Error(`清空后列表项目列应为「—」，实际「${clearedList}」`);
  await page.screenshot({ path: `${OUT}/04-list-project-cleared.png` });

  await page.locator(`[data-testid="btn-view-${orderNo}"]`).click();
  await page.locator('[data-testid="purchase-detail-project"]').waitFor({ state: 'visible', timeout: 15000 });
  await page.waitForTimeout(800);
  const clearedDetail = (await page.locator('[data-testid="purchase-detail-project"]').innerText()).trim();
  say(`   清空后详情「所属项目」= ${JSON.stringify(clearedDetail)}`);
  if (clearedDetail !== '—') throw new Error(`清空后详情项目应为「—」，实际「${clearedDetail}」`);
  await page.screenshot({ path: `${OUT}/05-detail-project-cleared.png` });
  await closeDrawer(page);

  // ==================== ⑥ 接口回读（第二真源） ====================
  const api = await page.evaluate(async ([no, token]) => {
    const resp = await fetch('/api/business/erp/purchase-order/page?page=1&page_size=200', {
      headers: { Authorization: `Bearer ${token}` },
    });
    const body = await resp.json();
    const row = (body.data?.list ?? []).find((r) => r.order_no === no);
    return { status: resp.status, row: row ? { order_no: row.order_no, project_id: row.project_id, project_name: row.project_name } : null };
  }, [orderNo, TOKEN]);
  say(`⑧ 接口回读（清空后）：${JSON.stringify(api)}`);
  if (!api.row || api.row.project_id !== null || api.row.project_name !== null) {
    throw new Error('接口回读的项目字段没被清空：' + JSON.stringify(api));
  }

  say('ALL PASS ✅');
  writeFileSync(`${OUT}/ui-flow.txt`, log.join('\n') + '\n');
} catch (error) {
  say(`FAIL ❌ ${error?.message ?? error}`);
  await page.screenshot({ path: `${OUT}/99-failure.png` }).catch(() => {});
  writeFileSync(`${OUT}/ui-flow.txt`, log.join('\n') + '\n');
  process.exitCode = 1;
} finally {
  await browser.close();
}
