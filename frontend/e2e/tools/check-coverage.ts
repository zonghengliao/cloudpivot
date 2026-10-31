/**
 * 自检：目录里每页都必须被某个 spec 真正跑起来（不许「目录里有、用例里没有」）。
 *
 * 运行：cd frontend && npx tsx e2e/tools/check-coverage.ts
 *
 * 例外只有一类：`menuOnlyBrokenRoute`（侧栏菜单有、router 里没路由的条目）。
 * 它们不跑 6 项检查（只会渲染 404 页），由 `30-menu-alignment.spec.ts` 断言，
 * 这里单列出来做台账，不算漏测。
 */
import { readdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { PAGES } from '../_catalog';

const dir = resolve(process.cwd(), 'e2e');
const specs: string[] = readdirSync(dir).filter((f: string) => f.endsWith('.spec.ts'));
const used = new Map<string, string[]>();

for (const f of specs) {
  const src = readFileSync(resolve(dir, f), 'utf8');
  for (const p of PAGES) {
    // 只认 spec 里出现的**字面量 key**（注释里提到也算引用，注释掉页面的会在这里露出来）
    if (src.includes(`'${p.key}'`)) {
      used.set(p.key, [...(used.get(p.key) ?? []), f]);
    }
  }
}

const runnable = PAGES.filter((p) => !p.menuOnlyBrokenRoute);
const menuOnly = PAGES.filter((p) => p.menuOnlyBrokenRoute);
const missing = runnable.filter((p) => !used.has(p.key));

console.log(`目录 ${PAGES.length} 页 = 逐页 6 项 ${runnable.length} 页 + 菜单有/路由无 ${menuOnly.length} 条`);
console.log(`被 spec 引用 ${used.size} 页 / 漏 ${missing.length} 页`);
for (const p of menuOnly) {
  console.log(`  · 菜单有 / 路由无（专项 1 断言）: ${p.key} (${p.path})`);
}
for (const p of missing) {
  console.log(`  ✗ 未覆盖: ${p.key} (${p.path})`);
}
if (missing.length > 0) {
  process.exitCode = 1;
}
