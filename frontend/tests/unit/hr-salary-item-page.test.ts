/**
 * 薪资项配置页（HR-44）单测：全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么
 *
 * 1. **词表镜像**：`salary-item-kinds.ts` 的类型 / 算法 / 来源三张表与考勤变量表，
 *    逐条对拍后端 `salary_item_dto.rs`（`ITEM_TYPES` / `CALC_TYPES` / `SOURCES`）与
 *    `salary_item_formula.rs`（`ATT_VARS`）—— 后端改取值而前端没跟 → 这里先红。
 * 2. **上限对齐**：编码 32 / 名称 50 / 公式 500 / 备注 500 / 科目码 30 / 顺序 999999，
 *    逐条与 Rust 常量对拍（改一边漏另一边 = 用户界面上填得进去、到后端才 400）。
 * 3. **变量插入**（验收第 2 条）：光标处插入、有选区替换、操作数相邻补空格、
 *    连续插入不粘连、越界下标不抛。
 * 4. **报错定位**（验收第 3 条）：后端三种真错误串（带「第 N 个字符」/ 点名变量 /
 *    只描述）都要换算成正确的字符区间；点错误能定位，落不到位置也要列出来。
 * 5. **公式高亮**：分词必须覆盖全文（`join('') === formula`），否则高亮错位。
 * 6. **表单校验 / 报文**：按类型分档必填科目、`SUMMARY` 必用公式、非 `FORMULA` 不发公式、
 *    空串科目不发。
 * 7. **接线**：`index.vue` 真的用了这些纯函数、真的走 `validateSalaryItemFormula`、
 *    `router` 真的注册了 `/business/hr/salary-item`。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom/jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  ATT_VARS,
  CALC_TYPE_LABELS,
  CALC_TYPE_VALUES,
  DEFAULT_PAGE_SIZE,
  DEFAULT_SOURCE,
  FORMULA_FUNCS,
  ITEM_CODE_PATTERN,
  ITEM_TYPE_LABELS,
  ITEM_TYPE_VALUES,
  MAX_ACCOUNT_CODE_LEN,
  MAX_FORMULA_LEN,
  MAX_ITEM_CODE_LEN,
  MAX_ITEM_NAME_LEN,
  MAX_PAGE_SIZE,
  MAX_REMARK_LEN,
  MAX_SORT_ORDER,
  SALARY_ITEM_COLUMNS,
  SALARY_ITEM_PERMS,
  SOURCE_LABELS,
  SOURCE_VALUES,
  accountNeeds,
  accountText,
  buildPageQuery,
  calcTypeOptions,
  dependencyTextOf,
  emptyItemFilter,
  emptyItemForm,
  fallbackVarOptions,
  formulaIssues,
  formFromRow,
  insertVariable,
  isActiveLocked,
  isItemFiltered,
  isTypeLocked,
  itemTypeOptions,
  issueLocationText,
  needsFormula,
  nextSortOrder,
  toCharIndex,
  toCodeUnitIndex,
  toWritePayload,
  tokenizeFormula,
  validateItemForm,
  validateSummary,
  validateView,
  varPanelGroups,
  type SalaryItemRow,
  type SalaryItemValidateRes,
  type SalaryItemVarOption,
} from '../../src/api/business/hr/salary-item-kinds.ts';

const here = dirname(fileURLToPath(import.meta.url));
const backend = resolve(here, '../../../backend/src/business/hr');
const readBackend = (file: string): string => readFileSync(resolve(backend, file), 'utf8');
const readFrontend = (file: string): string =>
  readFileSync(resolve(here, '../../src', file), 'utf8');

/** 造一行列表数据（只填用到的字段） */
function row(patch: Partial<SalaryItemRow> = {}): SalaryItemRow {
  return {
    id: patch.id ?? '00000000-0000-4000-8000-000000000001',
    item_code: 'BASE',
    item_name: '基本工资',
    item_type: 'EARNING',
    item_type_label: '收入项',
    calc_type: 'FIXED',
    calc_type_label: '取薪资标准',
    source: 'EMPLOYEE_STANDARD',
    source_label: '员工薪资标准',
    formula: null,
    depends_on: [],
    sort_order: 10,
    debit_account_code: '5602.10',
    credit_account_code: '2211.01',
    is_taxable: true,
    is_active: true,
    is_system: false,
    remark: null,
    create_by: null,
    create_by_name: null,
    update_by: null,
    update_by_name: null,
    created_at: '2026-11-12T00:00:00Z',
    updated_at: '2026-11-12T00:00:00Z',
    ...patch,
  };
}

// ==================== ① 词表镜像 ====================

test('类型 / 算法 / 来源三张表与后端 DTO 逐条一致（顺序也一致）', () => {
  const dto = readBackend('salary_item_dto.rs');

  // 从 `ITEM_TYPES` 常量块里按顺序取 (CODE, "中文")
  const itemBlock = /pub const ITEM_TYPES: \[\(&str, &str\); 5\] = \[([\s\S]*?)\];/.exec(dto);
  assert.ok(itemBlock, '没找到 ITEM_TYPES 常量块');
  const rustItemTypes = [...itemBlock![1].matchAll(/\((ITEM_TYPE_[A-Z_]+), "([^"]+)"\)/g)].map((m) => ({
    code: m[1],
    label: m[2],
  }));
  assert.equal(rustItemTypes.length, 5);
  assert.deepEqual(
    ITEM_TYPE_VALUES.map((code) => ITEM_TYPE_LABELS[code]),
    rustItemTypes.map((item) => item.label),
    '类型中文名与顺序必须与 ITEM_TYPES 一致',
  );

  const calcBlock = /pub const CALC_TYPES: \[\(&str, &str\); 3\] = \[([\s\S]*?)\];/.exec(dto);
  assert.ok(calcBlock, '没找到 CALC_TYPES 常量块');
  const rustCalc = [...calcBlock![1].matchAll(/\(CALC_TYPE_[A-Z_]+, "([^"]+)"\)/g)].map((m) => m[1]);
  assert.deepEqual(
    CALC_TYPE_VALUES.map((code) => CALC_TYPE_LABELS[code]),
    rustCalc,
    '算法中文名与顺序必须与 CALC_TYPES 一致',
  );

  const sourceBlock = /pub const SOURCES: \[\(&str, &str\); 5\] = \[([\s\S]*?)\];/.exec(dto);
  assert.ok(sourceBlock, '没找到 SOURCES 常量块');
  const rustSources = [...sourceBlock[1].matchAll(/\("([A-Z_]+)", "([^"]+)"\)/g)].map((m) => ({
    code: m[1],
    label: m[2],
  }));
  assert.deepEqual(
    SOURCE_VALUES.map((code) => ({ code, label: SOURCE_LABELS[code] })),
    rustSources,
    '来源码值与中文名必须与 SOURCES 一致',
  );

  // `DEFAULT_SOURCE` 也必须是后端那一个
  const defaultSource = /pub const DEFAULT_SOURCE: &str = "([A-Z_]+)"/.exec(dto)?.[1];
  assert.equal(DEFAULT_SOURCE, defaultSource);
  // 新建表单的缺省来源就是它（避免页面上手写 'INPUT' 与后端漂移）
  assert.equal(emptyItemForm().source, defaultSource);
});

test('考勤变量 11 个：变量名与中文名逐条等于 salary_item_formula::ATT_VARS', () => {
  const rust = readBackend('salary_item_formula.rs');
  const block = /pub const ATT_VARS: \[\(&str, &str, &str\); 11\] = \[([\s\S]*?)\];/.exec(rust);
  assert.ok(block, '没找到 ATT_VARS 常量块');
  const rustVars = [...block[1].matchAll(/\("([A-Z_]+)", "([a-z_]+)", "([^"]+)"\)/g)].map((m) => ({
    name: m[1],
    label: m[3],
  }));
  assert.equal(rustVars.length, 11);
  assert.deepEqual(
    ATT_VARS.map((item) => ({ name: item.name, label: item.label })),
    rustVars,
    '考勤变量名 / 中文名 / 顺序必须与 ATT_VARS 一致',
  );
});

test('函数白名单与后端 FUNCS 一致', () => {
  const rust = readBackend('salary_item_formula.rs');
  const block = /pub const FUNCS: \[&str; \d+\] = \[([\s\S]*?)\];/.exec(rust);
  if (!block) {
    // 常量可能写成切片；退一步按 `FUNCS` 行解析
    const line = /pub const FUNCS:[^=]*=\s*&?\[([^\]]+)\]/.exec(rust);
    assert.ok(line, '没找到 FUNCS 常量');
    const names = [...line[1].matchAll(/"([A-Z]+)"/g)].map((m) => m[1]);
    assert.deepEqual([...FORMULA_FUNCS], names);
    return;
  }
  const names = [...block[1].matchAll(/"([A-Z]+)"/g)].map((m) => m[1]);
  assert.deepEqual([...FORMULA_FUNCS], names, '函数白名单必须与后端 FUNCS 一致');
});

test('上限常量与后端 DTO 对拍（列宽放宽 = 用户填得进存不进）', () => {
  const dto = readBackend('salary_item_dto.rs');
  const formula = readBackend('salary_item_formula.rs');
  /** 从 DTO 常量读数字；`MAX_FORMULA_LEN` 是 `formula::MAX_FORMULA_LEN` 的别名，去公式模块读 */
  const num = (name: string): number => {
    const hit = new RegExp(`pub const ${name}: (?:usize|i32|u64) = ([0-9_]+)`).exec(dto);
    if (hit) {
      return Number(hit[1].replaceAll('_', ''));
    }
    const alias = new RegExp(`pub const ${name}: (?:usize|i32|u64) = formula::${name}`).test(dto);
    assert.ok(alias, `后端既没有常量 ${name}，也不是 formula::${name} 的别名`);
    const source = new RegExp(`pub const ${name}: usize = ([0-9_]+)`).exec(formula);
    assert.ok(source, `公式模块没有常量 ${name}`);
    return Number(source![1].replaceAll('_', ''));
  };
  assert.equal(MAX_FORMULA_LEN, num('MAX_FORMULA_LEN'));
  assert.equal(MAX_ITEM_CODE_LEN, num('MAX_ITEM_CODE_LEN'));
  assert.equal(MAX_ITEM_NAME_LEN, num('MAX_ITEM_NAME_LEN'));
  assert.equal(MAX_ACCOUNT_CODE_LEN, num('MAX_ACCOUNT_CODE_LEN'));
  assert.equal(MAX_REMARK_LEN, num('MAX_REMARK_LEN'));
  assert.equal(MAX_SORT_ORDER, num('MAX_SORT_ORDER'));
  assert.equal(DEFAULT_PAGE_SIZE, num('DEFAULT_PAGE_SIZE'));
  assert.equal(MAX_PAGE_SIZE, num('MAX_PAGE_SIZE'));
});

test('编码规则与后端 is_var_name 同口径（大写开头 + 大写/数字/下划线）', () => {
  assert.ok(ITEM_CODE_PATTERN.test('BASE'));
  assert.ok(ITEM_CODE_PATTERN.test('OT_WD'));
  assert.ok(ITEM_CODE_PATTERN.test('ATT_BONUS'));
  assert.ok(!ITEM_CODE_PATTERN.test('base'));
  assert.ok(!ITEM_CODE_PATTERN.test('1BASE'));
  assert.ok(!ITEM_CODE_PATTERN.test('OT-WD'));
  assert.ok(!ITEM_CODE_PATTERN.test(''));
});

test('权限码与后端 handler 常量逐字一致', () => {
  const handler = readBackend('salary_item_handler.rs');
  for (const code of Object.values(SALARY_ITEM_PERMS)) {
    assert.ok(handler.includes(`"${code}"`), `后端 handler 没有权限码 ${code}`);
  }
});

// ==================== ② 变量插入（验收第 2 条） ====================

test('变量插入：光标处插入 {CODE} 且光标落在变量之后', () => {
  const result = insertVariable('ROUND(, 2)', 'BASE', 6, 6);
  assert.equal(result.formula, 'ROUND({BASE}, 2)');
  assert.equal(result.caret, 6 + '{BASE}'.length);
});

test('变量插入：有选区时替换选区（改错变量最常见的动作）', () => {
  const result = insertVariable('{BASEE} + 1', 'BASE', 0, '{BASEE}'.length);
  assert.equal(result.formula, '{BASE} + 1');
  assert.equal(result.caret, '{BASE}'.length);
});

test('变量插入：两个操作数相邻时补空格（{POST}{MEAL} 后端解析不了）', () => {
  const result = insertVariable('{POST}', 'MEAL', 6, 6);
  assert.equal(result.formula, '{POST} {MEAL}');
  assert.equal(result.caret, '{POST} {MEAL}'.length);
});

test('变量插入：前面是运算符 / 括号 / 逗号时不加多余空格', () => {
  assert.equal(insertVariable('1 + ', 'BASE', 4, 4).formula, '1 + {BASE}');
  assert.equal(insertVariable('ROUND(', 'BASE', 6, 6).formula, 'ROUND({BASE}');
  assert.equal(insertVariable('MIN(a,', 'BASE', 6, 6).formula, 'MIN(a,{BASE}');
});

test('变量插入：后面紧跟操作数时补空格', () => {
  const result = insertVariable(' + 2', 'BASE', 0, 0);
  assert.equal(result.formula, '{BASE} + 2');
});

test('变量插入：下标越界 / 反序不抛，按合法区间处理', () => {
  // 负数收敛到开头（textarea 未聚焦时 selectionStart 就是 0）
  assert.equal(insertVariable('{BASE}', 'POST', -5, -1).formula, '{POST} {BASE}');
  // 超过长度收敛到结尾
  assert.equal(insertVariable('{BASE}', 'POST', 0, 99).formula, '{POST}');
  // 起点在中间、终点越界：只替换 [1, 末尾)
  assert.equal(insertVariable('{BASE}', 'POST', 1, 99).formula, '{{POST}');
  // 反序（end < start）按 [0,1) 处理：替换首字符
  const reversed = insertVariable('12', 'BASE', 2, 0);
  assert.equal(reversed.formula, '{BASE}');
});

test('连续插入两个变量不会粘在一起', () => {
  let formula = '';
  let caret = 0;
  for (const name of ['BASE', 'MEAL']) {
    const result = insertVariable(formula, name, caret, caret);
    formula = result.formula;
    caret = result.caret;
  }
  assert.equal(formula, '{BASE} {MEAL}');
  assert.ok(formula.length === caret);
});

// ==================== ③ 报错定位（验收第 3 条） ====================

test('报错定位：「公式第 N 个字符」换算成字符下标', () => {
  const formula = 'ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)';
  const issues = formulaIssues(['公式第 3 个字符 `;` 不在允许的字符里：只允许数字…'], formula);
  assert.equal(issues.length, 1);
  assert.equal(issues[0].located, true);
  assert.equal(issues[0].start, 2); // 第 3 个字符 = 下标 2
  assert.equal(issues[0].end, 3);
});

test('报错定位：未定义变量 → 落到 {BASEE} 整段', () => {
  const formula = 'ROUND({BASEE} / 21.75, 2)';
  const message =
    '公式引用了未定义的变量 `{BASEE}`：可用变量是「已有薪资项编码 + 考勤变量」—— BASE / POST';
  const issues = formulaIssues([message], formula);
  assert.equal(issues[0].located, true);
  assert.equal(issues[0].start, formula.indexOf('{BASEE}'));
  assert.equal(issues[0].end, formula.indexOf('{BASEE}') + '{BASEE}'.length);
  assert.equal(issueLocationText(issues[0], formula), `第 ${issues[0].start + 1} 个字符 {BASEE}`);
});

test('报错定位：多个未定义变量按「找得到的第一个」落位', () => {
  const formula = '{A} + {B}';
  const issues = formulaIssues(['公式引用了未定义的变量 `{ZZZ}`、`{B}`：可用变量是…'], formula);
  assert.equal(issues[0].located, true);
  assert.equal(issues[0].start, 6); // `{B}` 在下标 6
});

test('报错定位：只有描述没有位置时 located=false，消息仍保留', () => {
  const issues = formulaIssues(['公式里 `(` 没有配对的 `)`：括号不配对'], 'ROUND({BASE}, 2');
  assert.equal(issues.length, 1);
  assert.equal(issues[0].located, false);
  assert.match(issues[0].message, /没有配对/);
  assert.equal(issueLocationText(issues[0], 'ROUND({BASE}, 2'), '整条公式');
});

test('报错定位：SQL 关键字按裸词落位', () => {
  const formula = '{BASE} SELECT 1';
  const issues = formulaIssues(['公式里出现了 SQL 关键字 `SELECT`：公式是受限表达式…'], formula);
  assert.equal(issues[0].located, true);
  assert.equal(issues[0].start, formula.indexOf('SELECT'));
});

test('报错定位：为空公式时不会抛（区间收敛在 0）', () => {
  const issues = formulaIssues(['公式不能为空：`calc_type = FORMULA` 的薪资项必须给公式'], '');
  assert.equal(issues[0].located, false);
  assert.equal(issues[0].start, 0);
});

test('字符 / UTF-16 下标互换：中文粘进公式也不会偏一格', () => {
  const text = '中文{BASE}';
  assert.equal(toCharIndex(text, 2), 2);
  assert.equal(toCodeUnitIndex(text, 2), 2);
  const emoji = '😀{BASE}';
  assert.equal(toCharIndex(emoji, 2), 1); // UTF-16 里 😀 占 2
  assert.equal(toCodeUnitIndex(emoji, 1), 2);
});

// ==================== ④ 分词（高亮必须覆盖全文） ====================

test('分词覆盖全文：join 回去与原文逐字相等', () => {
  const cases = [
    'ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)',
    'IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)',
    '{BASE}+{POST}+{MEAL}+{PERF}',
    '  ',
    '',
    '{BASEE} + 1',
    '{BAD',
  ];
  for (const formula of cases) {
    const tokens = tokenizeFormula(formula);
    assert.equal(tokens.map((token) => token.text).join(''), formula, `分词丢字：${formula}`);
  }
});

test('分词分类：变量 / 函数 / 数字 / 运算符各归其类', () => {
  const tokens = tokenizeFormula('ROUND({BASE} / 21.75, 2)');
  const kinds = tokens.map((token) => `${token.kind}:${token.text}`);
  assert.ok(kinds.includes('func:ROUND'));
  assert.ok(kinds.includes('var:{BASE}'));
  assert.ok(kinds.includes('number:21.75'));
  assert.ok(kinds.includes('op:/'));
  assert.ok(kinds.includes('punct:('));
});

test('分词：不合法变量名与未闭合花括号着色为 var-invalid', () => {
  const invalid = tokenizeFormula('{base} + 1');
  assert.equal(invalid[0].kind, 'var-invalid');
  const unclosed = tokenizeFormula('{BASE');
  assert.equal(unclosed[0].kind, 'var-invalid');
  assert.equal(unclosed[0].end, 5);
});

test('分词：小写函数名不算 func（后端会提示「函数名请用大写」）', () => {
  const tokens = tokenizeFormula('round({BASE}, 2)');
  assert.equal(tokens[0].kind, 'text');
  assert.equal(tokens[0].text, 'round');
});

// ==================== ⑤ 表单校验 / 报文 ====================

test('表单校验：新建缺编码 / 名称必报，编辑不校验编码', () => {
  const form = { ...emptyItemForm(), item_name: '' };
  const creating = validateItemForm(form, { creating: true });
  assert.ok(creating.item_code);
  assert.ok(creating.item_name);
  const editing = validateItemForm({ ...form, item_code: '' }, { creating: false });
  assert.equal(editing.item_code, undefined);
});

test('表单校验：编码不能与考勤变量同名', () => {
  const form = { ...emptyItemForm(), item_code: 'ATT_OT_WD', item_name: '撞名' };
  assert.match(validateItemForm(form, { creating: true }).item_code ?? '', /考勤变量同名/);
});

test('表单校验：科目按类型分档必填（SUMMARY 免、WITHHOLD 只贷方）', () => {
  assert.deepEqual(accountNeeds('EARNING'), { debit: true, credit: true });
  assert.deepEqual(accountNeeds('PRE_DEDUCT'), { debit: true, credit: true });
  assert.deepEqual(accountNeeds('EMPLOYER'), { debit: true, credit: true });
  assert.deepEqual(accountNeeds('WITHHOLD'), { debit: false, credit: true });
  assert.deepEqual(accountNeeds('SUMMARY'), { debit: false, credit: false });

  const withholding = validateItemForm(
    { ...emptyItemForm(), item_code: 'SI_PERSONAL', item_name: '社保个人', item_type: 'WITHHOLD' },
    { creating: true },
  );
  assert.equal(withholding.debit_account_code, undefined);
  assert.ok(withholding.credit_account_code);

  const summary = validateItemForm(
    { ...emptyItemForm(), item_code: 'GROSS', item_name: '应发合计', item_type: 'SUMMARY', calc_type: 'FORMULA', formula: '{BASE}' },
    { creating: true },
  );
  assert.equal(Object.keys(summary).length, 0, `汇总项不该要科目：${JSON.stringify(summary)}`);
});

test('表单校验：汇总项必须用公式算法；FORMULA 必须给公式', () => {
  const summary = validateItemForm(
    { ...emptyItemForm(), item_code: 'GROSS', item_name: '应发合计', item_type: 'SUMMARY', calc_type: 'FIXED' },
    { creating: true },
  );
  assert.match(summary.calc_type ?? '', /汇总项必须用「公式计算」/);

  const formulaMissing = validateItemForm(
    { ...emptyItemForm(), item_code: 'OT_WD', item_name: '加班费', calc_type: 'FORMULA' },
    { creating: true },
  );
  assert.match(formulaMissing.formula ?? '', /必须给公式/);
});

test('表单校验：公式超 500 字、顺序越界都要拦住（不等后端 400）', () => {
  const longFormula = validateItemForm(
    {
      ...emptyItemForm(),
      item_code: 'OT_WD',
      item_name: '加班费',
      calc_type: 'FORMULA',
      formula: '1+'.repeat(300),
    },
    { creating: true },
  );
  assert.match(longFormula.formula ?? '', /最长 500 个字符/);

  const badSort = validateItemForm(
    { ...emptyItemForm(), item_code: 'OT_WD', item_name: '加班费', sort_order: '1000000' },
    { creating: true },
  );
  assert.match(badSort.sort_order ?? '', /0~999999/);
});

test('写入报文：非 FORMULA 不发公式、空科目不发、顺序空串不发', () => {
  const payload = toWritePayload({
    ...emptyItemForm(),
    item_code: ' BASE ',
    item_name: ' 基本工资 ',
    formula: '{IGNORED}',
    calc_type: 'FIXED',
    sort_order: '',
    debit_account_code: ' ',
  });
  assert.equal(payload.item_code, 'BASE');
  assert.equal(payload.item_name, '基本工资');
  assert.equal(payload.formula, undefined);
  assert.equal(payload.sort_order, undefined);
  assert.equal(payload.debit_account_code, undefined);
  assert.equal(needsFormula('FIXED'), false);
  assert.equal(needsFormula('FORMULA'), true);
});

test('写入报文：FORMULA 带公式、顺序转数字', () => {
  const payload = toWritePayload({
    ...emptyItemForm(),
    item_code: 'OT_WD',
    item_name: '平时加班费',
    calc_type: 'FORMULA',
    formula: ' ROUND({BASE} / 21.75, 2) ',
    sort_order: '50',
    debit_account_code: '5602.10',
    credit_account_code: '2211.01',
  });
  assert.equal(payload.formula, 'ROUND({BASE} / 21.75, 2)');
  assert.equal(payload.sort_order, 50);
});

test('表单 ↔ 行往返：formFromRow 不丢字段，系统项两个开关都锁', () => {
  const gross = row({
    item_code: 'GROSS',
    item_name: '应发合计',
    item_type: 'SUMMARY',
    calc_type: 'FORMULA',
    formula: '{BASE}+{POST}',
    is_system: true,
    debit_account_code: null,
    credit_account_code: null,
  });
  const form = formFromRow(gross);
  assert.equal(form.formula, '{BASE}+{POST}');
  assert.equal(form.debit_account_code, '');
  assert.equal(isTypeLocked(gross), true);
  assert.equal(isActiveLocked(gross), true);
  assert.equal(isTypeLocked(row()), false);
  assert.equal(isActiveLocked(row()), false);
});

// ==================== ⑥ 筛选 / 列 / 建议顺序 ====================

test('查询串：空筛选不发空参数，is_active 转数字', () => {
  assert.deepEqual(buildPageQuery(emptyItemFilter(), 1), { page: 1, page_size: DEFAULT_PAGE_SIZE });
  assert.equal(isItemFiltered(emptyItemFilter()), false);

  const query = buildPageQuery(
    { keyword: ' 加班 ', item_type: 'EARNING', calc_type: '', is_active: '0' },
    3,
    50,
  );
  assert.deepEqual(query, {
    page: 3,
    page_size: 50,
    keyword: '加班',
    item_type: 'EARNING',
    is_active: 0,
  });
  assert.equal(isItemFiltered({ keyword: '', item_type: '', calc_type: '', is_active: '0' }), true);
});

test('查询串：页码 / 每页大小归一（0 与超限不发给后端）', () => {
  assert.equal(buildPageQuery(emptyItemFilter(), 0, 0).page, 1);
  assert.equal(buildPageQuery(emptyItemFilter(), 1, 99999).page_size, MAX_PAGE_SIZE);
});

test('列定义覆盖设计 §6.2.4 的八列 + 操作列', () => {
  const keys = SALARY_ITEM_COLUMNS.map((column) => column.colKey);
  for (const expected of [
    'sort_order',
    'item_code',
    'item_name',
    'item_type',
    'calc_type',
    'formula',
    'account',
    'is_active',
    'op',
  ]) {
    assert.ok(keys.includes(expected), `缺列 ${expected}`);
  }
});

test('科目 / 公式列文本：空值显示 — 而不是空白', () => {
  assert.equal(accountText({ debit_account_code: '5602.10', credit_account_code: '2211.01' }), '5602.10 / 2211.01');
  assert.equal(accountText({ debit_account_code: null, credit_account_code: null }), '— / —');
  assert.equal(nextSortOrder([]), 10);
  assert.equal(nextSortOrder([row({ sort_order: 150 }), row({ sort_order: 20 })]), 160);
});

// ==================== ⑦ 检查回执 → 展示 ====================

test('检查回执：失败时 issues 可定位、标题带问题数', () => {
  const res: SalaryItemValidateRes = {
    ok: false,
    errors: ['公式引用了未定义的变量 `{BASEE}`：可用变量是…'],
    refs: ['BASEE'],
    item_refs: ['BASEE'],
    att_refs: [],
    undefined: ['BASEE'],
    dependency_order: [],
    cycle: null,
    available: [],
  };
  const view = validateView(res, 'ROUND({BASEE}, 2)');
  assert.equal(view.ok, false);
  assert.equal(view.issues.length, 1);
  assert.equal(view.issues[0].located, true);
  assert.equal(validateSummary(view), '公式未通过：1 处问题');
});

test('检查回执：通过时展示依赖顺序；无依赖时显示人话', () => {
  const res: SalaryItemValidateRes = {
    ok: true,
    errors: [],
    refs: ['BASE', 'ATT_OT_WD'],
    item_refs: ['BASE'],
    att_refs: ['ATT_OT_WD'],
    undefined: [],
    dependency_order: ['BASE', 'OT_WD'],
    cycle: null,
    available: [],
  };
  const view = validateView(res, 'ROUND({BASE} / 8 * {ATT_OT_WD}, 2)');
  assert.equal(validateSummary(view), '语法通过 · 依赖顺序 BASE → OT_WD');
  assert.equal(dependencyTextOf([]), '');
  assert.equal(validateSummary(validateView({ ...res, dependency_order: [] }, 'x')), '语法通过 · 无薪资项依赖');
});

test('检查回执：环的顺序原样拼给人看', () => {
  const view = validateView(
    {
      ok: false,
      errors: ['公式存在循环依赖'],
      refs: [],
      item_refs: [],
      att_refs: [],
      undefined: [],
      dependency_order: [],
      cycle: ['A', 'B', 'A'],
      available: [],
    },
    '{A}+{B}',
  );
  assert.equal(view.cycleText, 'A → B → A');
});

// ==================== ⑧ 变量面板 ====================

test('变量面板：薪资项 / 考勤两组，自己灰掉、停用标记', () => {
  // 后端的 `available` 会**带上本项自己**（`available_vars` 是「全部项 + 考勤」），
  // 面板负责把它灰掉（自引用是环，后端检查才能报，但不应鼓励点）。
  const available: SalaryItemVarOption[] = [
    { name: 'BASE', label: 'BASE', kind: 'ITEM', usable: true },
    { name: 'GROSS', label: 'GROSS', kind: 'ITEM', usable: true },
    { name: 'OLD', label: 'OLD', kind: 'ITEM', usable: false },
    ...ATT_VARS.map((item) => ({
      name: item.name,
      label: item.label,
      kind: 'ATTENDANCE',
      usable: true,
    })),
  ];
  const groups = varPanelGroups(available, { selfCode: 'GROSS', itemNames: { BASE: '基本工资' } });
  assert.equal(groups.length, 2);
  assert.equal(groups[0].key, 'ITEM');
  assert.equal(groups[1].key, 'ATTENDANCE');
  assert.equal(groups[0].items.length, 3);
  assert.equal(groups[1].items.length, 11, '考勤组必须装考勤变量（不是把薪资项再塞一遍）');

  const byName = new Map(groups[0].items.map((item) => [item.name, item]));
  assert.equal(byName.get('GROSS')?.usable, false, '自己不能引用自己');
  assert.equal(byName.get('OLD')?.usable, false, '停用项标不可用');
  assert.equal(byName.get('BASE')?.usable, true);
  assert.equal(byName.get('BASE')?.label, '基本工资', '用列表里的中文名补面板显示');
});

test('变量面板：兜底清单不含本项，考勤变量全都在', () => {
  const rows = [row(), row({ id: '2', item_code: 'GROSS', item_name: '应发合计' })];
  const options = fallbackVarOptions(rows, 'BASE');
  assert.ok(!options.some((item) => item.name === 'BASE'));
  for (const name of ATT_VARS.map((item) => item.name)) {
    assert.ok(options.some((item) => item.name === name), `缺考勤变量 ${name}`);
  }
});

// ==================== ⑨ 接线（源码断言） ====================

test('页面真的用了纯函数与检查接口（不是自己另写一套）', () => {
  const page = readFrontend('views/business/hr/salary-item/index.vue');
  for (const symbol of [
    'insertVariable',
    'validateItemForm',
    'validateSalaryItemFormula',
    'tokenizeFormula',
    'validateView',
    'varPanelGroups',
    'fallbackVarOptions',
    'toWritePayload',
  ]) {
    assert.ok(page.includes(symbol), `页面没用 ${symbol}`);
  }
  // 检查按钮与变量面板都要真的在模板里
  assert.ok(page.includes('hr-si-check'), '缺「检查」按钮');
  assert.ok(page.includes('hr-si-var-panel'), '缺变量面板');
  assert.ok(page.includes('hr-si-formula'), '缺公式输入框');
  // 报错定位：点错误要把选区落到 textarea
  assert.ok(page.includes('setSelectionRange'), '错误定位没有落光标到 textarea');
  assert.ok(page.includes('focusIssue'), '错误清单没有点击定位');
});

test('API 层：4 条接口路径写全 /api（漏前缀会命中 SPA fallback）', () => {
  const api = readFrontend('api/business/hr/salary-item.ts');
  assert.ok(api.includes("const BASE = '/api/business/hr'"), 'BASE 缺 /api 前缀');
  assert.ok(api.includes("`${BASE}/salary-item/page`"));
  assert.ok(api.includes("`${BASE}/salary-item`"));
  assert.ok(api.includes("`${BASE}/salary-item/${id}`"));
  assert.ok(api.includes("`${BASE}/salary-item/validate`"));
  // 没有 DELETE（配置表生命周期是停用，后端无路由）；注释里可以提这个名字，代码里不许有
  assert.ok(
    !/(^|\n)\s*(export\s+)?(async\s+)?(function|const)\s+deleteSalaryItem/.test(api),
    '不该有删除接口',
  );
  assert.ok(!/\bdel</.test(api), '不该 import 删除请求');
});

test('router 注册了 /business/hr/salary-item（漏了 = 点菜单 404）', () => {
  const router = readFrontend('router/index.ts');
  assert.ok(router.includes("path: 'business/hr/salary-item'"), 'router 没注册薪资项路由');
  assert.ok(router.includes("'@/views/business/hr/salary-item/index.vue'"), '路由组件路径不对');
  assert.ok(router.includes("permission: 'business:hr:salary-item:list'"), '路由权限码不对');
});

test('菜单种子 / 权限码与 V0387 一致（页面能用得上这 3 个码）', () => {
  const seed = readFileSync(
    resolve(here, '../../../backend/migrations/business/V0387__hr_menu_perms.up.sql'),
    'utf8',
  );
  assert.ok(seed.includes("'/business/hr/salary-item'"), 'V0387 没有薪资项菜单路径');
  for (const code of Object.values(SALARY_ITEM_PERMS)) {
    assert.ok(seed.includes(`'${code}'`), `V0387 没有权限码 ${code}`);
  }
});

test('下拉选项顺序与词表一致（类型 5 / 算法 3）', () => {
  assert.deepEqual(
    itemTypeOptions().map((option) => option.value),
    [...ITEM_TYPE_VALUES],
  );
  assert.deepEqual(
    calcTypeOptions().map((option) => option.value),
    [...CALC_TYPE_VALUES],
  );
});
