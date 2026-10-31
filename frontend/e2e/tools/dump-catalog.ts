/**
 * 把 e2e/_catalog.ts 的页面元数据（formMode / readOnly / note / listApi / kind）导出成
 * `e2e-report/catalog.json`，供 `tools/e2e_report.py` 生成报告时对齐（探针 JSON 只记现场，
 * 不重复记这些静态口径）。
 *
 * 运行：cd frontend && npx tsx e2e/tools/dump-catalog.ts
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { PAGES } from '../_catalog';

const out = resolve(process.cwd(), 'e2e-report/catalog.json');
mkdirSync(dirname(out), { recursive: true });
writeFileSync(
  out,
  JSON.stringify(
    PAGES.map((p) => ({
      key: p.key,
      path: p.path,
      kind: p.kind,
      listApi: p.listApi,
      legacyPath: p.legacyPath ?? null,
      readOnly: p.readOnly ?? false,
      formMode: p.formMode ?? 'dialog',
      knownBug: p.knownBug ?? null,
      note: p.note ?? '',
    })),
    null,
    2,
  ),
  'utf8',
);
console.log(`wrote ${out}（${PAGES.length} 页）`);
