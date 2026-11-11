/**
 * `doc-kinds.ts` 的**业务对象轴**镜像单测（卡 HR-07 / ARCH-9-2 §8.1）。
 *
 * 后端 `doc_kind_tests` 已经有三条「读前端 TS」的镜像断言（`frontend_ts_mirror_matches_rust`
 * 等），但它们只在 `cargo test` 里跑 —— 前端同学只跑 `pnpm test:unit` 时，动了
 * `DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES` 不会有人拦。本文件补上反方向：
 * **从前端这一侧读后端源码**，把 ① 业务对象轴的三个常量钉死。
 *
 * # 断言口径（全部抄源码，不抄设计文档）
 *
 * | 用例 | 钉住什么 |
 * |---|---|
 * | ① | `DocKind::ALL` ↔ `DOC_KINDS`：36 值**逐字 + 同序** |
 * | ② | `DocKind::label()` ↔ `DOC_KIND_LABELS`：36 个中文名**逐字** |
 * | ③ | `DocKind::table()` ↔ `DOC_KIND_TABLES`：有表的 35 项**逐条相等**（虚拟对象不列） |
 * | ④ | `ATTACHMENT_DOC_KINDS`（21）↔ `ATTACHMENT_DOC_TYPES`：同序，且必须是 `DOC_KINDS` 子集 |
 * | ⑤ | `DOC_KIND_ROUTES` 的每个键在后端都必须有 `route()`（前端只是联动作废的子集，不要求相等） |
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  ATTACHMENT_DOC_TYPES,
  DOC_KINDS,
  DOC_KIND_LABELS,
  DOC_KIND_ROUTES,
  DOC_KIND_TABLES,
} from '@/api/business/erp/doc-kinds';

/** 读后端 `doc_kind.rs` 全文。 */
function rustSource(): string {
  return readFileSync(new URL('../../../backend/src/business/shared/doc_kind.rs', import.meta.url), 'utf8');
}

/** 取 `impl <Type>` 块里某个 `pub const fn ...` 的 match 体（到第一个 4 空格缩进的 `}` 为止）。 */
function rustFnBody(source: string, type: string, signature: string): string {
  const implAt = source.indexOf(`impl ${type} {`);
  assert.ok(implAt > 0, `找不到 impl ${type}`);
  const fnAt = source.indexOf(signature, implAt);
  assert.ok(fnAt > 0, `找不到 ${type} 的 ${signature}`);
  const bodyStart = source.indexOf('match self {', fnAt);
  assert.ok(bodyStart > 0, `${signature} 里没有 match self`);
  const bodyEnd = source.indexOf('\n    }', bodyStart);
  assert.ok(bodyEnd > bodyStart, `${signature} 的 match 没有闭合`);
  return source.slice(bodyStart, bodyEnd);
}

/** `Type::Variant => "值"` 的映射（顺序 = match 臂顺序）。 */
function rustPairs(source: string, type: string, signature: string): Array<[string, string]> {
  const body = rustFnBody(source, type, signature);
  return [...body.matchAll(new RegExp(`${type}::(\\w+) => "([^"]*)"`, 'g'))].map((match) => [
    match[1] ?? '',
    match[2] ?? '',
  ]);
}

/** `DocKind::ALL` 的变体顺序（= 落库值顺序的唯一真相）。 */
function rustDocKindAll(source: string): string[] {
  const match = /pub const ALL: \[DocKind; (\d+)\] = \[([\s\S]*?)\n {4}\];/.exec(source);
  assert.ok(match, '找不到 DocKind::ALL');
  const variants = [...(match[2] ?? '').matchAll(/^\s*DocKind::(\w+),/gm)].map((item) => item[1] ?? '');
  assert.equal(variants.length, Number(match[1]), 'DocKind::ALL 的声明长度与实际变体数不一致');
  return variants;
}

/** `ATTACHMENT_DOC_KINDS` 的变体顺序。 */
function rustAttachmentDocKinds(source: string): string[] {
  const match = /pub const ATTACHMENT_DOC_KINDS: \[DocKind; (\d+)\] = \[([\s\S]*?)\n\];/.exec(source);
  assert.ok(match, '找不到 ATTACHMENT_DOC_KINDS');
  const variants = [...(match[2] ?? '').matchAll(/^\s*DocKind::(\w+),/gm)].map((item) => item[1] ?? '');
  assert.equal(variants.length, Number(match[1]), 'ATTACHMENT_DOC_KINDS 的声明长度与实际变体数不一致');
  return variants;
}

// ==================== ① 值域：36 值逐字 + 同序 ====================

test('DocKind::ALL ↔ DOC_KINDS：36 个业务对象逐字、同序（HR-06 的 5 类在末尾）', () => {
  const source = rustSource();
  const asStr = new Map(rustPairs(source, 'DocKind', 'pub const fn as_str'));
  const rust = rustDocKindAll(source).map((variant) => {
    const value = asStr.get(variant);
    assert.ok(value, `DocKind::${variant} 在 as_str() 里没有落库值`);
    return value;
  });

  assert.equal(rust.length, 36, `后端 DocKind::ALL 应为 36 值，实际 ${rust.length}`);
  assert.deepEqual([...DOC_KINDS], rust, 'DOC_KINDS 与后端 DocKind::ALL 不一致（顺序也要一致）');

  // HR 五类：追加在末尾（不插中间），且顺序与后端一致
  assert.deepEqual([...DOC_KINDS].slice(-5), [
    'EMPLOYEE',
    'EMPLOYEE_CONTRACT',
    'LEAVE_REQUEST',
    'ATTENDANCE_PERIOD',
    'SALARY_SHEET',
  ]);
});

// ==================== ② 中文名：36 个逐字 ====================

test('DocKind::label() ↔ DOC_KIND_LABELS：36 个中文名逐字一致（不许只改一边）', () => {
  const source = rustSource();
  const asStr = new Map(rustPairs(source, 'DocKind', 'pub const fn as_str'));
  const labels = new Map(rustPairs(source, 'DocKind', 'pub const fn label'));

  const rust: Record<string, string> = {};
  for (const [variant, label] of labels) {
    const value = asStr.get(variant);
    assert.ok(value, `DocKind::${variant} 在 as_str() 里没有落库值`);
    rust[value] = label;
  }

  assert.equal(Object.keys(rust).length, 36, `后端应有 36 个中文名，实际 ${Object.keys(rust).length}`);
  assert.deepEqual({ ...DOC_KIND_LABELS }, rust, 'DOC_KIND_LABELS 与后端 DocKind::label() 不一致');
});

// ==================== ③ 实体表：有表的 35 项逐条 ====================

test('DocKind::table() ↔ DOC_KIND_TABLES：有表的 35 项逐条相等（虚拟对象不列）', () => {
  const source = rustSource();
  const asStr = new Map(rustPairs(source, 'DocKind', 'pub const fn as_str'));
  const tables = new Map(rustPairs(source, 'DocKind', 'pub const fn table'));

  const rust: Record<string, string> = {};
  for (const [variant, table] of tables) {
    const value = asStr.get(variant);
    assert.ok(value, `DocKind::${variant} 在 as_str() 里没有落库值`);
    rust[value] = table;
  }

  assert.equal(Object.keys(rust).length, 35, `后端有表的对象应为 35 个，实际 ${Object.keys(rust).length}`);
  assert.deepEqual({ ...DOC_KIND_TABLES }, rust, 'DOC_KIND_TABLES 与后端 DocKind::table() 不一致');
});

// ==================== ④ 附件可挂对象：21 值同序 + 子集 ====================

test('ATTACHMENT_DOC_KINDS(21) ↔ ATTACHMENT_DOC_TYPES：同序，且是 DOC_KINDS 的子集', () => {
  const source = rustSource();
  const asStr = new Map(rustPairs(source, 'DocKind', 'pub const fn as_str'));
  const rust = rustAttachmentDocKinds(source).map((variant) => {
    const value = asStr.get(variant);
    assert.ok(value, `DocKind::${variant} 在 as_str() 里没有落库值`);
    return value;
  });

  assert.equal(rust.length, 21, `后端 ATTACHMENT_DOC_KINDS 应为 21 值，实际 ${rust.length}`);
  assert.deepEqual([...ATTACHMENT_DOC_TYPES], rust, 'ATTACHMENT_DOC_TYPES 与后端 ATTACHMENT_DOC_KINDS 不一致');

  // 子集关系：附件的每个取值都必须是合法业务对象
  for (const value of ATTACHMENT_DOC_TYPES) {
    assert.ok(
      (DOC_KINDS as readonly string[]).includes(value),
      `${value} 不在 DOC_KINDS 里 —— ATTACHMENT_DOC_TYPES 必须是它的子集`,
    );
  }
  // 注意：附件轴的顺序**不等于**业务对象轴的顺序（历史分组：合同 → 采购 → 库存 → 应付…
  // 由后端 ATTACHMENT_DOC_KINDS 决定），所以这里只钉「同序于后端」，不钉「同序于 DOC_KINDS」。

  // HR 只 4 类（考勤期间没有附件口径），且追加在末尾
  assert.deepEqual([...ATTACHMENT_DOC_TYPES].slice(-4), [
    'EMPLOYEE',
    'EMPLOYEE_CONTRACT',
    'LEAVE_REQUEST',
    'SALARY_SHEET',
  ]);
  assert.ok(
    !(ATTACHMENT_DOC_TYPES as readonly string[]).includes('ATTENDANCE_PERIOD'),
    'ATTENDANCE_PERIOD 不该进附件轴（考勤批次走导入任务留痕）',
  );
});

// ==================== ⑤ 联动作废路径段：键必须是后端的子集 ====================

test('DOC_KIND_ROUTES 的每个键，后端 DocKind::route() 都必须有路径段', () => {
  const source = rustSource();
  const asStr = new Map(rustPairs(source, 'DocKind', 'pub const fn as_str'));
  const routes = new Map(rustPairs(source, 'DocKind', 'pub const fn route'));
  const rustRoute: Record<string, string> = {};
  for (const [variant, route] of routes) {
    const value = asStr.get(variant);
    if (value) {
      rustRoute[value] = route;
    }
  }

  const keys = Object.keys(DOC_KIND_ROUTES);
  assert.ok(keys.length > 0, 'DOC_KIND_ROUTES 不能为空');
  for (const key of keys) {
    const expected = rustRoute[key];
    assert.ok(expected, `${key} 在后端 DocKind::route() 里没有路径段`);
    assert.equal(
      (DOC_KIND_ROUTES as Record<string, string>)[key],
      expected,
      `${key} 的路径段与后端 route() 不一致`,
    );
  }
});
