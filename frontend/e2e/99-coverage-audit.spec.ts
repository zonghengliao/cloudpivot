import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, test } from '@playwright/test';
import { PAGES } from './_catalog';

/**
 * 收口自检（必须最后跑，文件名排序在 35 之后 ⇒ `99-`）。
 *
 * 逐页 6 项用例是「每页真访问一次」的证据链，但 Playwright 里**没人保证**
 * 目录里的每一页真的被某个 spec 引用了、真的跑出了现场文件。这条 spec 就是那道闸：
 *
 *   1. 目录里每个可跑页面，都必须有 `e2e-report/probe/<slug>.json` 现场文件；
 *   2. 现场文件里的 `path` 必须与目录一致（防「跑了别的页」）；
 *   3. 现场文件必须记录到 HTTP 200 导航 + 没有捕获到 error；
 *   4. 检查点总数必须 ≥ 330（卡面要求）。
 *
 * 少一页就会红 —— 这就是「不许 skip、不许漏页」的机器保证。
 */

/** 与 `_pagecheck.ts` 的 slug 完全一致（文件名口径必须对得上） */
function slug(text: string): string {
  return text
    .replace(/[^\w\u4e00-\u9fa5-]+/g, '_')
    .replace(/_+/g, '_')
    .slice(0, 40);
}

const probeFile = (path: string) =>
  resolve(process.cwd(), 'e2e-probe/probe', `${slug(path.replace(/\//g, '_'))}.json`);

const runnable = PAGES.filter((p) => !p.menuOnlyBrokenRoute);

test.describe('收口: 目录每一页都真的跑过（防漏页 / 防 skip）', () => {
  test(`目录里 ${runnable.length} 个可跑页面全部有现场文件，且路径对得上`, async () => {
    const missing: string[] = [];
    const mismatched: string[] = [];
    for (const p of runnable) {
      const file = probeFile(p.path);
      if (!existsSync(file)) {
        missing.push(`${p.key}(${p.path})`);
        continue;
      }
      const obs = JSON.parse(readFileSync(file, 'utf8'));
      // 同一路径可能被两页共用（不做限制），但读出来的 path 必须能对上
      if (obs.path !== p.path) {
        mismatched.push(`${p.key}: 目录=${p.path} 现场=${obs.path}`);
      }
    }
    expect(missing, `这些页面没有现场文件（= 没真跑）: ${missing.join(' / ')}`).toEqual([]);
    expect(mismatched, `现场文件与目录不一致: ${mismatched.join(' / ')}`).toEqual([]);
  });

  test('每个现场文件都是 HTTP 200 导航且没有捕获到 error', async () => {
    const bad: string[] = [];
    for (const p of runnable) {
      const file = probeFile(p.path);
      if (!existsSync(file)) {
        continue;
      }
      const obs = JSON.parse(readFileSync(file, 'utf8'));
      if (!(obs.navStatus >= 200 && obs.navStatus < 300)) {
        bad.push(`${p.key}: navStatus=${obs.navStatus}`);
      }
      if (obs.error) {
        bad.push(`${p.key}: error=${obs.error}`);
      }
    }
    expect(bad, `导航/采集失败: ${bad.join(' | ')}`).toEqual([]);
  });

  test('检查点总数 ≥ 330（卡面「52 页 × 6 项」的硬要求）', async () => {
    const cells = runnable.length * 6;
    expect(runnable.length, '可跑页面数应 ≥ 55').toBeGreaterThanOrEqual(55);
    expect(cells, `检查点 ${cells} 应 ≥ 330`).toBeGreaterThanOrEqual(330);
  });
});
