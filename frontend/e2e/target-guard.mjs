/**
 * E2E 目标硬判据（唯一实现，Playwright 与 shell 脚本共用）
 * =========================================================
 *
 * 背景：2026-10-31 11:34:11，prod（172.18.10.144）被直接写进一批
 * `POST /api/business/erp/serial/batch-generate` 夹具（M-W1A-004 / WH01 / 10 件，
 * 零成本、不写库位账），当场让 `erp_invariants.py` 的 B1（BLOCKER）+ C3（WARN）变红。
 * 事后取证：那条走查来自 172.18.10.7 的 mac 真实 Chrome（见
 * `docs/e2e-prod-fixture-incident-20261031-172.18.10.7.md`），**不是** Playwright 套件；
 * 但仓库侧的真问题同样严重 —— `playwright.config.ts` / `global-setup.ts` / `support.ts`
 * 三处都把 BASE_URL 默认成了 prod `http://172.18.10.144`，任何人一句 `pnpm test:e2e`
 * 就在生产上造数。
 *
 * 本模块把「E2E 只能打隔离实例」变成**运行期硬判据**：
 *
 *   隔离 = 目标 host 是回环地址，且端口落在隔离段
 *          - 后端直连：18000-18999（副本库 + profile=test 起的实例）
 *          - 前端代理：5100-5999（vite dev/preview 反代到上面的后端）
 *          或在 `E2E_ISOLATED_HOSTS` 里显式列出（CI 容器名等，逗号分隔）
 *
 *   其它一律**拒绝**，并把「为什么拒绝 / 怎么起隔离实例」写进异常文案。
 *
 * 业务上确实必须打 prod（线上验收）时唯一的口子：
 *   `E2E_ALLOW_PROD=1 E2E_PROD_REASON='<谁批准的、为什么>'`
 *   两个都要给，缺一个照样拒绝；放行后会在 `e2e/.auth/prod-target.flag` 留痕，
 *   `scripts/run-full-rounds.sh` / `scripts/e2e-postrun-check.sh` 见到该标记
 *   就**强制**跑 `scripts/verify/e2e_fixture_residue.py` + `erp_invariants.py`，
 *   非 0 就清（P10 流程）。判据在代码里，不靠记忆。
 *
 * 用法：
 *   代码里：import { assertIsolatedTarget } from './target-guard.mjs';
 *           assertIsolatedTarget(process.env.E2E_BASE_URL ?? DEFAULT);
 *   命令行：node e2e/target-guard.mjs --check http://172.18.10.144   # 非 0 = 拒绝
 *           node e2e/target-guard.mjs --print-postrun               # 打印 prod 后置检查清单
 */
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

/** 已知的线上/共享目标（拒绝时的原因写得更具体，便于一眼认出） */
const KNOWN_SHARED = [
  { host: '172.18.10.144', why: 'prod（144：nginx 80 → 后端 8023，线上唯一实例）' },
  { host: '172.18.10.99', why: '开发机 ubuntu-svr（多 agent 共享工作区）' },
  { host: '144', why: 'prod 简写' },
];

/** 隔离端口段：后端实例 / 前端 dev-preview */
const BACKEND_ISOLATED_RANGE = [18000, 18999];
const FRONTEND_ISOLATED_RANGE = [5100, 5999];

/**
 * 即使在回环地址上，这些端口也不算隔离：
 * - 5173 = `pnpm dev` 的默认端口，`vite.config.ts` 的 proxy 默认指向 `127.0.0.1:8023`，
 *   一旦机器上跑着 prod 后端/隧道，就是「以为本地、实际生产」；
 * - 8023 / 80 / 443 / 3000 = 各环境的正式服务端口。
 * 判据宁可误杀：确需例外就在 E2E_ISOLATED_HOSTS 里显式写 `<host>:<port>`。
 */
const NON_ISOLATED_PORTS = new Set(['5173', '8023', '80', '443', '3000', '8080']);

const PROD_POSTRUN_STEPS = [
  'set -a; . /etc/cloudpivot/cloudpivot.env; set +a',
  'python3 scripts/verify/e2e_marker.py --assert-clean   # exit 1 = 生产上还有活跃 [E2E] 标记行 / 在用测试账号',
  'python3 scripts/verify/e2e_fixture_residue.py     # exit 1 = 有人造了零成本夹具',
  'bash    scripts/datafix/p10/00_backup.sh',
  'P10_GENERIC_APPLY=1 bash scripts/datafix/p10/apply.sh',
  'python3 scripts/verify/erp_invariants.py          # 必须 BLOCKER 0 / WARN 0（含 O1 E2E 标记不变量）',
];

function inRange(port, [lo, hi]) {
  const n = Number(port);
  return Number.isInteger(n) && n >= lo && n <= hi;
}

function isLoopback(hostname) {
  const h = String(hostname).toLowerCase();
  return h === '127.0.0.1' || h === '::1' || h === '[::1]' || h === 'localhost' || h === '0.0.0.0';
}

function extraHosts() {
  return (process.env.E2E_ISOLATED_HOSTS ?? '')
    .split(',')
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean);
}

/**
 * 判定目标：返回 { isolated, url, host, port, reason }
 * 不做任何 IO，纯函数式，便于单测与 bash 侧复用。
 */
export function classifyTarget(rawUrl) {
  let u;
  try {
    u = new URL(String(rawUrl));
  } catch {
    return { isolated: false, url: String(rawUrl), host: '', port: '', reason: `不是合法 URL：${rawUrl}` };
  }
  const host = u.hostname.toLowerCase();
  const port = u.port || (u.protocol === 'https:' ? '443' : '80');

  const shared = KNOWN_SHARED.find((k) => k.host === host);
  if (shared) {
    return { isolated: false, url: String(rawUrl), host, port, reason: `命中已知线上/共享目标：${shared.why}` };
  }
  if (host === '172.18.10.7') {
    return {
      isolated: false,
      url: String(rawUrl),
      host,
      port,
      reason: '172.18.10.7 是同网段操作机（11:34 夹具的来源），不是隔离实例',
    };
  }
  if (extraHosts().includes(`${host}:${port}`) || extraHosts().includes(host)) {
    return { isolated: true, url: String(rawUrl), host, port, reason: 'E2E_ISOLATED_HOSTS 显式放行' };
  }
  if (!isLoopback(host)) {
    return {
      isolated: false,
      url: String(rawUrl),
      host,
      port,
      reason: `非回环地址且不在 E2E_ISOLATED_HOSTS：${host}（隔离实例只应起在 127.0.0.1:1800x/51xx）`,
    };
  }
  if (NON_ISOLATED_PORTS.has(String(port))) {
    return {
      isolated: false,
      url: String(rawUrl),
      host,
      port,
      reason: `端口 ${port} 是共享/默认服务端口（5173 dev 默认端口 / 8023 后端 / 80·443 线上站点），不算隔离`,
    };
  }
  if (inRange(port, BACKEND_ISOLATED_RANGE) || inRange(port, FRONTEND_ISOLATED_RANGE)) {
    return { isolated: true, url: String(rawUrl), host, port, reason: `回环 + 隔离端口 ${port}` };
  }
  return {
    isolated: false,
    url: String(rawUrl),
    host,
    port,
    reason:
      `回环地址但端口 ${port} 不在隔离段（后端 18000-18999 / 前端 5100-5999）：` +
      'dev 端口 8023/5173 或 80/443 指向共享实例，同样会污染数据',
  };
}

/** 放行 prod 的两个必要条件（都设了才算「有意为之」） */
function prodEscapeHatch() {
  const flag = process.env.E2E_ALLOW_PROD === '1';
  const reason = (process.env.E2E_PROD_REASON ?? '').trim();
  return { granted: flag && reason.length >= 8, flag, reason };
}

/** 在 `e2e/.auth/prod-target.flag` 留痕，供后置检查脚本识别 */
function markProdTarget(projectRoot, url, reason) {
  try {
    const dir = resolve(projectRoot, 'e2e/.auth');
    mkdirSync(dir, { recursive: true });
    const flag = resolve(dir, 'prod-target.flag');
    writeFileSync(flag, `target=${url}\nreason=${reason}\nat=${new Date().toISOString()}\n`);
    appendFileSync(
      resolve(projectRoot, 'e2e/.auth/prod-target.log'),
      `${new Date().toISOString()} target=${url} reason=${reason}\n`,
    );
    return flag;
  } catch {
    return null;
  }
}

/**
 * 硬判据：目标不是隔离实例就抛错（Playwright 里表现为 setup 阶段直接红，不会跑用例）。
 * 放行 prod 时打醒目的横幅，并（默认）写 `prod-target.flag` 触发后置检查。
 */
export function assertIsolatedTarget(rawUrl, options = {}) {
  const { projectRoot = process.cwd(), logger = console, writeFlag = true } = options;
  const verdict = classifyTarget(rawUrl);
  if (verdict.isolated) {
    logger.log(`[e2e-target] OK 隔离实例：${verdict.url}（${verdict.reason}）`);
    return verdict;
  }

  const escape = prodEscapeHatch();
  if (escape.granted) {
    const flagPath = writeFlag ? markProdTarget(projectRoot, verdict.url, escape.reason) : null;
    logger.warn(
      `\n!! [e2e-target] 已按 E2E_ALLOW_PROD=1 放行非隔离目标：${verdict.url}\n` +
        `!! 原因：${escape.reason}\n` +
        `!! 判据原始结论：${verdict.reason}\n` +
        `!! 跑完必须执行（缺一步都不算完）：\n` +
        PROD_POSTRUN_STEPS.map((s) => `!!   ${s}`).join('\n') +
        (flagPath ? `\n!! 已留痕：${flagPath}（postrun 检查据此强制运行）` : '') +
        '\n',
    );
    return { ...verdict, overridden: true };
  }

  const hint =
    '\nE2E 不允许直连非隔离目标（2026-10-31 11:34 的生产夹具事故就是这类直连造成的）。\n' +
    '  正确做法：bash scripts/verify/e2e_isolated_instance.sh   # 起副本库实例，打印 E2E_BASE_URL\n' +
    '  或显式指过去：E2E_BASE_URL=http://127.0.0.1:5189 pnpm test:e2e\n' +
    '  业务上必须打 prod（线上验收）时：E2E_ALLOW_PROD=1 E2E_PROD_REASON="<谁批准/为什么>" pnpm test:e2e\n' +
    '  跑完照做：' +
    PROD_POSTRUN_STEPS.map((s) => `\n    ${s}`).join('');
  throw new Error(`[e2e-target] 拒绝执行：${verdict.reason}\n  目标 = ${verdict.url}${hint}`);
}

/** 后置检查清单（供 shell 打印 / 文档引用） */
export function postrunSteps() {
  return [...PROD_POSTRUN_STEPS];
}

// ---------------------------------------------------------------------------
// CLI：node e2e/target-guard.mjs --check <url> | --print-postrun
// 退出码：0 = 放行（含 escape hatch），1 = 拒绝
// ---------------------------------------------------------------------------
const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]).endsWith('target-guard.mjs');
if (invokedDirectly) {
  const args = process.argv.slice(2);
  if (args[0] === '--print-postrun') {
    for (const line of PROD_POSTRUN_STEPS) {
      console.log(line);
    }
    process.exit(0);
  }
  if (args[0] === '--check') {
    const target = args[1] ?? '';
    const verdict = classifyTarget(target);
    if (verdict.isolated) {
      console.log(`OK ${verdict.url} — ${verdict.reason}`);
      process.exit(0);
    }
    const escape = prodEscapeHatch();
    if (escape.granted) {
      console.warn(`WARN 放行（E2E_ALLOW_PROD=1）：${verdict.url} — ${verdict.reason}`);
      console.warn('跑完必须执行：');
      for (const line of PROD_POSTRUN_STEPS) {
        console.warn(`  ${line}`);
      }
      process.exit(0);
    }
    console.error(`REFUSED ${verdict.url} — ${verdict.reason}`);
    console.error('起隔离实例：bash scripts/verify/e2e_isolated_instance.sh');
    console.error('确需打 prod：E2E_ALLOW_PROD=1 E2E_PROD_REASON="<谁批准/为什么>"');
    process.exit(1);
  }
  console.error('用法: node e2e/target-guard.mjs --check <url> | --print-postrun');
  process.exit(2);
}
