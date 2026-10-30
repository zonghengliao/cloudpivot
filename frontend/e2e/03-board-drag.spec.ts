import { expect, test } from '@playwright/test';
import { newApi, openAuthed, toast, RUN, type Api } from './support';

/**
 * 流程 3：看板拖拽（**最关键的交互**）
 *
 * 1. 切到看板视图 → 断言 5 列（待开始 / 进行中 / 待验收 / 已完成 / 已取消）；
 * 2. 拖 TODO 卡片到 DOING 列 → 断言卡片真的换了列 + 两列计数都变了（后端已落地）；
 * 3. 拖另一张 TODO 卡片直接到 DONE（跳级）→ 断言**回弹**（卡片仍在 TODO）+ 错误提示。
 *
 * 数据：两张卡片都用 API 预置在本账号名下（真接口），用例结束 API 删除，不留脏数据。
 */
test.describe('流程3 看板拖拽', () => {
  const titleMove = `[E2E]拖拽-正常-${RUN}`;
  const titleJump = `[E2E]拖拽-跳级-${RUN}`;
  let api: Api;
  let idMove = '';
  let idJump = '';

  test.beforeAll(async () => {
    api = await newApi();
    idMove = (await api.createTask({ title: titleMove, priority: 'P2' })).id;
    idJump = (await api.createTask({ title: titleJump, priority: 'P2' })).id;
  });

  test.afterAll(async () => {
    for (const id of [idMove, idJump]) {
      if (id) {
        await api.deleteTask(id).catch(() => undefined);
      }
    }
    await api.dispose();
  });

  test('五列 / 拖到进行中 / 跳级回弹 + 错误提示', async ({ page }) => {

    await openAuthed(page, '/task?view=board');

    // ---------- ① 五列 ----------
    const cols = page.locator('.board .col');
    await expect(cols).toHaveCount(5);
    for (const [status, name] of [
      ['TODO', '待开始'],
      ['DOING', '进行中'],
      ['REVIEW', '待验收'],
      ['DONE', '已完成'],
      ['CANCELLED', '已取消'],
    ] as const) {
      const col = page.locator(`.board .col[data-status="${status}"]`);
      await expect(col).toBeVisible();
      await expect(col.locator('.col__name')).toHaveText(name);
    }

    const col = (status: string) => page.locator(`.board .col[data-status="${status}"]`);
    const countOf = async (status: string) => Number(await col(status).locator('.col__count').innerText());

    const todoBefore = await countOf('TODO');
    const doingBefore = await countOf('DOING');
    const cardMove = col('TODO').locator('.board-card').filter({ hasText: titleMove }).first();
    await expect(cardMove).toBeVisible({ timeout: 20_000 });

    // ---------- ② TODO → DOING（合法流转） ----------
    await cardMove.dragTo(col('DOING'));
    await expect(col('DOING').locator('.board-card').filter({ hasText: titleMove })).toHaveCount(1, {
      timeout: 20_000,
    });
    await expect(col('TODO').locator('.board-card').filter({ hasText: titleMove })).toHaveCount(0);
    await expect(toast(page, '已移动到「进行中」')).toBeVisible({ timeout: 20_000 });
    // 计数：TODO -1，DOING +1（后端总数，不是本地临时值 —— 断言前已收到回执）
    await expect
      .poll(async () => [await countOf('TODO'), await countOf('DOING')], { timeout: 20_000 })
      .toEqual([todoBefore - 1, doingBefore + 1]);

    // 刷新页面：卡片仍在「进行中」= 状态真的落库了（不是只改了 DOM）
    await page.reload();
    await expect(col('DOING').locator('.board-card').filter({ hasText: titleMove })).toHaveCount(1, {
      timeout: 20_000,
    });

    // ---------- ③ TODO → DONE（跳级，非法） ----------
    const doneBefore = await countOf('DONE');
    const todoBefore2 = await countOf('TODO');
    const cardJump = col('TODO').locator('.board-card').filter({ hasText: titleJump }).first();
    await expect(cardJump).toBeVisible({ timeout: 20_000 });

    await cardJump.dragTo(col('DONE'));

    // 错误提示：后端 409 + 63010 的原文（说明为什么不行，而不是「操作失败」）
    await expect(toast(page, '该状态流转不被允许')).toBeVisible({ timeout: 20_000 });
    // 回弹：卡片回到 TODO，DONE 列没有它
    await expect(col('TODO').locator('.board-card').filter({ hasText: titleJump })).toHaveCount(1, {
      timeout: 20_000,
    });
    await expect(col('DONE').locator('.board-card').filter({ hasText: titleJump })).toHaveCount(0);
    await expect
      .poll(async () => [await countOf('TODO'), await countOf('DONE')], { timeout: 20_000 })
      .toEqual([todoBefore2, doneBefore]);
  });
});
