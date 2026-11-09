// =============================================================================
// N-8 性能基线 · k6 公共库
//
// 约定（5 个脚本共用，改动只在这里改一次）：
//   · 地址/账号/期间全部走环境变量，脚本里**不写死**任何环境；
//   · 令牌在 setup() 里登一次拿到，VU 之间只传令牌不重复登录
//     （登录接口有「用户名+IP 10 次/60 秒」限流，见 shared/constants.rs
//       LOGIN_RATE_MAX_ATTEMPTS，各脚本重复登录会直接把 429 打出来）；
//   · 每个请求都打 tag（endpoint=xxx），threshold 才能按接口分别卡 p95。
//
// 环境变量：
//   K6_BASE_URL        默认 http://127.0.0.1:18099
//   K6_ADMIN_USER      默认 admin
//   K6_ADMIN_PASSWORD  必填（用 `set -a; . /etc/cloudpivot/cloudpivot-dev.env` 注入）
//   K6_PERIOD          试算平衡/资产负债表的会计期间，默认 2026-11
//   K6_VUS / K6_DURATION  场景规模（各脚本有默认值）
// =============================================================================

import http from 'k6/http';
import { check } from 'k6';

export const BASE = __ENV.K6_BASE_URL || 'http://127.0.0.1:18099';
export const PERIOD = __ENV.K6_PERIOD || '2026-11';

const USER = __ENV.K6_ADMIN_USER || 'admin';
const PASS = __ENV.K6_ADMIN_PASSWORD || '';

/// 登录拿令牌。密码为空直接失败（不要静默跑出一个「全部 401」的假结果）。
export function login() {
  if (!PASS) {
    throw new Error('缺少 K6_ADMIN_PASSWORD（先 `set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a`）');
  }
  const res = http.post(
    `${BASE}/api/system/auth/login`,
    JSON.stringify({ username: USER, password: PASS }),
    { headers: { 'Content-Type': 'application/json' }, tags: { endpoint: 'login' } },
  );
  const ok = check(res, {
    '登录返回 200': (r) => r.status === 200,
    '登录响应带 access_token': (r) => {
      try {
        return !!(r.json('data.access_token'));
      } catch (_e) {
        return false;
      }
    },
  });
  if (!ok) {
    throw new Error(`登录失败：HTTP ${res.status} ${String(res.body).slice(0, 200)}`);
  }
  return res.json('data.access_token');
}

export function authHeaders(token, endpoint) {
  return { headers: { Authorization: `Bearer ${token}` }, tags: { endpoint } };
}

/// GET + 断言（200 且返回体 code=0）。返回响应对象，调用方可继续取 id。
export function getOk(token, path, endpoint) {
  const res = http.get(`${BASE}${path}`, authHeaders(token, endpoint));
  check(res, {
    [`${endpoint}: HTTP 200`]: (r) => r.status === 200,
    [`${endpoint}: body.code=0`]: (r) => {
      try {
        return r.json('code') === 0;
      } catch (_e) {
        return false;
      }
    },
  });
  return res;
}

/// 场景规模：VUS 台并发恒定跑 DURATION。
export function vus() {
  return Number(__ENV.K6_VUS || 5);
}

export function duration() {
  return __ENV.K6_DURATION || '20s';
}

/// 从数组里随机取一个（k6 的 VU 上下文有 Math.random，够用）。
export function pick(list) {
  return list[Math.floor(Math.random() * list.length)];
}
