/**
 * ATT-03 文件管理「上传人」显示名（真实浏览器 E2E）。
 *
 * # 覆盖的验收项
 *
 * | 验收 | 用例 |
 * |------|------|
 * | ① 列表「上传人」列显示真名/用户名，不是 UUID | `列表：上传人列显示显示名` |
 * | ② 预览弹窗右侧「上传人」同样显示名字 | `预览：右侧文件信息的上传人同口径` |
 * | ③ 指向不存在用户的 uploaded_by → 显示 '-' | `脏数据：上传人显示 '-'`（需 `E2E_DIRTY_FILE_ID`，由 20/30 脚本先改库触发） |
 *
 * # 为什么 ③ 要外部触发
 *
 * 「`uploaded_by` 指向不存在用户」这种历史脏数据没有任何接口能造（也不该有），
 * 只能在隔离实例的**拷贝库**里 UPDATE。所以本 spec 接收 `E2E_DIRTY_FILE_ID`：
 * 外部脚本把某行的 `uploaded_by` 改成幽灵 UUID 后再跑一次，本用例断言界面显示 `-`。
 * 不提供该变量时只跑 ①②，不静默跳过断言（用例名带「脏数据」前缀，输出里一眼能看出跑没跑）。
 *
 * # 目标
 *
 * 走隔离实例（默认 `http://127.0.0.1:5189`，vite dev 反代隔离后端；`target-guard` 硬拦非隔离目标）：
 *
 * ```bash
 * BACKEND_ORIGIN=http://127.0.0.1:18229 ./node_modules/.bin/vite --port 5189 --strictPort
 * E2E_USERNAME=admin E2E_PASSWORD=*** E2E_RUN=att03xxx \
 *   pnpm exec playwright test e2e/39-file-uploader-name.spec.ts
 * ```
 */
import { expect, test, request as playwrightRequest, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const BASE_URL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:5189';
/** 本轮文件名前缀：列表页按它搜索；不带时自己造一个 */
const RUN = process.env.E2E_RUN ?? `att03${Date.now().toString(36)}`;
/** 外部造好的脏数据行 id（`uploaded_by` 已指向不存在的用户） */
const DIRTY_ID = process.env.E2E_DIRTY_FILE_ID ?? '';

/** 后端下发的「上传人显示名」期望值（`real_name` 优先 → `username`） */
let expectedName = '';
/** 本轮夹具文件名 */
const FILE_NAME = `${RUN}-上传人.png`;

/**
 * 造夹具：经真实上传接口灌一个 PNG。
 *
 * 字节里掺 RUN，避免后端「内容哈希秒传」把**别人**上传的旧记录复用回来
 * （复用会让 uploaded_by 变成别人，名字断言就没意义了）。
 */
async function ensureFixture(): Promise<void> {
  const token = readFileSync(resolve(process.cwd(), 'e2e/.auth/token.txt'), 'utf8').trim();
  const api = await playwrightRequest.newContext({
    baseURL: BASE_URL,
    extraHTTPHeaders: { Authorization: `Bearer ${token}` },
  });
  try {
    // 期望值来自真实的 /system/auth/me（不写死 admin 的中文名）
    const meRes = await api.get('/api/system/auth/me');
    expect(meRes.status(), '/system/auth/me 应 200').toBe(200);
    const me = await meRes.json();
    expectedName = String(me.data?.real_name ?? '').trim() || String(me.data?.username ?? '');
    expect(expectedName, '当前用户必须有显示名').not.toBe('');

    const existing = await api.get(
      `/api/support/file/page?page=1&page_size=1&original_name=${encodeURIComponent(RUN)}`,
    );
    const existingBody = await existing.json();
    if ((existingBody?.data?.list?.length ?? 0) > 0) {
      return; // 已经造过（重跑幂等）
    }

    const png = Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      Buffer.from(`ATT-03 uploader fixture ${RUN}`),
    ]);
    const res = await api.post('/api/support/file/upload?category=attachment', {
      multipart: { file: { name: FILE_NAME, mimeType: 'image/png', buffer: png } },
    });
    expect(res.status(), '上传夹具应 200').toBe(200);
    const body = await res.json();
    expect(body.code, `上传夹具业务码应为 0：${JSON.stringify(body).slice(0, 200)}`).toBe(0);
    // 上传响应本身就该带显示名（ATT-03 的接口契约）
    expect(body.data.uploader_name, '上传响应 uploader_name 应是显示名').toBe(expectedName);
  } finally {
    await api.dispose();
  }
}

/**
 * 登录后的「未读汇总」弹窗会挡住第一次点击（不是本卡被测对象）：
 * 打上「已看过」标记（与 38 号 spec 同款），另加一次兜底点击。
 */
async function markLoginUnreadSeen(page: Page): Promise<void> {
  await page.addInitScript(() => {
    try {
      window.sessionStorage.setItem('login-unread-shown', '1');
    } catch {
      /* 隐私模式下 setItem 可能抛错，忽略 */
    }
  });
}

/** 兜底：未读弹窗若已经弹出来，点「知道了」关掉 */
async function dismissLoginUnread(page: Page): Promise<void> {
  const ok = page.getByTestId('login-unread-ok');
  if (await ok.isVisible().catch(() => false)) {
    await ok.click();
    await expect(ok).toBeHidden();
  }
}

/** 按文件名搜索，返回该行的「上传人」单元格文本 */
async function uploaderCellOf(page: Page, keyword: string): Promise<string> {
  await page.goto('/support/file');
  await dismissLoginUnread(page);
  await page.getByPlaceholder('文件名称').fill(keyword);
  await page.getByRole('button', { name: '查询' }).click();
  const row = page.locator('tr').filter({ hasText: keyword }).first();
  await expect(row).toBeVisible({ timeout: 30_000 });
  const cell = row.locator('[data-testid="file-uploader-name"]').first();
  await expect(cell).toBeVisible();
  return (await cell.innerText()).trim();
}

test.beforeEach(async ({ page }) => {
  await markLoginUnreadSeen(page);
});

test.beforeAll(async () => {
  await ensureFixture();
});

test('列表：上传人列显示显示名（不是 UUID）', async ({ page }) => {
  const text = await uploaderCellOf(page, RUN);
  expect(text, '上传人列必须显示显示名').toBe(expectedName);
  expect(text, '上传人列不许再是 UUID').not.toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-/i);
});

test('预览：右侧文件信息的上传人同口径', async ({ page }) => {
  await page.goto('/support/file');
  await dismissLoginUnread(page);
  await page.getByPlaceholder('文件名称').fill(RUN);
  await page.getByRole('button', { name: '查询' }).click();
  const row = page.locator('tr').filter({ hasText: RUN }).first();
  await expect(row).toBeVisible({ timeout: 30_000 });
  await row.locator('[data-testid="file-row-preview"]').click();

  const panel = page.locator('[data-testid="attachment-preview"]');
  await expect(panel).toBeVisible();
  // 右侧「文件信息」里的「上传人」一行（组件已留 data-testid）
  const uploader = page.locator('[data-testid="attachment-preview-meta-uploader"]');
  await expect(uploader).toBeVisible();
  await expect(uploader).toHaveText(expectedName);
  await expect(panel).not.toContainText(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i);
});

test('脏数据：uploaded_by 指向不存在用户时上传人显示 "-"', async ({ page }) => {
  expect(DIRTY_ID, '需要 E2E_DIRTY_FILE_ID（外部脚本先把该行 uploaded_by 改成幽灵 UUID）').not.toBe('');
  const token = readFileSync(resolve(process.cwd(), 'e2e/.auth/token.txt'), 'utf8').trim();
  const api = await playwrightRequest.newContext({
    baseURL: BASE_URL,
    extraHTTPHeaders: { Authorization: `Bearer ${token}` },
  });
  let dirtyName = '';
  try {
    const res = await api.get(`/api/support/file/${DIRTY_ID}`);
    expect(res.status(), '脏数据行详情应 200（不报错）').toBe(200);
    const body = await res.json();
    expect(body.data.uploader_name, '脏数据行 uploader_name 应为空串').toBe('');
    dirtyName = String(body.data.original_name ?? '');
  } finally {
    await api.dispose();
  }
  expect(dirtyName, '脏数据行要有原始文件名才好搜').not.toBe('');

  const text = await uploaderCellOf(page, dirtyName);
  expect(text, '上传人已不存在时显示 "-"，不显示 UUID').toBe('-');
});
