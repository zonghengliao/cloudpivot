/**
 * P1-FIX-7 · 导入模板 / 字段说明入口回归（真后端，不 mock）。
 *
 * # 卡面背景
 *
 * `sys_import_task` 长期 0 行，评审归因为「模板入口深、没有字段说明」。本轮把模板下载与
 * 字段说明搬到 7 个业务页的「导入」弹窗 + 向导页的「模板中心」。**这套入口一旦被后续改动
 * 悄悄删掉/改名，用户又会退回「找不到导入」**，所以用 e2e 钉住：
 *
 * | 用例 | 钉住什么 |
 * |------|----------|
 * | ① | 7 个可导入业务页都有「导入」按钮（data-testid 可点） |
 * | ② | 弹窗里字段说明表有行（来自 `def/columns`）、表头带 `*`、下载模板真的触发下载 |
 * | ③ | 向导页模板中心逐行可下载、`?biz_type=` 预选生效、字段说明抽屉能打开 |
 * | ④ | 上传一个不是 xlsx 的文件 → 页面出错误提示（不是白屏、不是 500 无反馈） |
 *
 * # 判据口径
 *
 * 只断「入口在 + 接口 200 + 下载事件发生」，**不依赖种子数据条数**（生产快照副本与迁移种子
 * 库的条数不同，钉死条数会让用例在别的库上无故变红）。
 */
import { expect, test } from '@playwright/test';
import { Buffer } from 'node:buffer';
import { openAuthed } from './support';

/** 7 个可导入业务：业务页路径 + 按钮 testid + 业务编码（后端注册名） */
const ENTRIES = [
  { path: '/business/erp/material', testid: 'btn-import-material', biz: 'business.erp.material' },
  { path: '/business/erp/supplier', testid: 'btn-import-supplier', biz: 'business.erp.supplier' },
  { path: '/business/erp/unit', testid: 'btn-import-unit', biz: 'business.erp.unit' },
  { path: '/business/erp/account', testid: 'btn-import-account', biz: 'business.erp.account' },
  { path: '/business/erp/warehouse', testid: 'btn-import-warehouse', biz: 'business.erp.warehouse' },
  { path: '/business/erp/location', testid: 'btn-import-location', biz: 'business.erp.location' },
  { path: '/support/dict', testid: 'btn-import-dict-item', biz: 'system.dict_item' },
];

test('① 7 个可导入业务页都有「导入」入口', async ({ page }) => {
  for (const entry of ENTRIES) {
    await openAuthed(page, entry.path);
    await expect(page.locator(`[data-testid="${entry.testid}"]`), `${entry.path} 缺导入入口`).toBeVisible();
  }
});

test('② 物料页导入弹窗：字段说明表有内容 + 表头带星标 + 模板能下载', async ({ page }) => {
  await openAuthed(page, '/business/erp/material');
  await page.click('[data-testid="btn-import-material"]');

  const dialog = page.locator('[data-testid="btn-import-material-dialog"]');
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('字段说明');
  await expect(dialog).toContainText('物料编码*');
  await expect(dialog).toContainText('必填');
  await expect(dialog).toContainText('引用 计量单位');

  // 表体至少 1 行（列数由后端定义决定，不钉死）
  const rows = dialog.locator('[data-testid="btn-import-material-fields"] tbody tr');
  expect(await rows.count()).toBeGreaterThan(0);

  // 一键下载：真的触发浏览器下载事件，文件名带「导入模板」
  const [download] = await Promise.all([
    page.waitForEvent('download', { timeout: 20_000 }),
    page.click('[data-testid="btn-import-material-download"]'),
  ]);
  expect(download.suggestedFilename()).toContain('导入模板');
});

test('③ 向导模板中心逐行下载 + ?biz_type 预选 + 字段说明抽屉', async ({ page }) => {
  await openAuthed(page, '/business/impexp/import?biz_type=business.erp.material');

  // 模板中心：7 个可导入业务逐行展示
  const center = page.locator('[data-testid="import-template-center"]');
  await expect(center).toBeVisible();
  for (const entry of ENTRIES) {
    await expect(center.locator(`[data-testid="import-tpl-download-${entry.biz}"]`)).toBeVisible();
  }

  // 预选：表单里的业务已是物料，字段说明面板随之就绪
  await expect(page.locator('[data-testid="import-field-panel"]')).toContainText('字段说明 · 物料');
  await expect(page.locator('[data-testid="import-field-panel-table"]')).toContainText('物料编码*');

  // 行内一键下载（供应商，验证按行取 biz_type 而不是只看表单当前值）
  const [download] = await Promise.all([
    page.waitForEvent('download', { timeout: 20_000 }),
    page.click('[data-testid="import-tpl-download-business.erp.supplier"]'),
  ]);
  expect(download.suggestedFilename()).toContain('供应商');

  // 字段说明抽屉
  await page.click('[data-testid="import-tpl-fields-business.erp.supplier"]');
  const drawer = page.locator('.t-drawer');
  await expect(drawer).toBeVisible();
  await expect(drawer).toContainText('字段说明 · 供应商');
  await expect(drawer).toContainText('供应商编码*');
});

test('④ 上传非 xlsx 文件：页面给出错误提示（不白屏、不静默）', async ({ page }) => {
  await openAuthed(page, '/business/impexp/import?biz_type=business.erp.material');

  await page.setInputFiles('input.file-input', {
    name: 'not-a-workbook.xlsx',
    mimeType: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    buffer: Buffer.from('这不是 xlsx，只是一段文本', 'utf8'),
  });
  await page.click('text=开始校验');

  // 结构校验失败：错误提示要可见且说明「文件」问题（不是静默失败）
  const alert = page.locator('.step-alert').filter({ hasText: /文件|格式|xlsx|Excel/ }).first();
  await expect(alert).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('.fail-table')).toHaveCount(0);
});
