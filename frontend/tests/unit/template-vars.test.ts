/**
 * `template-vars.ts` 单测 —— 模板变量白名单 + 占位符解析 + **光标处插入**（P-FLOW-1C）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）
 *
 * # 这份测试存在的两个理由
 *
 * 1. **行为**：白名单的三种归属（固定 10 / 只给模板的 4 / `form.*` 动态）、
 *    `task.*` 的 `TASK_ACCEPT` 开关、路径文法（一个点号 + 每段 ≤ 64）、
 *    以及「插到光标处 / 替换选中段 / 插完光标落在 `}}` 之后」这条卡面验收 3 的口径。
 * 2. **对齐**：直接读 `backend/src/**` 的 rust 源码，断言常量与后端**逐字一致**。
 *    后端加一个字段、改一个上限，这里先红 —— 而不是等到发布时报 65037 才发现。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import {
  FIXED_FIELDS,
  MAX_VAR_SEGMENT,
  TASK_ACCEPT_BIZ_TYPE,
  TEMPLATE_ONLY_FIELDS,
  extractPlaceholders,
  insertPlaceholder,
  isTaskAccept,
  isTemplateFieldAllowed,
  pathGrammarError,
  scanPlaceholders,
  validateTemplateVars,
  varGroups,
} from '../../src/views/business/flow/components/template-vars.ts';
import { FIXED_FIELDS as CONDITION_FIXED_FIELDS } from '../../src/views/business/flow/components/condition-tree.ts';

// ==================== 读后端源码的工具 ====================

/** 读 `backend/src/business/flow/<relative>` */
function rust(relative: string): string {
  return readFileSync(
    fileURLToPath(new URL(`../../../backend/src/business/flow/${relative}`, import.meta.url)),
    'utf8',
  );
}

/** 读 `backend/src/business/notice/render.rs`（变量名文法的唯一出处） */
function renderSource(): string {
  return readFileSync(
    fileURLToPath(new URL('../../../backend/src/business/notice/render.rs', import.meta.url)),
    'utf8',
  );
}

/** 从 rust 源码里抠一个 `pub const NAME: type = 值;` */
function rustConst(source: string, name: string): string {
  const matched = new RegExp(`pub const ${name}:\\s*\\w+\\s*=\\s*([^;]+);`).exec(source);
  assert.ok(matched, `后端源码里找不到常量 ${name}`);
  return (matched[1] as string).trim();
}

/** 抠一个 `pub const NAME: [&str; N] = [ "a", "b", ];` 数组 */
function rustStrArray(source: string, name: string): string[] {
  const matched = new RegExp(`pub const ${name}:\\s*\\[&str;\\s*\\d+\\]\\s*=\\s*\\[([\\s\\S]*?)\\];`).exec(source);
  assert.ok(matched, `后端源码里找不到数组常量 ${name}`);
  const body = matched[1] as string;
  return [...body.matchAll(/"([^"]*)"/g)].map((item) => item[1] as string);
}

/** 非 TASK_ACCEPT 的业务类型（任何普通流程都属于这一类） */
const OTHER_BIZ = 'PURCHASE_ORDER';

// ==================== ① 白名单与后端逐字对齐 ====================

test('FIXED_FIELDS 与 graph.rs 逐字一致（10 个，顺序也一致）', () => {
  const backend = rustStrArray(rust('graph.rs'), 'FIXED_FIELDS');
  assert.deepEqual([...FIXED_FIELDS], backend);
  assert.equal(FIXED_FIELDS.length, 10);
  // 条件 DSL 的同一份白名单不能漂移（两处各写一份是历史原因，内容必须相等）
  assert.deepEqual([...FIXED_FIELDS], [...CONDITION_FIXED_FIELDS]);
});

test('TEMPLATE_ONLY_FIELDS 与 graph.rs 逐字一致（4 个只给模板用的根）', () => {
  const backend = rustStrArray(rust('graph.rs'), 'TEMPLATE_ONLY_FIELDS');
  assert.deepEqual([...TEMPLATE_ONLY_FIELDS], backend);
  // 这 4 个**不在**条件 DSL 白名单里（设计 §2.5.1 的唯一一处「故意不一致」）
  for (const field of TEMPLATE_ONLY_FIELDS) {
    assert.ok(!(CONDITION_FIXED_FIELDS as readonly string[]).includes(field), `${field} 不该进条件白名单`);
  }
});

test('MAX_VAR_SEGMENT 与 render.rs 的 MAX_VAR_NAME_LEN 一致', () => {
  const backend = renderSource();
  assert.equal(MAX_VAR_SEGMENT, Number(rustConst(backend, 'MAX_VAR_NAME_LEN')));
  assert.equal(TASK_ACCEPT_BIZ_TYPE, 'TASK_ACCEPT');
});

// ==================== ② TASK_ACCEPT 流程下：固定字段全量合法 ====================

test('TASK_ACCEPT 流程下 FIXED_FIELDS + TEMPLATE_ONLY_FIELDS 全量合法（14 条零 issue）', () => {
  for (const field of [...FIXED_FIELDS, ...TEMPLATE_ONLY_FIELDS]) {
    const issues = validateTemplateVars(`前 {{${field}}} 后`, TASK_ACCEPT_BIZ_TYPE);
    assert.deepEqual(issues, [], `${field} 在 TASK_ACCEPT 下应合法`);
  }
});

// ==================== ③ task.* 只在 TASK_ACCEPT 流程可用（卡面验收 1 的一条） ====================

test('非 TASK_ACCEPT 流程里 task.* 判非法（65037），其余 6 个固定字段仍合法', () => {
  const taskFields = FIXED_FIELDS.filter((field) => field.startsWith('task.'));
  assert.equal(taskFields.length, 4);
  for (const field of taskFields) {
    const issues = validateTemplateVars(`{{${field}}}`, OTHER_BIZ);
    assert.equal(issues.length, 1, `${field} 在 ${OTHER_BIZ} 下应恰好一条 issue`);
    assert.equal(issues[0]?.code, 65037);
    assert.match(issues[0]?.reason ?? '', /只在 TASK_ACCEPT 流程里有值/);
  }
  const others = FIXED_FIELDS.filter((field) => !field.startsWith('task.'));
  assert.deepEqual(validateTemplateVars(others.map((field) => `{{${field}}}`).join(' '), OTHER_BIZ), []);
});

test('bizType 大小写 / 前后空白不敏感（与后端 eq_ignore_ascii_case 同口径）', () => {
  assert.equal(isTaskAccept(' task_accept '), true);
  assert.equal(isTaskAccept('TASK_ACCEPT'), true);
  assert.equal(isTaskAccept('Task_Accept'), true);
  assert.equal(isTaskAccept(''), false);
  assert.equal(isTaskAccept(null), false);
  assert.deepEqual(validateTemplateVars('{{task.title}}', 'task_accept'), []);
});

// ==================== ④ form.* 的合法 / 非法 ====================

test('form.* 合法形状：字母或下划线开头的标识符', () => {
  for (const field of ['form.amount', 'form.supplier_name', 'form._x', 'form.a1', 'form.A']) {
    assert.deepEqual(validateTemplateVars(`{{${field}}}`, OTHER_BIZ), [], `${field} 应合法`);
    assert.equal(isTemplateFieldAllowed(field), true);
  }
});

test('form.* 非法形状：空段 / 数字开头 / 连字符 / 两层点号 / 缺段', () => {
  const bad = ['form.', 'form.1x', 'form.a-b', 'form.a.b', 'form', 'form.a b'];
  for (const field of bad) {
    const issues = validateTemplateVars(`{{${field}}}`, OTHER_BIZ);
    assert.ok(issues.length > 0, `${field} 应判非法`);
    // 前三条是变量名文法错（65036），`form` / `form.a b` 同理；白名单错只可能是 65037
    assert.ok([65036, 65037].includes(issues[0]?.code ?? 0), `${field} 的码应是 65036/65037`);
    assert.equal(isTemplateFieldAllowed(field), false);
  }
});

test('完全不存在的根（order.amount / unknown.field）判非法 65037', () => {
  const issues = validateTemplateVars('{{order.amount}}', OTHER_BIZ);
  assert.equal(issues.length, 1);
  assert.equal(issues[0]?.code, 65037);
  assert.equal(issues[0]?.path, 'order.amount');
  assert.match(issues[0]?.reason ?? '', /不在白名单/);
});

// ==================== ⑤ 超长段名（卡面点名的边界） ====================

test(`单个路径段超过 ${MAX_VAR_SEGMENT} 字符判语法错（65036），等于上限合法`, () => {
  const atLimit = 'a'.repeat(MAX_VAR_SEGMENT);
  const tooLong = 'a'.repeat(MAX_VAR_SEGMENT + 1);

  // 叶段（form.* 的字段名——白名单里**只有** form. 后面允许任意合法标识符）
  assert.deepEqual(validateTemplateVars(`{{form.${atLimit}}}`, OTHER_BIZ), []);
  const leafIssues = validateTemplateVars(`{{form.${tooLong}}}`, OTHER_BIZ);
  assert.equal(leafIssues[0]?.code, 65036);
  assert.match(leafIssues[0]?.reason ?? '', new RegExp(`超过 ${MAX_VAR_SEGMENT} 个字符`));

  // 根段（文法层面：`<段>.name`）
  assert.equal(pathGrammarError(`${atLimit}.name`), null);
  const rootIssues = validateTemplateVars(`{{${tooLong}.name}}`, OTHER_BIZ);
  assert.equal(rootIssues[0]?.code, 65036);
  assert.match(rootIssues[0]?.reason ?? '', new RegExp(`超过 ${MAX_VAR_SEGMENT} 个字符`));

  // 固定根（initiator / instance / node / prev）后面的叶子必须**逐字命中**，长名字不是「合法但超长」而是白名单外
  const fixedLeaf = validateTemplateVars(`{{initiator.${atLimit}}}`, OTHER_BIZ);
  assert.equal(fixedLeaf[0]?.code, 65037);
});

// ==================== ⑥ 语法：未闭合 / 多于一层点号 / 花括号内空白 ====================

test('未闭合的占位符报 65036 且只报一条（后端的 ? 也是直接返回）', () => {
  const issues = validateTemplateVars('前 {{instance.biz_no 后', OTHER_BIZ);
  assert.equal(issues.length, 1);
  assert.equal(issues[0]?.code, 65036);
  assert.match(issues[0]?.reason ?? '', /缺少收尾的/);
});

test('多于一层点号（form.a.b）判语法错 65036', () => {
  const issues = validateTemplateVars('{{form.a.b}}', OTHER_BIZ);
  assert.equal(issues[0]?.code, 65036);
  assert.match(issues[0]?.reason ?? '', /最多允许一层点号/);
  assert.equal(pathGrammarError('form.a.b') !== null, true);
  assert.equal(pathGrammarError('form.a'), null);
});

test('花括号内允许空白（{{ instance.biz_no }}），路径按 trim 解析', () => {
  const scan = scanPlaceholders('A {{ instance.biz_no }} B');
  assert.equal(scan.syntax.length, 0);
  assert.equal(scan.placeholders.length, 1);
  assert.equal(scan.placeholders[0]?.path, 'instance.biz_no');
  assert.equal(scan.placeholders[0]?.raw, '{{ instance.biz_no }}');
});

// ==================== ⑦ 抽取：顺序、去重、下标 ====================

test('extractPlaceholders 保持出现顺序并去重；start/end 含花括号', () => {
  const template = '{{instance.biz_no}} 催 {{form.amount}} 又 {{instance.biz_no}}';
  assert.deepEqual(extractPlaceholders(template), ['instance.biz_no', 'form.amount']);
  const scan = scanPlaceholders(template);
  assert.equal(scan.placeholders.length, 3);
  assert.equal(template.slice(scan.placeholders[0]!.start, scan.placeholders[0]!.end), '{{instance.biz_no}}');
  assert.equal(scan.placeholders[1]!.start, template.indexOf('{{form.amount}}'));
});

// ==================== ⑧ 插入：光标位置（卡面验收 3 的核心） ====================

test('光标在中间插入：文本正确、光标落 `}}` 之后', () => {
  const source = '前催后';
  // 光标在「催」之后（index 2）
  const out = insertPlaceholder(source, 'instance.biz_no', 2, 2);
  assert.equal(out.text, '前催{{instance.biz_no}}后');
  // 光标应落在 `}}` 之后 = 插入点 + token 长度
  assert.equal(out.caret, 2 + '{{instance.biz_no}}'.length);
  assert.equal(out.text.slice(0, out.caret), '前催{{instance.biz_no}}');
});

test('选中文字时替换选中段（不是插入到选区前面）', () => {
  const source = '前【旧】后';
  const start = source.indexOf('旧');
  const out = insertPlaceholder(source, 'form.amount', start, start + 1);
  assert.equal(out.text, '前【{{form.amount}}】后');
  assert.equal(out.caret, start + '{{form.amount}}'.length);
});

test('越界 / 反向 / 非数字的选区夹紧，不抛错', () => {
  // 超出长度
  assert.equal(insertPlaceholder('ab', 'instance.title', 99, 99).text, 'ab{{instance.title}}');
  // end < start（松开鼠标时可能出现）
  const reversed = insertPlaceholder('abcd', 'form.x', 3, 1);
  assert.equal(reversed.text, 'abc{{form.x}}d');
  // NaN（老浏览器 / 计算得来）
  const nan = insertPlaceholder('ab', 'form.x', Number.NaN, Number.NaN);
  assert.equal(nan.text, 'ab{{form.x}}');
});

// ==================== ⑨ 变量选择器的分组（决策 3：按模式 / 按 biz_type 切换） ====================

test('非 TASK_ACCEPT 流程的选择器不出现 task.* 组；TASK_ACCEPT 才出现', () => {
  const normal = varGroups({ bizType: OTHER_BIZ, formFields: ['amount'] });
  assert.equal(normal.some((group) => group.key === 'task'), false);
  assert.equal(normal.some((group) => group.key === 'instance'), true);

  const taskAccept = varGroups({ bizType: TASK_ACCEPT_BIZ_TYPE });
  const taskGroup = taskAccept.find((group) => group.key === 'task');
  assert.ok(taskGroup);
  assert.deepEqual(taskGroup.options.map((item) => item.path), ['task.title', 'task.status', 'task.priority', 'task.estimate_hours']);
});

test('表单字段候选过滤掉非法名、去重、保持顺序（宁可少列，不可伪造）', () => {
  const groups = varGroups({ bizType: OTHER_BIZ, formFields: ['amount', '1bad', 'amount', 'supplier_name', 'a-b'] });
  const form = groups.find((group) => group.key === 'form');
  assert.deepEqual(form?.options.map((item) => item.path), ['form.amount', 'form.supplier_name']);
});

test('没有历史实例时表单组是空数组（组件据此只给手输，不伪造候选）', () => {
  const groups = varGroups({ bizType: OTHER_BIZ, formFields: [] });
  const form = groups.find((group) => group.key === 'form');
  assert.deepEqual(form?.options, []);
});

// ==================== ⑩ 与 render_paths 的「一层点号」口径一致 ====================

test('文法与 render.rs 的 validate_var_path 注释口径一致（最多一层点号）', () => {
  const source = renderSource();
  // 后端注释里写死了「最多允许一层点号」，前端判据与它同义
  assert.match(source, /最多允许一层点号/);
  assert.equal(pathGrammarError('a.b.c'), '占位符变量名非法：a.b.c（最多允许一层点号，如 form.amount）');
  assert.equal(pathGrammarError('a.b'), null);
});
