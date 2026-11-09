// =============================================================================
// N-8 · 任务列表（GET /business/task/page）
//
// 压的是「列表 + 可见性谓词 + 分页计数」这条最常被打开的路：
//   每次迭代随机页码（1 / 2 / 5 / 10），模拟用户翻页，而不是只打第 1 页。
//
// 10 万行实测（145 cloudpivot_perf，单请求中位）：
//   第 1 页 39.2 ms / 第 100 页 42.5 ms / 第 1000 页 90.8 ms（V0369 索引后）
//   → 阈值 p95 < 300 ms（5 并发下有 3 倍以上余量）
//
// 运行：
//   set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
//   K6_BASE_URL=http://127.0.0.1:18099 k6 run scripts/verify/perf/k6_task_list.js
//
// 退出码：k6 在任一 threshold 不达标时返回 99（CI 直接看退出码即可）。
// =============================================================================

import { check } from 'k6';
import { login, getOk, vus, duration, pick } from './k6_lib.js';

const PAGES = [1, 2, 5, 10];
const PAGE_SIZE = 20;

export const options = {
  scenarios: {
    task_list: {
      executor: 'constant-vus',
      vus: vus(),
      duration: duration(),
      gracefulStop: '5s',
    },
  },
  thresholds: {
    // 分页列表：p95 < 300 ms、p99 < 800 ms
    'http_req_duration{endpoint:task_list}': ['p(95)<300', 'p(99)<800'],
    // 不允许出现非 2xx（429 也算失败：列表接口没有限流，出现就是 bug）
    'http_req_failed{endpoint:task_list}': ['rate<0.001'],
    checks: ['rate>0.99'],
  },
};

export function setup() {
  return { token: login() };
}

export default function (data) {
  const page = pick(PAGES);
  getOk(data.token, `/api/business/task/page?page=${page}&page_size=${PAGE_SIZE}`, 'task_list');
}
