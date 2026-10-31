/**
 * `e2e/target-guard.mjs` 判据单测（`node --test`，零新依赖）
 *
 * 这些断言是「E2E 不许打 prod」这条红线的**回归网**：判据本身一旦被改松
 * （比如有人把 `172.18.10.144` 从 KNOWN_SHARED 里删掉、或把 5173 放回隔离段），
 * 这里立刻红，不用等下一次有人在生产上造完数才发现。
 *
 * 背景：2026-10-31 11:34:11 prod 被写进 10 个零成本序列号夹具，
 * `erp_invariants.py` B1（BLOCKER）+ C3（WARN）变红；取证见
 * `docs/e2e-prod-fixture-incident-20261031-172.18.10.7.md`。
 *
 * 运行：`node --test tests/unit/target-guard.test.ts`（pnpm test:unit 已包含）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { classifyTarget, assertIsolatedTarget, postrunSteps } from '../../e2e/target-guard.mjs';

/** 临时改环境变量（测完还原，避免用例之间互相污染） */
function withEnv<T>(vars: Record<string, string | undefined>, fn: () => T): T {
  const backup: Record<string, string | undefined> = {};
  for (const [k, v] of Object.entries(vars)) {
    backup[k] = process.env[k];
    if (v === undefined) {
      delete process.env[k];
    } else {
      process.env[k] = v;
    }
  }
  try {
    return fn();
  } finally {
    for (const [k, v] of Object.entries(backup)) {
      if (v === undefined) {
        delete process.env[k];
      } else {
        process.env[k] = v;
      }
    }
  }
}

test('prod（144）一律拒绝 —— 本次事故的目标', () => {
  const v = classifyTarget('http://172.18.10.144');
  assert.equal(v.isolated, false);
  assert.match(v.reason, /线上\/共享目标|prod/);
});

test('prod 后端直连端口 8023 拒绝（哪怕 host 写成回环）', () => {
  assert.equal(classifyTarget('http://127.0.0.1:8023').isolated, false);
  assert.equal(classifyTarget('http://127.0.0.1:8023').port, '8023');
});

test('dev 默认端口 5173 拒绝（vite 代理默认指 8023，容易「以为本地其实生产」）', () => {
  assert.equal(classifyTarget('http://127.0.0.1:5173').isolated, false);
});

test('事故来源机 172.18.10.7 拒绝', () => {
  const v = classifyTarget('http://172.18.10.7');
  assert.equal(v.isolated, false);
  assert.match(v.reason, /172\.18\.10\.7/);
});

test('隔离实例（回环 + 5189 前端 / 18089 后端）放行', () => {
  assert.equal(classifyTarget('http://127.0.0.1:5189').isolated, true);
  assert.equal(classifyTarget('http://127.0.0.1:18089').isolated, true);
  assert.equal(classifyTarget('http://localhost:5199').isolated, true);
});

test('回环但端口不在隔离段 → 拒绝', () => {
  assert.equal(classifyTarget('http://127.0.0.1:9999').isolated, false);
});

test('E2E_ISOLATED_HOSTS 可显式放行 CI 容器名', () => {
  withEnv({ E2E_ISOLATED_HOSTS: 'e2e-front:5189' }, () => {
    assert.equal(classifyTarget('http://e2e-front:5189').isolated, true);
  });
});

test('非法 URL → 拒绝且不抛异常', () => {
  const v = classifyTarget('not-a-url');
  assert.equal(v.isolated, false);
  assert.match(v.reason, /不是合法 URL/);
});

test('assertIsolatedTarget：打 prod 抛错，报错里带「怎么起隔离实例」', () => {
  withEnv({ E2E_ALLOW_PROD: undefined, E2E_PROD_REASON: undefined }, () => {
    assert.throws(
      () => assertIsolatedTarget('http://172.18.10.144', { logger: { log() {}, warn() {} }, writeFlag: false }),
      /拒绝执行[\s\S]*e2e_isolated_instance\.sh/,
    );
  });
});

test('escape hatch：只设 E2E_ALLOW_PROD=1 不给理由 → 仍然拒绝', () => {
  withEnv({ E2E_ALLOW_PROD: '1', E2E_PROD_REASON: '' }, () => {
    assert.throws(
      () => assertIsolatedTarget('http://172.18.10.144', { logger: { log() {}, warn() {} }, writeFlag: false }),
      /拒绝执行/,
    );
  });
});

test('escape hatch：flag + 理由都给了 → 放行，并打印 prod 后置检查清单', () => {
  withEnv({ E2E_ALLOW_PROD: '1', E2E_PROD_REASON: '线上验收（总管批准）' }, () => {
    const warnings: string[] = [];
    const verdict = assertIsolatedTarget('http://172.18.10.144', {
      logger: { log() {}, warn: (m: string) => warnings.push(m) },
      writeFlag: false,
    });
    assert.equal(verdict.isolated, false);
    assert.equal((verdict as { overridden?: boolean }).overridden, true);
    const banner = warnings.join('\n');
    assert.match(banner, /E2E_ALLOW_PROD=1 放行/);
    assert.match(banner, /e2e_fixture_residue\.py/);
    assert.match(banner, /erp_invariants\.py/);
  });
});

test('后置检查清单含 residue → backup → apply → invariants 四步', () => {
  const steps = postrunSteps().join('\n');
  for (const needle of ['e2e_fixture_residue.py', '00_backup.sh', 'P10_GENERIC_APPLY=1', 'erp_invariants.py']) {
    assert.ok(steps.includes(needle), `后置清单缺 ${needle}`);
  }
});
