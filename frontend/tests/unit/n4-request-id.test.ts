/**
 * N-4 单测：前端 `request_id` 贯穿。
 *
 * # 分两层
 *
 * 1. **行为层**（`src/api/request-id.ts` 的纯逻辑，真跑）：
 *    - 每个请求的 ID 只能含后端 `is_acceptable` 采信的字符集（否则后端会丢弃并换 ID，
 *      前端展示的就不是日志里那个，「用户报障→grep 日志」整条链路断掉）；
 *    - 无 `crypto.randomUUID` 的内网 http 场景（`randomUUID` 只在安全上下文存在）也要给出可用 ID；
 *    - 响应头优先于「我们发出去的那个」；
 *    - 文案追加规则，以及**没有 ID 时不污染文案**。
 * 2. **接线层**（`src/api/request.ts` 的源码契约）：
 *    `request.ts` 顶层读 `import.meta.env`（Vite 专有），Node 侧无法 import，
 *    因此对它做文本断言：拦截器写头、`toApiError`/`request` 传 `requestId`。
 *    行为面由 145 真实 curl（响应头 + 日志 grep）兜底，见证据包。
 *
 * 运行：`pnpm test:unit`（或 `npx tsx --test tests/unit/n4-request-id.test.ts`）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  MAX_REQUEST_ID_LEN,
  REQUEST_ID_HEADER,
  isAcceptableRequestId,
  newRequestId,
  resolveRequestId,
  withRequestId,
} from '../../src/api/request-id.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const REQUEST_TS = readFileSync(resolve(HERE, '../../src/api/request.ts'), 'utf8');

/** 后端 `is_acceptable` 的字符集：客户端发出去的值必须落在这个集合里 */
const BACKEND_ACCEPTABLE = /^[A-Za-z0-9._:-]{1,128}$/;

test('newRequestId() 生成 UUID 形态，且落在后端采信字符集内', () => {
  for (let i = 0; i < 20; i += 1) {
    const id = newRequestId();
    assert.match(id, BACKEND_ACCEPTABLE, `不合规的 request_id：${id}`);
    assert.match(id, /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i);
  }
});

test('newRequestId() 唯一（两次请求不能撞成同一个 ID）', () => {
  const ids = new Set(Array.from({ length: 200 }, () => newRequestId()));
  assert.equal(ids.size, 200);
});

test('无 crypto.randomUUID（内网 http）时退化为 web- 前缀且仍可被后端采信', () => {
  const original = globalThis.crypto;
  Object.defineProperty(globalThis, 'crypto', { value: {}, configurable: true, writable: true });
  try {
    const id = newRequestId();
    assert.match(id, /^web-[a-z0-9]+-[a-z0-9]+$/);
    assert.match(id, BACKEND_ACCEPTABLE);
  } finally {
    Object.defineProperty(globalThis, 'crypto', { value: original, configurable: true, writable: true });
  }
});

test('头名与后端一致：X-Request-Id（后端按大小写不敏感匹配 x-request-id）', () => {
  assert.equal(REQUEST_ID_HEADER.toLowerCase(), 'x-request-id');
});

test('isAcceptableRequestId() 只认后端字符集与长度上限', () => {
  assert.equal(isAcceptableRequestId('a1b2c3d4-0000-7000-8000-123456789abc'), true);
  assert.equal(isAcceptableRequestId('web-lq3k2-9f8a1b2c'), true);
  assert.equal(isAcceptableRequestId('0af7651916cd43dd8448eb211c80319c'), true);
  assert.equal(isAcceptableRequestId(''), false);
  assert.equal(isAcceptableRequestId('abc def'), false);
  assert.equal(isAcceptableRequestId('abc\ndef'), false);
  assert.equal(isAcceptableRequestId('注入'), false);
  assert.equal(isAcceptableRequestId('a'.repeat(MAX_REQUEST_ID_LEN + 1)), false);
  assert.equal(isAcceptableRequestId(undefined), false);
  assert.equal(isAcceptableRequestId(123), false);
});

test('resolveRequestId()：响应头优先于请求里发出去的值', () => {
  assert.equal(resolveRequestId({ 'x-request-id': 'srv-1' }, 'web-2'), 'srv-1');
  // 没有响应（网络层失败）→ 退回发出去的那个，用户仍能报上来
  assert.equal(resolveRequestId(undefined, 'web-2'), 'web-2');
  assert.equal(resolveRequestId({}, 'web-2'), 'web-2');
  // 两头都没有
  assert.equal(resolveRequestId(undefined, undefined), undefined);
  assert.equal(resolveRequestId({}, ''), undefined);
});

test('withRequestId()：有 ID 追加、无 ID 原样（既有文案不被污染）', () => {
  assert.equal(withRequestId('权限不足', 'srv-1'), '权限不足（请求 ID：srv-1）');
  assert.equal(withRequestId('权限不足'), '权限不足');
  assert.equal(withRequestId('权限不足', ''), '权限不足');
});

// ── 接线层：`request.ts` 源码契约（它的行为由 145 实测兜底） ──

test('request.ts：请求拦截器给每个请求写 X-Request-Id', () => {
  assert.match(REQUEST_TS, /config\.headers\[REQUEST_ID_HEADER\] = newRequestId\(\)/);
  assert.match(REQUEST_TS, /service\.interceptors\.request\.use/);
});

test('request.ts：ApiError 带上 requestId 字段并把 ID 拼进 message', () => {
  assert.match(REQUEST_TS, /readonly requestId\?: string/);
  assert.match(REQUEST_TS, /super\(withRequestId\(message, requestId\)\)/);
});

test('request.ts：错误路径（toApiError / 业务码非 0）都解析并传入 requestId', () => {
  const calls = REQUEST_TS.match(/resolveRequestId\(/g) ?? [];
  // 两处：拦截器错误路径（toApiError）+ 统一响应体业务码路径（request<T>）
  assert.ok(calls.length >= 2, `期望至少 2 处 resolveRequestId，实际 ${calls.length}`);
  assert.match(REQUEST_TS, /const requestId = resolveRequestId\(error\.response\?\.headers/);
  assert.match(REQUEST_TS, /const requestId = resolveRequestId\(\s*response\.headers,/);
});
