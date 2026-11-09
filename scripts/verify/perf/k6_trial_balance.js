// =============================================================================
// N-8 · 试算平衡（GET /business/erp/ledger/trial-balance）—— 全表聚合
//
// 这条是「数据量上去最先爆」的一类：`opening` 与 `current` 两个 CTE 都要
// 把 biz_voucher_entry 与 biz_voucher 全表 join 后按科目聚合，
// **代价与分录行数成正比，没有索引能把它变成常数时间**。
//
// 10 万行实测（145 cloudpivot_perf，2026-11）：
//   10 万分录 / 3 千凭证 ：69.0 ms（HTTP 中位）
//   3 万分录 / 1 万凭证   ：14.5 ms
//   3 千分录 / 1 千凭证   ：6.4 ms
//   → 约 **线性**（每 10 万分录 ≈ +55 ms）。阈值按 3 并发留 5 倍余量：p95 < 400 ms。
//
// 运行：
//   set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
//   K6_BASE_URL=http://127.0.0.1:18099 K6_PERIOD=2026-11 k6 run scripts/verify/perf/k6_trial_balance.js
// =============================================================================

import { login, getOk } from './k6_lib.js';

export const options = {
  scenarios: {
    trial_balance: {
      // 聚合查询本身就是重活：并发压 3 台即可（再多只是排队，测不出新信息）
      executor: 'constant-vus',
      vus: Number(__ENV.K6_VUS || 3),
      duration: __ENV.K6_DURATION || '20s',
      gracefulStop: '10s',
    },
  },
  thresholds: {
    'http_req_duration{endpoint:trial_balance}': ['p(95)<400', 'p(99)<1000'],
    'http_req_failed{endpoint:trial_balance}': ['rate<0.001'],
    checks: ['rate>0.99'],
  },
};

export function setup() {
  return { token: login() };
}

export default function (data) {
  const period = __ENV.K6_PERIOD || '2026-11';
  getOk(data.token, `/api/business/erp/ledger/trial-balance?period=${period}`, 'trial_balance');
}
