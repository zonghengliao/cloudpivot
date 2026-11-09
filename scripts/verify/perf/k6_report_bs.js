// =============================================================================
// N-8 · 资产负债表（GET /business/erp/report/balance-sheet）—— 最重的一条
//
// 一次请求里跑了 6 段 SQL（REPEATABLE READ 快照内）：
//   ① load_items     报表项配置（54 行）
//   ② load_opening_totals  期初合计
//   ③ load_accounts  两个全表聚合子查询（本期 + 期初，各自 join 凭证）
//   ④ load_ledger_totals   本期分录借贷合计（又一次全表聚合）
//   ⑤ load_unbalanced_vouchers 不平凭证扫描（再 join 一次）
//   ⑥ 上期对比：期间行存在时把 ③ 再跑一遍
// → 它是「单请求里全表聚合次数最多」的接口，容量规划要按它算。
//
// 10 万行实测（145 cloudpivot_perf，2026-11，单请求中位）：
//   1 万凭证 / 3 万分录  ：34.0 ms
//   10 万凭证 / 30 万分录：176.8 ms（SQL 侧 load_accounts 76 ms + 合计/不平 43 ms + 其余）
//   → 阈值 p95 < 800 ms（2 并发）
//
// 运行：
//   set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
//   K6_BASE_URL=http://127.0.0.1:18099 K6_PERIOD=2026-11 k6 run scripts/verify/perf/k6_report_bs.js
// =============================================================================

import { login, getOk } from './k6_lib.js';

export const options = {
  scenarios: {
    report_bs: {
      executor: 'constant-vus',
      // 报表是「打开一次看半天」的页面，2 并发已经高于真实并发
      vus: Number(__ENV.K6_VUS || 2),
      duration: __ENV.K6_DURATION || '20s',
      gracefulStop: '15s',
    },
  },
  thresholds: {
    'http_req_duration{endpoint:report_bs}': ['p(95)<800', 'p(99)<2000'],
    'http_req_failed{endpoint:report_bs}': ['rate<0.001'],
    checks: ['rate>0.99'],
  },
};

export function setup() {
  return { token: login() };
}

export default function (data) {
  const period = __ENV.K6_PERIOD || '2026-11';
  getOk(data.token, `/api/business/erp/report/balance-sheet?period=${period}`, 'report_bs');
}
