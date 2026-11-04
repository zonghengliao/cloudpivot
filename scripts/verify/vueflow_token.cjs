/**
 * 验证脚本共用：取 admin token（带本地缓存，避免登录限流 429）。
 *
 * 后端的登录限流是按 IP 的（1 分钟内多次 → 10005「登录尝试过于频繁」），
 * 而验收脚本常常连着跑好几遍，所以把 token 缓存在 /tmp 里 8 分钟。
 */
'use strict';

const fs = require('node:fs');

const CACHE = '/tmp/vueflow_verify_token';

async function loginToken(base, user = 'admin', password = 'Admin@123456') {
  if (process.env.VF_TOKEN) return process.env.VF_TOKEN;
  try {
    const cached = fs.readFileSync(CACHE, 'utf8').trim().split('\n');
    if (cached[0] && Date.now() - Number(cached[1]) < 8 * 60 * 1000) return cached[0];
  } catch {
    /* 无缓存 */
  }
  const resp = await fetch(`${base}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: user, password }),
  });
  const body = await resp.json().catch(() => ({}));
  const token = body?.data?.access_token;
  if (!token) {
    throw new Error(`登录失败（HTTP ${resp.status}）：${JSON.stringify(body).slice(0, 200)}`);
  }
  fs.writeFileSync(CACHE, `${token}\n${Date.now()}`);
  return token;
}

module.exports = { loginToken };
