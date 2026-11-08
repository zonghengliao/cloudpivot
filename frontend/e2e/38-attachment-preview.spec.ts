/**
 * ATT-01 附件在线预览（真实浏览器，E2E）。
 *
 * # 覆盖的验收项
 *
 * | 验收 | 用例 |
 * |------|------|
 * | ① 上传 jpg/png/pdf/xlsx/zip 各一个 → 点开 | `beforeAll` 通过真实上传接口灌 5 个夹具，用例再点「预览」 |
 * | ② jpg/png 能显示 + 滚轮缩放 | `jpg：显示 + 滚轮缩放 + 旋转` |
 * | ③ pdf 能翻页 | `pdf：画布渲染 + 翻页` |
 * | ④ xlsx/zip 只显示下载按钮，点了能下载 | `xlsx / zip：只给下载` |
 * | ⑤ 多附件左右切换 | `多附件左右切换` |
 * | ⑥ 关闭后 blob URL 已释放 | 每个用例结束都断言 `URL.createObjectURL/revokeObjectURL` 记账归零 |
 *
 * # 为什么自己要造夹具
 * 图片用页面里的 canvas 现场生成（保证是真 JPEG/PNG，不然「显示不出来」到底是夹具坏还是
 * 组件坏分不清）；PDF 用最小合法对象图手搓两页（验证翻页要有 >1 页）；xlsx / zip 只要求
 * 扩展名（后端上传白名单按扩展名判），内容不参与断言。
 *
 * # 目标
 * 走隔离实例（`http://127.0.0.1:5189`，vite dev 反代到隔离后端），`target-guard` 会硬拦非隔离目标。
 * 起法见 `scripts/verify/e2e_isolated_instance.sh`；本卡实测时用本地 vite dev 反代 145 测试环境：
 *
 * ```bash
 * BACKEND_ORIGIN=http://172.18.10.145 ./node_modules/.bin/vite --port 5189 --strictPort
 * E2E_USERNAME=admin E2E_PASSWORD=*** pnpm exec playwright test e2e/38-attachment-preview.spec.ts
 * ```
 */
import { expect, test, request as playwrightRequest, type Page } from '@playwright/test';
import { readFileSync, statSync } from 'node:fs';
import { resolve } from 'node:path';

const BASE_URL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:5189';
/** 本轮唯一的文件名前缀：列表页按它搜索，避免和其他数据混在一起 */
const RUN = `att01${Date.now().toString(36)}`;

interface Fixture {
  /** 唯一文件名（带扩展名） */
  name: string;
  mimeType: string;
  buffer: Buffer;
}

/** 5 个夹具：jpg / png / pdf / xlsx / zip */
const fixtures: Fixture[] = [];

/**
 * 手搓一个最小合法 PDF（2 页）。
 *
 * 只有 Catalog / Pages / Font / Page / Contents 这几类对象，内容流不压缩（免去 zlib），
 * xref 用实际字节偏移算 —— pdf.js 虽然能修复坏 xref，但夹具坏了会掩盖真问题。
 */
export function buildTwoPagePdf(tag = ''): Buffer {
  const objects: string[] = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>',
    '',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents 7 0 R >>',
    '',
  ];
  const streams = [
    `BT /F1 24 Tf 72 760 Td (ATT-01 PDF Page 1 ${tag}) Tj ET`,
    `BT /F1 24 Tf 72 760 Td (ATT-01 PDF Page 2 ${tag}) Tj ET`,
  ];
  // 对象 5 / 7 是内容流（索引 4 / 6）
  objects[4] = `<< /Length ${Buffer.byteLength(streams[0])} >>\nstream\n${streams[0]}\nendstream`;
  objects[6] = `<< /Length ${Buffer.byteLength(streams[1])} >>\nstream\n${streams[1]}\nendstream`;

  let body = '%PDF-1.4\n';
  const offsets: number[] = [];
  objects.forEach((obj, index) => {
    offsets.push(Buffer.byteLength(body));
    body += `${index + 1} 0 obj\n${obj}\nendobj\n`;
  });
  const xrefStart = Buffer.byteLength(body);
  body += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const offset of offsets) {
    body += `${String(offset).padStart(10, '0')} 00000 n \n`;
  }
  body += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xrefStart}\n%%EOF\n`;
  return Buffer.from(body, 'latin1');
}

/** 现场用 canvas 生成真图片（返回去掉 data: 前缀的 base64） */
async function canvasImage(page: Page, type: 'image/png' | 'image/jpeg', text: string): Promise<Buffer> {
  const dataUrl: string = await page.evaluate(
    ({ mime, label }) => {
      const canvas = document.createElement('canvas');
      canvas.width = 160;
      canvas.height = 100;
      const ctx = canvas.getContext('2d');
      if (!ctx) {
        throw new Error('canvas 2d 不可用');
      }
      ctx.fillStyle = '#2b6cb0';
      ctx.fillRect(0, 0, 160, 100);
      ctx.fillStyle = '#ffffff';
      ctx.font = '16px sans-serif';
      ctx.fillText(label, 8, 56);
      return canvas.toDataURL(mime, 0.92);
    },
    { mime: type, label: text },
  );
  return Buffer.from(dataUrl.slice(dataUrl.indexOf(',') + 1), 'base64');
}

test.beforeAll(async ({ browser }) => {
  const context = await browser.newContext();
  const page = await context.newPage();
  const jpg = await canvasImage(page, 'image/jpeg', `ATT-01 JPG ${RUN}`);
  const png = await canvasImage(page, 'image/png', `ATT-01 PNG ${RUN}`);
  await context.close();

  // 注意：夹具内容也必须每轮唯一。后端上传有「内容哈希秒传」，字节相同会复用**旧记录**
  // （连 original_name 都是旧的），于是按本轮 RUN 搜不到 —— 这正是第一版 e2e 红的原因。
  fixtures.push(
    { name: `${RUN}-1.jpg`, mimeType: 'image/jpeg', buffer: jpg },
    { name: `${RUN}-2.png`, mimeType: 'image/png', buffer: png },
    { name: `${RUN}-3.pdf`, mimeType: 'application/pdf', buffer: buildTwoPagePdf(RUN) },
    { name: `${RUN}-4.xlsx`, mimeType: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet', buffer: Buffer.from(`ATT-01 xlsx placeholder ${RUN} (只下载，不解析)`) },
    { name: `${RUN}-5.zip`, mimeType: 'application/zip', buffer: Buffer.from(`ATT-01 zip placeholder ${RUN} (只下载，不解析)`) },
  );

  const token = readFileSync(resolve(process.cwd(), 'e2e/.auth/token.txt'), 'utf8').trim();
  const api = await playwrightRequest.newContext({
    baseURL: BASE_URL,
    extraHTTPHeaders: { Authorization: `Bearer ${token}` },
  });
  try {
    for (const fixture of fixtures) {
      const res = await api.post('/api/support/file/upload', {
        multipart: {
          file: { name: fixture.name, mimeType: fixture.mimeType, buffer: fixture.buffer },
        },
      });
      expect(res.status(), `上传 ${fixture.name} 应 200`).toBe(200);
      const body = await res.json();
      expect(body.code, `上传 ${fixture.name} 业务码应为 0：${JSON.stringify(body).slice(0, 200)}`).toBe(0);
    }
  } finally {
    await api.dispose();
  }
});

/** 给页面装 blob URL 记账：创建 / 释放各计一次，`live` 归零即「无泄漏」 */
async function installBlobStats(page: Page): Promise<void> {
  await page.addInitScript(() => {
    // 登录后的「未读汇总」弹窗会挡住第一次点击（它不是本组用例的被测对象）：
    // 直接打上「已看过」标记，等价于用户点过「知道了」
    try {
      window.sessionStorage.setItem('login-unread-shown', '1');
    } catch {
      /* 隐私模式下 setItem 可能抛错，忽略即可 */
    }
    const stats = { created: 0, revoked: 0, live: 0 };
    (window as unknown as { __blobStats: typeof stats }).__blobStats = stats;
    const create = URL.createObjectURL.bind(URL);
    const revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = (obj: Blob | MediaSource): string => {
      stats.created += 1;
      stats.live += 1;
      return create(obj as Blob);
    };
    URL.revokeObjectURL = (url: string): void => {
      stats.revoked += 1;
      stats.live -= 1;
      revoke(url);
    };
  });
}

/** 兜底：未读弹窗若已经弹出来，点「知道了」关掉（标记方式在不同会话里可能失效） */
async function dismissLoginUnread(page: Page): Promise<void> {
  const ok = page.getByTestId('login-unread-ok');
  if (await ok.isVisible().catch(() => false)) {
    await ok.click();
    await expect(ok).toBeHidden();
  }
}

/** 打开文件管理页并筛出本轮夹具 */
async function openFileList(page: Page): Promise<void> {
  await page.goto('/support/file');
  await dismissLoginUnread(page);
  await page.getByPlaceholder('文件名称').fill(RUN);
  await page.getByRole('button', { name: '查询' }).click();
  await expect(page.getByTestId('file-row-preview')).toHaveCount(fixtures.length, { timeout: 30_000 });
}

/** 点某一行的「预览」，返回弹窗根节点 */
async function openPreviewOf(page: Page, name: string) {
  await page.locator('tr', { hasText: name }).first().getByTestId('file-row-preview').click();
  const dialog = page.getByTestId('attachment-preview');
  await expect(dialog).toBeVisible();
  return dialog;
}

/** 读 blob 记账 */
async function blobStats(page: Page): Promise<{ created: number; revoked: number; live: number }> {
  return page.evaluate(() => (window as unknown as { __blobStats: { created: number; revoked: number; live: number } }).__blobStats);
}

/** 关闭弹窗（ESC），并断言 blob URL 已全部释放 */
async function closeAndAssertNoLeak(page: Page): Promise<void> {
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('attachment-preview')).toBeHidden();
  await expect
    .poll(async () => (await blobStats(page)).live, { timeout: 10_000, message: '关闭弹窗后仍有未释放的 blob URL' })
    .toBe(0);
}

test('jpg：显示 + 滚轮缩放 + 旋转；关闭后 blob URL 已释放', async ({ page }) => {
  await installBlobStats(page);
  await openFileList(page);
  await openPreviewOf(page, `${RUN}-1.jpg`);

  const img = page.getByTestId('attachment-preview-img');
  await expect(img).toBeVisible();
  // 真渲染出来了（不是碎图）：naturalWidth > 0
  await expect.poll(() => img.evaluate((el) => (el as HTMLImageElement).naturalWidth), { timeout: 15_000 }).toBeGreaterThan(0);
  await expect(page.getByTestId('attachment-preview-zoom')).toHaveText('100%');

  // 滚轮缩放：每格 10%
  await img.hover();
  await page.mouse.wheel(0, -240);
  await expect(page.getByTestId('attachment-preview-zoom')).not.toHaveText('100%');

  // 按钮缩放：每格 25%
  const before = await page.getByTestId('attachment-preview-zoom').textContent();
  await page.getByTestId('attachment-preview-zoom-in').click();
  await expect(page.getByTestId('attachment-preview-zoom')).not.toHaveText(before ?? '');

  // 旋转 90°，transform 上必须体现出来
  await page.getByTestId('attachment-preview-rotate').click();
  await expect(img).toHaveAttribute('style', /rotate\(90deg\)/);

  // 右侧元信息：文件名 / 大小 / 上传人 / 上传时间 / 备注
  await expect(page.getByTestId('attachment-preview-meta-name')).toContainText(`${RUN}-1.jpg`);
  await expect(page.getByTestId('attachment-preview-meta-size')).not.toHaveText('-');
  await expect(page.getByTestId('attachment-preview-meta-time')).not.toHaveText('-');
  await expect(page.getByTestId('attachment-preview-meta-remark')).toBeVisible();

  await closeAndAssertNoLeak(page);
});

test('png：同样能显示；多附件可左右切换（计数器 + 元信息一起变）', async ({ page }) => {
  await installBlobStats(page);
  await openFileList(page);
  const dialog = await openPreviewOf(page, `${RUN}-2.png`);

  const img = page.getByTestId('attachment-preview-img');
  await expect.poll(() => img.evaluate((el) => (el as HTMLImageElement).naturalWidth), { timeout: 15_000 }).toBeGreaterThan(0);
  const firstCounter = await page.getByTestId('attachment-preview-counter').textContent();
  expect(firstCounter).toContain(`/ ${fixtures.length}`);

  // 下一张：计数器与元信息文件名必须一起变
  await page.getByTestId('attachment-preview-next').click();
  const secondCounter = await page.getByTestId('attachment-preview-counter').textContent();
  expect(secondCounter).not.toBe(firstCounter);
  const secondName = await page.getByTestId('attachment-preview-meta-name').textContent();
  expect(secondName).not.toContain(`${RUN}-2.png`);

  // 上一张：回到 png
  await page.getByTestId('attachment-preview-prev').click();
  await expect(page.getByTestId('attachment-preview-meta-name')).toContainText(`${RUN}-2.png`);
  await expect(dialog).toBeVisible();

  await closeAndAssertNoLeak(page);
});

test('pdf：画布渲染 + 翻页（1/2 → 2/2）', async ({ page }) => {
  await installBlobStats(page);
  await openFileList(page);
  await openPreviewOf(page, `${RUN}-3.pdf`);

  const canvas = page.getByTestId('attachment-preview-canvas');
  await expect(canvas).toBeVisible({ timeout: 20_000 });
  await expect(page.getByTestId('attachment-preview-pdf-page')).toHaveText('1 / 2', { timeout: 20_000 });
  // 画布真的画了东西：宽高 > 0
  await expect.poll(() => canvas.evaluate((el) => (el as HTMLCanvasElement).width), { timeout: 15_000 }).toBeGreaterThan(0);

  await page.getByTestId('attachment-preview-pdf-next').click();
  await expect(page.getByTestId('attachment-preview-pdf-page')).toHaveText('2 / 2');
  await expect(page.getByTestId('attachment-preview-pdf-next')).toBeDisabled();
  await page.getByTestId('attachment-preview-pdf-prev').click();
  await expect(page.getByTestId('attachment-preview-pdf-page')).toHaveText('1 / 2');

  await closeAndAssertNoLeak(page);
});

test('xlsx / zip：不预览，只给「下载」按钮，点了真下载', async ({ page }) => {
  await installBlobStats(page);
  await openFileList(page);

  for (const [name, ext] of [
    [`${RUN}-4.xlsx`, '.xlsx'],
    [`${RUN}-5.zip`, '.zip'],
  ] as const) {
    await openPreviewOf(page, name);
    // 不是图片、不是 PDF：不渲染 img / canvas
    await expect(page.getByTestId('attachment-preview-fallback')).toBeVisible();
    await expect(page.getByTestId('attachment-preview-img')).toHaveCount(0);
    await expect(page.getByTestId('attachment-preview-canvas')).toHaveCount(0);
    await expect(page.getByTestId('attachment-preview-fallback-download')).toBeVisible();

    const [download] = await Promise.all([
      page.waitForEvent('download', { timeout: 20_000 }),
      page.getByTestId('attachment-preview-fallback-download').click(),
    ]);
    expect(download.suggestedFilename()).toContain(ext);
    const savedPath = await download.path();
    expect(savedPath).toBeTruthy();
    expect(statSync(savedPath as string).size).toBeGreaterThan(0);

    await closeAndAssertNoLeak(page);
  }
});
