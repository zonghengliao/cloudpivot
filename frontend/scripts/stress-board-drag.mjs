/**
 * 现象1 复现探针：在真实生产站点上反复「建卡 → 看板拖 TODO→DOING」，全程记录
 * 网络 / console / DOM 时间线，用来回答两个问题：
 *   (a) PATCH 200 之后，卡片是否短暂出现在 DOING（= 乐观更新成功过，随后被 catch 回滚）
 *   (b) 若回滚，catch 分支吐出的 toast 文案是什么（ApiError 还是 TypeError 兜底文案）
 *
 * 用法（在 frontend/ 下）：node scripts/stress-board-drag.mjs [轮数] [每轮拖动前随机等待毫秒上限]
 * 结果：追加写 e2e-stress/out/*.jsonl，每轮一行（含 console / 接口 / DOM 时间线）
 */
import { chromium, request } from '@playwright/test';
import { appendFileSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

const BASE_URL = process.env.E2E_BASE_URL ?? 'http://172.18.10.144';
const ACCOUNT = { username: 'laoliu_test', password: 'Test@12345' };
const ROUNDS = Number(process.argv[2] ?? 8);
const JITTER_MAX = Number(process.argv[3] ?? 1500);
const stamp = `e2e${Date.now().toString(36)}`;

const OUT_DIR = resolve(process.cwd(), 'e2e-stress/out');
mkdirSync(OUT_DIR, { recursive: true });
const OUT = resolve(OUT_DIR, `stress-${stamp}.jsonl`);

async function token() {
  const ctx = await request.newContext({ baseURL: BASE_URL });
  const res = await ctx.post('/api/system/auth/login', { data: ACCOUNT });
  const body = await res.json();
  if (res.status() !== 200 || body?.code !== 0) throw new Error(`登录失败 ${res.status()} ${JSON.stringify(body)}`);
  await ctx.dispose();
  return body.data;
}

async function api(tok) {
  const ctx = await request.newContext({ baseURL: BASE_URL, extraHTTPHeaders: { Authorization: `Bearer ${tok}` } });
  return {
    createTask: async (title) => {
      const r = await ctx.post('/api/business/task', { data: { title, priority: 'P2' } });
      const b = await r.json();
      if (r.status() !== 200 || b.code !== 0) throw new Error(`建卡失败 ${r.status()} ${JSON.stringify(b)}`);
      return b.data.id;
    },
    deleteTask: (id) => ctx.delete(`/api/business/task/${id}`),
    dispose: () => ctx.dispose(),
  };
}

const centerOf = async (loc) => {
  const b = await loc.boundingBox();
  if (!b) throw new Error('none');
  return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
};
const dropPoint = async (loc, offsetY = 80) => {
  const b = await loc.boundingBox();
  if (!b) throw new Error('落点不可见');
  return { x: b.x + b.width / 2, y: b.y + offsetY };
};

/** 采样一次看板 DOM 状态 */
const SAMPLE = `(() => {
  const cards = (st) => Array.from(document.querySelectorAll('.board .col[data-status="' + st + '"] .board-card'))
    .map(e => (e.querySelector('.board-card__title')?.textContent || '').trim());
  const cnt = (st) => (document.querySelector('.board .col[data-status="' + st + '"] .col__count')?.textContent || '').trim();
  return {
    todo: cards('TODO'), doing: cards('DOING'),
    todoCount: cnt('TODO'), doingCount: cnt('DOING'),
    toasts: Array.from(document.querySelectorAll('.t-message')).map(e => (e.innerText || '').replace(/\\s+/g, ' ').trim()),
    alerts: Array.from(document.querySelectorAll('.board .col .t-alert')).map(e => e.innerText.trim()),
  };
})()`;

const main = async () => {
  const tk = await token();
  const client = await api(tk.access_token);
  const browser = await chromium.launch();
  const state = {
    cookies: [],
    origins: [
      {
        origin: BASE_URL,
        localStorage: [
          { name: 'x_access_token', value: tk.access_token },
          { name: 'x_refresh_token', value: tk.refresh_token },
          { name: 'cp-onboarding-done', value: '1' },
        ],
      },
    ],
  };

  const summary = [];
  for (let i = 1; i <= ROUNDS; i += 1) {
    const t0 = Date.now();
    const rel = () => Date.now() - t0;
    const title = `[E2E]拖拽-正常-${stamp}r${i}`;
    const id = await client.createTask(title);
    const ctx = await browser.newContext({ baseURL: BASE_URL, storageState: state, viewport: { width: 1600, height: 1000 }, locale: 'zh-CN' });
    const page = await ctx.newPage();
    const rec = { round: i, title, id, console: [], pageerror: [], net: [], samples: [], verdict: '' };
    page.on('console', (m) => {
      const text = m.text();
      if (m.type() === 'error' || m.type() === 'warning') rec.console.push({ t: rel(), type: m.type(), text: text.slice(0, 600) });
    });
    page.on('pageerror', (e) => rec.pageerror.push({ t: rel(), text: String(e).slice(0, 800) }));
    page.on('response', (r) => {
      const u = r.url();
      if (!u.includes('/api/business/task')) return;
      rec.net.push({ t: rel(), method: r.request().method(), path: u.replace(BASE_URL, ''), status: r.status() });
    });
    page.on('requestfailed', (r) => {
      if (!r.url().includes('/api/')) return;
      rec.net.push({ t: rel(), method: r.method(), path: r.url().replace(BASE_URL, ''), failed: r.failure()?.errorText });
    });

    let sampleTimer = null;
    let last = '';
    try {
      await page.goto('/task?view=board', { timeout: 30000 });
      const col = (s) => page.locator(`.board .col[data-status="${s}"]`);
      const card = col('TODO').locator('.board-card').filter({ hasText: title }).first();
      await card.waitFor({ state: 'visible', timeout: 25000 });
      const todoBefore = Number(await col('TODO').locator('.col__count').innerText());
      const doingBefore = Number(await col('DOING').locator('.col__count').innerText());
      rec.before = { todoBefore, doingBefore };

      // 随机抖动，覆盖「加载完 0.2s~2s 后开拖」这一段（12:20 那次是 ~1.0s）
      const jitter = Math.floor(Math.random() * JITTER_MAX);
      await page.waitForTimeout(jitter);
      rec.jitter = jitter;

      // 开始采样（拖之前就开，抓住每一个 DOM 变化）
      const pollOnce = async () => {
        const s = await page.evaluate(SAMPLE).catch(() => null);
        if (!s) return;
        const key = JSON.stringify([s.todo.length, s.doing.length, s.toasts]);
        if (key !== last) {
          last = key;
          rec.samples.push({ t: rel(), ...s });
        }
      };
      sampleTimer = setInterval(pollOnce, 150);
      rec.dragAt = rel();
      const start = await centerOf(card);
      const to = await dropPoint(col('DOING'));
      await page.mouse.move(start.x, start.y);
      await page.mouse.down();
      await page.mouse.move(to.x, to.y, { steps: 25 });
      await page.waitForTimeout(150);
      await page.mouse.move(to.x + 2, to.y + 2, { steps: 3 });
      await page.mouse.up();

      // 观察 12s：e2e 的断言是 20s，这里取前 12s 足够判断「回滚 vs 未渲染」
      const deadline = Date.now() + 12000;
      let landedAt = null;
      while (Date.now() < deadline) {
        await page.waitForTimeout(200);
        const s = await page.evaluate(SAMPLE).catch(() => null);
        if (!s) continue;
        const inDoing = s.doing.some((t) => t.includes(title));
        if (inDoing && landedAt === null) landedAt = rel();
        if (landedAt !== null && !inDoing) break; // 出现了又消失 = 回滚，立刻停
      }
      const finalS = await page.evaluate(SAMPLE).catch(() => null);
      rec.landedAt = landedAt;
      rec.final = finalS;
      if (landedAt === null) rec.verdict = 'RED-未进DOING';
      else if (finalS && !finalS.doing.some((t) => t.includes(title))) rec.verdict = 'RED-进了又被回滚';
      else rec.verdict = 'GREEN';
    } catch (e) {
      rec.verdict = `ERROR ${String(e).slice(0, 300)}`;
    } finally {
      if (sampleTimer) clearInterval(sampleTimer);
      await ctx.close().catch(() => undefined);
      await client.deleteTask(id).catch(() => undefined);
    }
    appendFileSync(OUT, `${JSON.stringify(rec)}\n`);
    summary.push(`${rec.round}\t${rec.verdict}\tlandedAt=${rec.landedAt}\tjitter=${rec.jitter}\tsamples=${rec.samples.length}\tconsoleErr=${rec.console.length}\tpageerror=${rec.pageerror.length}`);
    console.log(summary[summary.length - 1]);
  }
  await browser.close();
  await client.dispose();
  console.log(`\n=== 汇总 ${ROUNDS} 轮 ===`);
  const red = summary.filter((s) => !s.includes('GREEN')).length;
  console.log(summary.join('\n'));
  console.log(`\nRED/ERROR = ${red} / ${ROUNDS}\n结果文件：${OUT}`);
};

main().catch((e) => {
  console.error('FATAL', e);
  process.exit(1);
});
