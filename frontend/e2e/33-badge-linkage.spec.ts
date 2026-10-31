import { expect, test } from '@playwright/test';
import { newApi, readSharedToken, RUN, type Api } from './support';

/**
 * 专项 4：**徽标联动**（公告 / 消息 / 审批 / 任务）。
 *
 * 徽标只有一份数据源：`GET /api/system/badge/counts`（四个桶，见 `src/api/badge.ts`）。
 * 侧栏 `.side-menu-item__badge` 与顶栏铃铛都读它。所以每条的验证方式统一为：
 *   ① 用接口读**动作前**的桶值；
 *   ② 用真动作改变业务状态（页面点「标记已读」/ 接口流转任务 / 审批通过）；
 *   ③ 再读接口 + 重新进页面看侧栏徽标数字 —— 三个数必须对得上，且**正好 -1**。
 *
 * 为什么任务/审批的"动作"用接口而不是纯 UI：这两条链路（建任务 → 看板拖拽 → 状态机、
 * 建流程 → 发布 → 发起 → 同意）在 03 / 05 两条 e2e 里已经全 UI 覆盖了；这里验的是
 * 「业务状态变了，徽标是否跟着变」这一层联动，用接口造/推状态更稳，也不会因为拖拽
 * 偶发波动把徽标的结论带歪。
 */
const headers = () => ({ Authorization: `Bearer ${readSharedToken()}` });

interface BadgeCounts {
  announcement_unread: number;
  notice_unread: number;
  flow_todo: number;
  task_todo: number;
}

async function badges(api: Api): Promise<BadgeCounts> {
  const res = await api.ctx.get('/api/system/badge/counts', { headers: headers() });
  expect(res.status(), '徽标接口状态').toBe(200);
  const body = await res.json();
  expect(body.code, '徽标接口业务码').toBe(0);
  return body.data as BadgeCounts;
}

/**
 * 侧栏某菜单标题上的徽标数字（没有徽标 = 0）。
 *
 * 类名口径（2026-10-31 实测，别再按组件名猜）：菜单项是 TDesign 的 `<t-menu-item>` /
 * `<t-submenu>`，**没有 `.side-menu-item` 这个类**；仓库自写的节点是
 * `.side-menu-item__title-wrap`（标题 + 徽标的容器）与 `.side-menu-item__badge`（t-badge）。
 *
 * 折叠态陷阱：二级菜单在父目录收起时**仍在 DOM 里但不可见**（`offsetParent=null`），
 * 直接 `toBeVisible()` 会超时。展开必须点父目录 `li.t-submenu` 里的 `.t-menu__content`
 * （点自写的 `.side-menu-item__title` 会触发导航而不是展开）。
 */
async function revealMenuItem(page: import('@playwright/test').Page, title: string) {
  const item = page.locator('.side-menu-item__title-wrap').filter({ hasText: title }).first();
  for (let depth = 0; depth < 3; depth += 1) {
    if (await item.isVisible().catch(() => false)) {
      return item;
    }
    const content = item
      .locator(
        // 祖先必须是**目录节点** `li.t-submenu`；叶子节点的 class 是 `t-menu__item t-submenu__item`，
        // 用 `contains(@class,'t-submenu')` 会把它自己也匹配进来（于是点到的是隐藏的叶子，超时）
        'xpath=ancestor::li[contains(concat(" ", normalize-space(@class), " "), " t-submenu ")][1]//span[contains(@class,"t-menu__content")]',
      )
      .first();
    if ((await content.count()) === 0) {
      break;
    }
    await content.click({ timeout: 5_000 }).catch(() => undefined);
    // 展开动画有过渡，点完立刻 isVisible() 会读到中间态，于是下一轮又点一次把菜单收起来
    // （2026-10-31 实测「点了反而变成 hidden」就是这条）。必须等一次 visible 再判断。
    const shown = await item.waitFor({ state: 'visible', timeout: 3_000 }).then(() => true).catch(() => false);
    if (shown) {
      return item;
    }
  }
  return item;
}

async function sidebarBadge(page: import('@playwright/test').Page, title: string): Promise<number> {
  const item = await revealMenuItem(page, title);
  await expect(item).toBeVisible();
  const badge = item.locator('.side-menu-item__badge');
  if ((await badge.count()) === 0) {
    return 0;
  }
  const text = ((await badge.first().innerText()) || '').trim();
  const n = Number(text.replace(/[^\d]/g, ''));
  return Number.isFinite(n) ? n : 0;
}

test.describe('专项4 徽标联动', () => {
  test('徽标口径一致：侧栏四个徽标数字 == /api/system/badge/counts', async ({ page }) => {
    const api = await newApi();
    const counts = await badges(api);
    await api.dispose();

    await page.goto('/dashboard');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    // 顶栏铃铛读的是同一个 notice_unread。
    // 菜单是「目录 + 叶子」两级：徽标挂在各自标题上（目录的徽标 = 子树里各桶之和）。
    const pairs: Array<[string, number]> = [
      ['公告中心', counts.announcement_unread],
      ['消息中心', counts.notice_unread],
      ['审批中心', counts.flow_todo],
      ['工作任务', counts.task_todo],
    ];
    for (const [title, expected] of pairs) {
      const shown = await sidebarBadge(page, title);
      // t-badge 超过 99 渲染 99+，所以比对时按 min(99) 收敛
      expect(shown, `${title} 徽标：接口 ${expected} / 页面 ${shown}`).toBe(Math.min(expected, 99));
    }
    // 目录聚合：公告与消息 = 公告未读 + 消息未读（两个桶相加）
    const dirSum = counts.announcement_unread + counts.notice_unread;
    expect(
      await sidebarBadge(page, '公告与消息'),
      `目录「公告与消息」徽标应为两桶之和 ${dirSum}`,
    ).toBe(Math.min(dirSum, 99));
  });

  test('消息已读 → 消息中心徽标 -1', async ({ page }) => {
    const api = await newApi();
    // 副本库（生产快照）里这个测试账号的未读数可能是 0 —— 那样「-1」根本无从观测。
    // 不 skip、不假装：先用管理端发送一条只发给自己的通知把未读数种起来，再验联动。
    let before = await badges(api);
    if (before.notice_unread === 0) {
      const me = await api.ctx.get('/api/system/auth/me');
      expect(me.status(), '读取当前账号').toBe(200);
      const myId = (await me.json())?.data?.id;
      expect(myId, '当前账号 id').toBeTruthy();
      const seeded = await api.ctx.post('/api/business/notice', {
        headers: headers(),
        data: {
          notice_type: 'SYSTEM',
          level: 'INFO',
          title: `[E2E]徽标种子通知-${RUN}`,
          content: '用于验证「消息已读 → 徽标 -1」的种子通知，跑完即被标记已读。',
          target_type: 'USER',
          target_ids: [myId],
        },
      });
      expect(seeded.status(), '种子通知发送状态').toBe(200);
      before = await badges(api);
    }
    expect(before.notice_unread, '消息未读数应 > 0（不足时上面已用种子通知补齐）').toBeGreaterThan(0);

    await page.goto('/business/notice/message');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    const markBtn = page.locator('button').filter({ hasText: /^标记已读$/ }).first();
    await expect(markBtn).toBeVisible({ timeout: 20_000 });
    await markBtn.click();

    // 等接口侧真的变了（30s 轮询太慢，直接查接口）
    await expect
      .poll(async () => (await badges(api)).notice_unread, { timeout: 20_000, message: '未读数应减 1' })
      .toBe(before.notice_unread - 1);
    const after = await badges(api);
    await api.dispose();

    await page.goto('/dashboard');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    expect(await sidebarBadge(page, '消息中心'), '侧栏消息徽标应等于新未读数').toBe(Math.min(after.notice_unread, 99));
  });

  test('任务流转 → 工作任务徽标 -1', async ({ page }) => {
    const api = await newApi();
    const before = await badges(api);
    const task = await api.createTask({ title: `[E2E]徽标任务-${RUN}`, priority: 'P1' });
    try {
      await expect.poll(async () => (await badges(api)).task_todo, { timeout: 15_000 }).toBe(before.task_todo + 1);

      const res = await api.ctx.patch(`/api/business/task/${task.id}/status`, {
        headers: headers(),
        data: { to: 'DOING' },
      });
      expect(res.status(), '任务流转状态码').toBe(200);

      await expect.poll(async () => (await badges(api)).task_todo, { timeout: 15_000 }).toBe(before.task_todo);
      const after = await badges(api);
      await page.goto('/dashboard');
      await expect(page.locator('.basic-layout__aside')).toBeVisible();
      expect(await sidebarBadge(page, '工作任务'), `侧栏任务徽标应回到 ${after.task_todo}`).toBe(Math.min(after.task_todo, 99));
    } finally {
      await api.deleteTask(task.id).catch(() => undefined);
      await api.dispose();
    }
  });

  test('审批处理 → 审批中心徽标 -1', async () => {
    const api = await newApi();
    const before = await badges(api);
    const def = await api.createFlowDef({
      flow_code: `e2e_badge_${RUN}`,
      flow_name: `[E2E]徽标审批流程-${RUN}`,
      biz_type: `E2E_BADGE_${RUN.toUpperCase()}`,
    });
    const title = `[E2E]徽标审批单-${RUN}`;
    try {
      await api.saveFlowGraph(
        def.version_id,
        [
          { key: 'n_start', name: '开始', type: 'START', x: 80, y: 140 },
          {
            key: 'n_approve',
            name: '徽标审批',
            type: 'APPROVAL',
            x: 300,
            y: 120,
            assignee_type: 'INITIATOR',
            self_approval: 'ALLOW',
          },
          { key: 'n_end', name: '结束', type: 'END', x: 600, y: 140 },
        ],
        [
          { key: 'e1', from: 'n_start', to: 'n_approve' },
          { key: 'e2', from: 'n_approve', to: 'n_end' },
        ],
      );
      await api.publishFlowDef(def.id);
      const inst = await api.createInstance(def.id, title);

      await expect.poll(async () => (await badges(api)).flow_todo, { timeout: 15_000 }).toBe(before.flow_todo + 1);

      // 待办里找到这条任务并同意
      const todoRes = await api.ctx.get('/api/business/flow/task/todo?page=1&page_size=100', { headers: headers() });
      const todoBody = await todoRes.json();
      const item = (todoBody.data.list as Array<{ id: string; title?: string; instance_title?: string }>).find(
        (t) => (t.title ?? t.instance_title ?? '').includes(title),
      );
      expect(item, `待办里应能找到「${title}」`).toBeTruthy();
      const approve = await api.ctx.post(`/api/business/flow/task/${item!.id}/approve`, {
        headers: headers(),
        data: { opinion: 'E2E 徽标联动：同意' },
      });
      expect(approve.status(), '审批通过状态码').toBe(200);

      await expect.poll(async () => (await badges(api)).flow_todo, { timeout: 20_000 }).toBe(before.flow_todo);

      // 收尾：撤回在跑的实例（已全部通过则无需撤回），再删流程定义
      const detail = await api.getInstance(inst.instance_id).catch(() => null);
      if (detail?.instance?.status && ['RUNNING', 'PENDING'].includes(detail.instance.status)) {
        await api.withdrawInstance(inst.instance_id).catch(() => undefined);
      }
    } finally {
      await api.deleteFlowDef(def.id).catch(() => undefined);
      await api.dispose();
    }
  });
});
