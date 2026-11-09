// =============================================================================
// N-8 · 登录（POST /api/system/auth/login）—— argon2 口令校验开销
//
// 登录的开销**不随数据量变化**（它是一次 argon2 校验 + 几条按用户名索引的查询），
// 所以它是「CPU 容量」而不是「数据量」的基线：
//
//   145 实测（4 核）：冷启动后单次 264.5 ms；单核上限 ≈ 3.8 次/秒，
//   4 核理论 ≈ 15 次/秒。并发登录会直接把 4 个核占满，其余接口跟着排队。
//
// # 为什么这个脚本只跑 8 次登录（vus=4 × iterations=2）
//
//   登录接口有「用户名 + IP 10 次/60 秒」限流（shared/constants.rs
//   LOGIN_RATE_MAX_ATTEMPTS / LOGIN_RATE_WINDOW_SECS）。这是**产品行为**，
//   不是缺陷：超过 10 次就是 429。所以：
//     · 本脚本刻意把总量压在 10 次以内（8 次 + setup 1 次 = 9 次），
//       用「不出现 429」当阈值 —— 一旦 429 出现，说明限流窗口被别的流量占过，
//       这次测量无效，应该重跑；
//     · 要压「登录吞吐」请另开一份限流放宽的配置，别把 429 混进 p95
//       （429 是 1~2 ms 返回的，混进来会把 p95 拉低成假象 —— 本卡在 1 万行那轮
//        就踩过这个坑，见 docs/运维规程-性能基线.md §5.3）。
//
// 运行：
//   set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
//   K6_BASE_URL=http://127.0.0.1:18099 k6 run scripts/verify/perf/k6_login.js
// =============================================================================

import http from 'k6/http';
import { check } from 'k6';
import { BASE } from './k6_lib.js';

const USER = __ENV.K6_ADMIN_USER || 'admin';
const PASS = __ENV.K6_ADMIN_PASSWORD || '';

export const options = {
  scenarios: {
    login: {
      executor: 'per-vu-iterations',
      vus: Number(__ENV.K6_VUS || 4),
      iterations: Number(__ENV.K6_LOGIN_ITERATIONS || 2),
      maxDuration: '60s',
      gracefulStop: '5s',
    },
  },
  thresholds: {
    // argon2 单次 ~265 ms：p95 卡 800 ms，出现 429 立刻由 http_req_failed 抓到
    'http_req_duration{endpoint:login}': ['p(95)<800', 'p(99)<1500'],
    'http_req_failed{endpoint:login}': ['rate<0.001'],
    checks: ['rate>0.99'],
  },
};

export default function () {
  if (!PASS) {
    throw new Error('缺少 K6_ADMIN_PASSWORD（先 `set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a`）');
  }
  const res = http.post(
    `${BASE}/api/system/auth/login`,
    JSON.stringify({ username: USER, password: PASS }),
    { headers: { 'Content-Type': 'application/json' }, tags: { endpoint: 'login' } },
  );
  check(res, {
    '登录 200': (r) => r.status === 200,
    '带 access_token': (r) => {
      try {
        return !!r.json('data.access_token');
      } catch (_e) {
        return false;
      }
    },
  });
}
