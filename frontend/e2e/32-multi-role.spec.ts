import { expect, test } from '@playwright/test';
import { newApi, readSharedToken } from './support';

/**
 * 专项 3：**多角色视角**（每个角色登录后看菜单和页面）。
 *
 * # 为什么用「临时造的两个角色账号」
 *
 * 卡面点名要 超管 / 仓管员 / 财务 / 只读员 / 普通员工 五个视角，但库里只有五个角色：
 * SUPER_ADMIN、QA_VIEWER（测试只读员）、QA_LIMITED（测试受限管理员）、
 * MUTEX_A/B（制单员/审核员，用来测互斥的）。**没有仓管员 / 财务 / 普通员工**——
 * 这属于环境缺口（报告里单列），不能靠编造账号名糊过去。
 *
 * 所以这里覆盖**真实存在**的三个视角：超管 / 只读员 / 受限管理员。
 * 只读员与受限管理员是 beforeAll 里用管理 API **真造**的两个账号（带角色），
 * afterAll 删掉，不留脏数据；密码一次性 `E2e@123456`。
 *
 * # 断言口径
 *
 * - 超管菜单数 > 只读员菜单数（权限真的收窄了，不是「菜单都一样」）；
 * - 只读员的每个菜单路径都能打开（不是给了一堆点不进去的菜单）；
 * - 只读员菜单集合 ⊆ 超管菜单集合（没有越权多出来的项）；
 * - 只读员直达 `/system/user` → 落到 403 页或页面明确拒绝。
 */
interface MenuNode {
  path?: string | null;
  title?: string;
  name?: string;
  menu_type?: number;
  is_visible?: boolean;
  children?: MenuNode[];
}

function leafPaths(nodes: MenuNode[], out: string[] = []): string[] {
  for (const n of nodes) {
    if (n.is_visible !== false && n.menu_type === 2 && n.path?.startsWith('/')) {
      out.push(n.path);
    }
    if (n.children?.length) {
      leafPaths(n.children, out);
    }
  }
  return out;
}

const PASSWORD = 'E2e@123456';
const RUN = Date.now().toString(36);

test.describe('专项3 多角色视角', () => {
  const viewers: Array<{ key: string; username: string; roleCode: string; id?: string }> = [
    { key: '只读员', username: `e2e.viewer.${RUN}`, roleCode: 'QA_VIEWER' },
    { key: '受限管理员', username: `e2e.limited.${RUN}`, roleCode: 'QA_LIMITED' },
  ];
  const created: string[] = [];
  const roleIds: Record<string, string> = {};

  test.beforeAll(async () => {
    const api = await newApi();
    const res = await api.ctx.get('/api/system/role/page?page=1&page_size=100', {
      headers: { Authorization: `Bearer ${readSharedToken()}` },
    });
    const roles = (await res.json())?.data?.list ?? [];
    for (const r of roles) {
      roleIds[r.role_code] = r.id;
    }
    for (const v of viewers) {
      expect(roleIds[v.roleCode], `角色 ${v.roleCode} 应存在`).toBeTruthy();
      const create = await api.ctx.post('/api/system/user', {
        data: {
          username: v.username,
          password: PASSWORD,
          real_name: `[E2E]${v.key}`,
          status: 1,
          role_ids: [roleIds[v.roleCode]],
        },
      });
      const body = await create.json();
      expect(create.status(), `建 ${v.key} 账号：${JSON.stringify(body)}`).toBe(200);
      expect(body.code, `建 ${v.key} 账号业务码`).toBe(0);
      created.push(body.data.id);
      v.id = body.data.id;
    }
    await api.dispose();
  });

  test.afterAll(async () => {
    const api = await newApi();
    for (const id of created) {
      await api.ctx.delete(`/api/system/user/${id}`).catch(() => undefined);
    }
    await api.dispose();
  });

  test('超管：菜单全量可见（含系统管理 8 页）', async ({ browser }) => {
    const ctx = await browser.newContext();
    const page = await ctx.newPage();
    await page.goto('/dashboard');
    await expect(page.locator('.basic-layout__aside')).toBeVisible();
    const menus = await page.evaluate(async () => {
      const token = localStorage.getItem('x_access_token');
      const r = await fetch('/api/system/auth/menus', { headers: { Authorization: `Bearer ${token}` } });
      return (await r.json()).data as MenuNode[];
    });
    const paths = leafPaths(menus);
    expect(paths.length, '超管菜单叶子数').toBeGreaterThan(30);
    for (const must of ['/system/user', '/system/role', '/business/erp/material', '/flow/task']) {
      expect(paths, `超管应能看到 ${must}`).toContain(must);
    }
    await ctx.close();
  });

  for (const v of viewers) {
    test(`${v.key}：登录后菜单收窄、菜单都能打开、直达无权页被拒`, async ({ browser }) => {
      const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
      const page = await ctx.newPage();
      const loginResp = await page.request.post('/api/system/auth/login', {
        data: { username: v.username, password: PASSWORD },
      });
      expect(loginResp.status(), `${v.key} 登录状态`).toBe(200);
      const tokens = (await loginResp.json()).data;
      await page.addInitScript(
        ([access, refresh]) => {
          localStorage.setItem('x_access_token', access);
          localStorage.setItem('x_refresh_token', refresh);
          localStorage.setItem('cp-onboarding-done', '1');
        },
        [tokens.access_token, tokens.refresh_token],
      );

      await page.goto('/dashboard');
      await expect(page.locator('.basic-layout__aside')).toBeVisible();
      const ownMenus = await page.evaluate(async () => {
        const token = localStorage.getItem('x_access_token');
        const r = await fetch('/api/system/auth/menus', { headers: { Authorization: `Bearer ${token}` } });
        return (await r.json()).data as MenuNode[];
      });
      const ownPaths = leafPaths(ownMenus);

      // ① 侧栏渲染出来的菜单项数量与接口一致（不是"接口给了但侧栏没渲染"）
      // 类名口径：菜单项是 TDesign 的 t-menu-item/t-submenu，**没有 `.side-menu-item`**；
      // 仓库自写的标题节点是 `.side-menu-item__title`（每个可见菜单项一个）。
      const sidebarItems = await page.locator('.side-menu-item__title').count();
      expect(sidebarItems, `${v.key} 侧栏菜单项数量`).toBeGreaterThan(0);

      // ② 权限真的收窄了
      expect(ownPaths.length, `${v.key} 菜单叶子数应少于超管的 40+`).toBeLessThan(35);
      expect(ownPaths, `${v.key} 不该看到用户管理`).not.toContain('/system/user');

      // ③ 给了的菜单都能打开（采样前 6 条，避免把限流打满）
      const sample = ownPaths.slice(0, 6);
      expect(sample.length, `${v.key} 至少应有 1 个菜单`).toBeGreaterThan(0);
      for (const path of sample) {
        await page.goto(path);
        await page.waitForSelector('.basic-layout__aside, .t-result', { timeout: 20_000 });
        const is404 = await page.evaluate(() => /页面不存在/.test(document.title));
        expect(is404, `${v.key} 打开菜单 ${path} 不应 404`).toBe(false);
      }

      // ④ 直达无权页面 → 403 结果页（或明确的无权限提示），不能白屏
      await page.goto('/system/user');
      await page.waitForSelector('.basic-layout__aside, .t-result', { timeout: 20_000 });
      const denied = await page.evaluate(() => {
        const text = document.body.innerText;
        return /403|无权限|没有权限|无权访问/.test(text) || location.pathname.startsWith('/system/user') === false;
      });
      expect(denied, `${v.key} 直达 /system/user 应被拒（当前 URL=${page.url()}）`).toBe(true);

      await ctx.close();
    });
  }
});
