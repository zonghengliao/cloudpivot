/**
 * ATT-09 单测：附件规则管理界面的纯逻辑 + 接口 / 路由 / 页面契约。
 *
 * # 钉死的口径
 *
 * 1. **allow_exts 校验与后端同源**：小写、去重、去点号前缀、中文逗号也认；
 *    `*` / 空格 / 超长 token / 只写分隔符 / 超 20 个都必须拦下（后端也会 400）。
 * 2. **数量区间**：`min_count ≥ 1`、`max_count` 留空或 `≥ min_count`。
 * 3. **接口路径必须带 `/api` 前缀**（漏了会命中 SPA fallback 返回 HTML）。
 * 4. **路由**：`/support/attachment-rule` + `support:attachment-rule:list`。
 * 5. **页面**：新增 / 编辑 / 删除分别挂 `create` / `update` / `delete` 权限码，
 *    启停是 switch（即时生效），列表列覆盖卡面要求的所有字段。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  EXT_TOKEN_MAX_CHARS,
  EXT_TOKEN_MAX_COUNT,
  parseAllowExts,
  requirementSummary,
  validateCounts,
} from '../../src/views/support/attachment-rule/rule-pure';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

// ---------- allow_exts ----------

test('allow_exts：留空 = 不限格式', () => {
  assert.deepEqual(parseAllowExts(''), { value: null, error: null });
  assert.deepEqual(parseAllowExts('   '), { value: null, error: null });
  assert.deepEqual(parseAllowExts(null), { value: null, error: null });
  assert.deepEqual(parseAllowExts(undefined), { value: null, error: null });
});

test('allow_exts：归一为小写、去点号前缀、去重保序、认中文逗号', () => {
  assert.deepEqual(parseAllowExts(' PDF, .Ofd ,pdf，， JPG '), {
    value: 'pdf,ofd,jpg',
    error: null,
  });
});

test('allow_exts：通配符与带空格的 token 必须拦下', () => {
  const star = parseAllowExts('pdf,*');
  assert.equal(star.value, null);
  assert.match(star.error ?? '', /不合法/);

  const spaced = parseAllowExts('pdf,p df');
  assert.equal(spaced.value, null);
  assert.match(spaced.error ?? '', /不合法/);
});

test('allow_exts：只写分隔符要拦下（否则等于「什么都不许」）', () => {
  const only = parseAllowExts(' , ， ');
  assert.equal(only.value, null);
  assert.match(only.error ?? '', /分隔符/);
});

test('allow_exts：单 token 超长 / 超 20 个都要拦下', () => {
  const long = 'a'.repeat(EXT_TOKEN_MAX_CHARS + 1);
  assert.equal(parseAllowExts(`pdf,${long}`).value, null);

  const many = Array.from({ length: EXT_TOKEN_MAX_COUNT + 1 }, (_, index) => `e${index}`).join(',');
  const result = parseAllowExts(many);
  assert.equal(result.value, null);
  assert.match(result.error ?? '', /20/);

  const justEnough = Array.from({ length: EXT_TOKEN_MAX_COUNT }, (_, index) => `e${index}`).join(',');
  assert.equal(parseAllowExts(justEnough).error, null);
});

// ---------- 数量区间 ----------

test('数量：min_count 至少 1', () => {
  assert.equal(validateCounts(0, null).error !== null, true);
  assert.equal(validateCounts(-1, null).error !== null, true);
  assert.equal(validateCounts(1, null).error, null);
  assert.equal(validateCounts(2, 2).error, null, 'min == max 合法');
});

test('数量：max_count 留空 = 不限，非空必须 ≥ min_count', () => {
  assert.equal(validateCounts(3, null).error, null);
  assert.equal(validateCounts(3, undefined).error, null);
  assert.equal(validateCounts(3, 2).error !== null, true);
  assert.equal(validateCounts(3, 0).error !== null, true);
});

// ---------- 摘要 ----------

test('摘要在禁用时追加说明（不改写后端文案）', () => {
  const text = '发票必须上传发票影像（pdf/ofd/xml）';
  assert.equal(requirementSummary(text, true), text);
  assert.equal(requirementSummary(text, false), `${text}（当前已禁用，不参与校验）`);
});

// ---------- 接口契约 ----------

test('接口：路径全部带 /api 前缀，启停走 /{id}/enabled', () => {
  const source = readSource('api/support/attachment-rule-api.ts');
  for (const path of [
    "'/api/support/attachment-rule/list'",
    "'/api/support/attachment-rule/options'",
    "'/api/support/attachment-rule'",
    '`/api/support/attachment-rule/${id}`',
    '`/api/support/attachment-rule/${id}/enabled`',
  ]) {
    assert.ok(source.includes(path), `缺少接口路径 ${path}`);
  }
  // 六个接口一个都不能少（卡面要求列表 / 下拉 / 新建 / 编辑 / 启停 / 软删）
  for (const fn of [
    'listAttachmentRules',
    'getAttachmentRuleOptions',
    'createAttachmentRule',
    'updateAttachmentRule',
    'setAttachmentRuleEnabled',
    'deleteAttachmentRule',
  ]) {
    assert.ok(source.includes(`export function ${fn}`), `缺少接口函数 ${fn}`);
  }
});

// ---------- 路由 / 页面契约 ----------

test('路由：/support/attachment-rule 与权限码逐字一致', () => {
  const router = readSource('router/index.ts');
  assert.ok(router.includes("path: 'support/attachment-rule'"), '缺少路由 path');
  assert.ok(
    router.includes("permission: 'support:attachment-rule:list'"),
    '路由必须挂 list 权限码（非超管打不开）',
  );
  assert.ok(
    router.includes("import('@/views/support/attachment-rule/index.vue')"),
    '路由必须指向新页面',
  );
});

test('页面：新增 / 编辑 / 删除各挂自己的权限码', () => {
  const page = readSource('views/support/attachment-rule/index.vue');
  for (const code of [
    'support:attachment-rule:create',
    'support:attachment-rule:update',
    'support:attachment-rule:delete',
  ]) {
    assert.ok(page.includes(`v-privilege="['${code}']"`), `缺少权限码 ${code}`);
  }
  // 启停是开关，且禁用状态跟随 update 权限
  assert.ok(page.includes('t-switch'), '启停必须是 t-switch');
  assert.ok(page.includes('setAttachmentRuleEnabled'), '启停必须调启停接口');
});

test('页面：列表列覆盖卡面要求的全部字段', () => {
  const page = readSource('views/support/attachment-rule/index.vue');
  for (const key of [
    "'doc_type'",
    "'file_role'",
    "'required_at'",
    "'count'",
    "'allow_exts'",
    "'enabled'",
  ]) {
    assert.ok(page.includes(`colKey: ${key}`), `缺少列 ${key}`);
  }
  // 表单里 doc_type / required_at 必须来自后端下拉，不能写死
  assert.ok(page.includes('options.doc_types'), 'doc_type 下拉必须来自后端');
  assert.ok(page.includes('options.required_ats'), 'required_at 下拉必须来自后端');
  assert.ok(page.includes('rule-form-allow-exts'), '缺少 allow_exts 输入');
});
