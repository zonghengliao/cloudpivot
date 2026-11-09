// =============================================================================
// N-8 · 凭证列表 + 凭证详情（GET /business/erp/voucher/page、/voucher/{id}）
//
// 两个接口放在一个脚本里按 8:2 混合：列表是高频入口，详情是点进去才发的请求。
// 详情的主键**从列表响应里取**（不写死 id：夹具库换库/清数据后脚本不该失效）。
//
// 10 万行实测（145 cloudpivot_perf，单请求中位）：
//   列表 第 1 页 8.3 ms / 第 100 页 9.0 ms / 第 1000 页 22.1 ms（V0369 索引后）
//   详情 3.1 ms
//   → 阈值 p95 < 150 ms
//
// 运行：
//   set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
//   K6_BASE_URL=http://127.0.0.1:18099 k6 run scripts/verify/perf/k6_voucher_list.js
// =============================================================================

import { check } from 'k6';
import { login, getOk, vus, duration, pick } from './k6_lib.js';

const PAGES = [1, 2, 5, 10];
const PAGE_SIZE = 20;

export const options = {
  scenarios: {
    voucher_list: {
      executor: 'constant-vus',
      vus: vus(),
      duration: duration(),
      gracefulStop: '5s',
    },
  },
  thresholds: {
    'http_req_duration{endpoint:voucher_list}': ['p(95)<150', 'p(99)<400'],
    'http_req_duration{endpoint:voucher_detail}': ['p(95)<150', 'p(99)<400'],
    'http_req_failed{endpoint:voucher_list}': ['rate<0.001'],
    'http_req_failed{endpoint:voucher_detail}': ['rate<0.001'],
    checks: ['rate>0.99'],
  },
};

export function setup() {
  return { token: login() };
}

export default function (data) {
  const page = pick(PAGES);
  const res = getOk(
    data.token,
    `/api/business/erp/voucher/page?page=${page}&page_size=${PAGE_SIZE}`,
    'voucher_list',
  );
  // 每 5 次迭代抽一次详情（比例接近真实操作：翻页多、点进详情少）
  if (__ITER % 5 === 0) {
    let id = null;
    try {
      const list = res.json('data.list') || [];
      if (list.length > 0) {
        id = list[0].id;
      }
    } catch (_e) {
      id = null;
    }
    check(res, { 'voucher_list: 拿到可用的凭证主键': () => !!id });
    if (id) {
      getOk(data.token, `/api/business/erp/voucher/${id}`, 'voucher_detail');
    }
  }
}
