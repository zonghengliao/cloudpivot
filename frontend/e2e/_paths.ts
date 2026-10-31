import { resolve } from 'node:path';

/**
 * 现场产物目录（**必须**在 Playwright 的 html 报告目录之外）。
 *
 * 踩过的坑（2026-10-31）：现场文件原本写在 `e2e-report/probe/`，而 `playwright.config.ts`
 * 把 html 报告也输出到 `e2e-report/` —— **每次 `playwright test` 启动都会清空报告目录**，
 * 于是单独跑一条 spec 就把上一次全量跑出的 58 份现场文件全删了
 * （表现：`31-dead-buttons`/`99-coverage-audit` 报「每页都要有现场 JSON，实际 0 份」）。
 *
 * 所以现场产物统一放到 `e2e-probe/`：
 *   - `e2e-probe/probe/<slug>.json|png`：逐页现场（接口流水 / 按钮 / 点击反应 / 表单校验）
 *   - `e2e-probe/catalog.json`：页面目录元数据（`tools/dump-catalog.ts` 产出）
 *   - `e2e-probe/legacy-paths.json`：卡面旧路径采集（`35-legacy-paths.spec.ts` 产出）
 */
export const ARTIFACTS_DIR = resolve(process.cwd(), 'e2e-probe');
export const PROBE_DIR = resolve(ARTIFACTS_DIR, 'probe');
export const CATALOG_JSON = resolve(ARTIFACTS_DIR, 'catalog.json');
export const LEGACY_JSON = resolve(ARTIFACTS_DIR, 'legacy-paths.json');
