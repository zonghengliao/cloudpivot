/**
 * ATT-03 单测：上传人显示名（列表列 / 预览面板共用同一函数）。
 *
 * # 钉死的口径
 *
 * 1. **后端已下发 `uploader_name` 时以它为准** —— 有名字显示名字；
 * 2. **空串 = 上传人已不存在**（`uploaded_by` 指向已删用户 / 历史脏数据）→ 显示 `-`，
 *    绝不把 UUID 显示给用户（ATT-01 实测就是这个 bug）；
 * 3. **只有字段缺失**（老后端 / 老缓存没有该字段）才回退 `uploaded_by`，再空显示 `-`。
 *
 * 另外用源码断言钉住视图接线：列表「上传人」列与预览 items 都必须走
 * `uploaderLabel()`，不许把 `row.uploaded_by` 直接递给界面。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { uploaderLabel } from '../../src/views/support/file/uploader-label';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');
const UUID = '075c3b0e-07d8-7730-8bdc-1e1c241f2995';

test('有名字：显示名字（真名 / 用户名都走这一支）', () => {
  assert.equal(uploaderLabel({ uploader_name: '系统管理员', uploaded_by: UUID }), '系统管理员');
  assert.equal(uploaderLabel({ uploader_name: 'admin', uploaded_by: UUID }), 'admin');
  // 前后空白要 trim（后端 COALESCE 不会产生空白，但别让空白把名字顶掉）
  assert.equal(uploaderLabel({ uploader_name: '  张三  ', uploaded_by: UUID }), '张三');
});

test('空串：显示 "-"，不是 UUID（脏数据 uploaded_by 指向不存在用户）', () => {
  assert.equal(uploaderLabel({ uploader_name: '', uploaded_by: UUID }), '-');
  assert.equal(uploaderLabel({ uploader_name: '   ', uploaded_by: UUID }), '-');
});

test('字段缺失（老后端）：才回退 uploaded_by，再空也显示 "-"', () => {
  assert.equal(uploaderLabel({ uploaded_by: UUID }), UUID);
  assert.equal(uploaderLabel({ uploaded_by: '' }), '-');
  assert.equal(uploaderLabel({}), '-');
});

test('视图接线：列表列与预览 items 都走 uploaderLabel，不直接把 ID 递给界面', () => {
  const vue = readFileSync(join(SRC, 'views/support/file/index.vue'), 'utf8');
  // 列表列 colKey 换成 uploader_name，并绑 cell 模板
  assert.match(vue, /colKey:\s*'uploader_name'[^}]*cell:\s*'uploader_name'/, '列必须改成 uploader_name + cell 模板');
  assert.match(
    vue,
    /#uploader_name="\{ row \}">\s*<span data-testid="file-uploader-name">\{\{ uploaderLabel\(row\) \}\}<\/span>/,
    '列模板必须调用 uploaderLabel，并留 data-testid 给 e2e',
  );
  // 预览 items 的 uploader 同理
  assert.match(vue, /uploader:\s*uploaderLabel\(row\)/, '预览 items.uploader 必须调用 uploaderLabel');
  assert.doesNotMatch(vue, /uploader:\s*row\.uploaded_by/, '不许再把 uploaded_by（UUID）当上传人传给预览面板');
});

test('API 契约：FileRes 带 uploader_name?: string', () => {
  const api = readFileSync(join(SRC, 'api/support/file-api.ts'), 'utf8');
  assert.match(api, /uploader_name\?:\s*string;/, 'FileRes 必须声明 uploader_name?: string');
  assert.match(api, /uploaded_by:\s*string;/, 'uploaded_by 仍保留（兼容老后端 / 排障）');
});
