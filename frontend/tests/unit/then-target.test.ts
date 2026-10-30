/**
 * `then-target.ts` 单测 —— then 目标选择器的生成 / 校验 / 反解契约。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 .ts 而不是 .vue：所有「生成什么 JSON / 什么算不合法」的判定都在 `then-target.ts` 里，
 * 组件只负责渲染。生成的 JSON 必须与后端 `business/flow/expression.rs::parse_target` 逐字兼容，
 * 所以这里把**键名 / 来源枚举 / 上限数值**全写成断言 —— 后端改常量或改枚举时，这里会先红。
 *
 * 端口径提醒：卡面写的 `SUPERVISOR / INITIATOR_DEPT / STATIC` 在库与后端都不存在，
 * 真实来源是 `INITIATOR_SUPERIOR / DEPT_LEADER / USER`（见模块头注释的对照表）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MAX_ROLE_CODES,
  MAX_SUPERIOR_LEVELS,
  MAX_TARGETS,
  MAX_USER_IDS,
  RESERVED_PREV_APPROVER,
  SOURCE_OPTIONS,
  TARGET_TYPES,
  createTarget,
  describeTarget,
  formVarKeyError,
  fromThenJson,
  inputKind,
  isReserved,
  isSelectableSource,
  isTargetType,
  issueByIndex,
  toTargetJson,
  toThenJson,
  validateTargets,
  type ThenTarget,
} from '../../src/views/business/flow/components/then-target.ts';

/** 两个合法 uuid（后端只认 UUID 文本） */
const U1 = '3f2b9c1e-7a44-4d8e-9b11-2c5d6e7f8a90';
const U2 = '9a8b7c6d-1e2f-4a3b-8c4d-5e6f7a8b9c0d';
const DEPT = 'c1d2e3f4-a5b6-4c7d-8e9f-0a1b2c3d4e5f';

/** 造一个填好的目标 */
const target = (patch: Partial<ThenTarget>): ThenTarget => ({ ...createTarget(), ...patch });

/** 校验后应有的消息片段 */
const messages = (list: ThenTarget[]): string[] => validateTargets(list).map((issue) => issue.message);

// ==================== ① 10 种来源各生成正确的 JSON ====================

test('① 10 种来源逐条生成后端认得的 JSON（键名 = expression.rs 的 snake_case）', () => {
  // 顺序与后端 TARGET_TYPES 一致，一条一个形态
  const cases: Array<[ThenTarget, Record<string, unknown>]> = [
    [target({ type: 'USER', userIds: [U1] }), { type: 'USER', user_ids: [U1] }],
    [target({ type: 'ROLE', roleCodes: ['FINANCE_DIRECTOR'] }), { type: 'ROLE', role_codes: ['FINANCE_DIRECTOR'] }],
    [target({ type: 'DEPT_LEADER', deptIds: [DEPT] }), { type: 'DEPT_LEADER', dept_ids: [DEPT] }],
    [target({ type: 'INITIATOR' }), { type: 'INITIATOR' }],
    [target({ type: 'INITIATOR_SUPERIOR', levels: 2 }), { type: 'INITIATOR_SUPERIOR', levels: 2 }],
    [target({ type: 'TASK_OWNER' }), { type: 'TASK_OWNER' }],
    [target({ type: 'TASK_CREATOR' }), { type: 'TASK_CREATOR' }],
    [target({ type: 'TASK_ASSIGNER' }), { type: 'TASK_ASSIGNER' }],
    [target({ type: 'FORM_VAR', formVarKey: 'owner_id' }), { type: 'FORM_VAR', form_var_key: 'owner_id' }],
    [target({ type: 'ADMIN' }), { type: 'ADMIN' }],
  ];
  assert.equal(cases.length, 10, '来源必须是 10 种');
  for (const [input, expected] of cases) {
    const json = toTargetJson(input);
    assert.deepEqual(json, expected, `${input.type} 生成的 JSON 不对`);
    // 键顺序也要稳定（序列化文本逐字比对，方便和 curl 出来的报文核对）
    assert.equal(JSON.stringify(json), JSON.stringify(expected));
  }
  // 10 种来源与后端枚举逐字一致（拼错一个字母后端就 400）
  assert.deepEqual([...TARGET_TYPES], cases.map(([input]) => input.type));
});

test('①-b 无需输入的五种来源只落 type 一个键（后端 deny_unknown_keys 会拒多余键）', () => {
  for (const type of ['INITIATOR', 'TASK_OWNER', 'TASK_CREATOR', 'TASK_ASSIGNER', 'ADMIN']) {
    const json = toTargetJson(target({ type }));
    assert.deepEqual(json, { type });
    assert.equal(Object.keys(json as object).length, 1);
  }
});

// ==================== ② ROLE 存 role_code 而不是 role_id ====================

test('② ROLE 存的是 role_code（不是 role_id）：键名只能是 role_codes', () => {
  const json = toTargetJson(target({ type: 'ROLE', roleCodes: ['FINANCE_DIRECTOR', 'hr_admin'] }));
  assert.deepEqual(json, { type: 'ROLE', role_codes: ['FINANCE_DIRECTOR', 'hr_admin'] });
  // D-E4：绝不能出现 role_ids / role_id / id 这类跨环境会失配的键
  const keys = Object.keys(json as object);
  assert.deepEqual(keys, ['type', 'role_codes']);
  assert.ok(!keys.includes('role_ids'), '禁止 role_ids（后端会专开一条文案拒绝它）');
  assert.ok(!keys.includes('role_id'));
  // 旧数据里带 role_ids 时，反解要明确提示「改用 role_codes」
  const parsed = fromThenJson([{ type: 'ROLE', role_ids: [U1] }]);
  assert.ok(
    parsed.issues.some((issue) => issue.message.includes('role_codes') && issue.message.includes('role_ids')),
    `应提示改用 role_codes，实际：${JSON.stringify(parsed.issues)}`,
  );
});

// ==================== ③ PREV_APPROVER 不可选 ====================

test('③ PREV_APPROVER 下拉里灰显不可选，且不产出 JSON（D-E8：不落 501）', () => {
  const reserved = SOURCE_OPTIONS.find((option) => option.value === RESERVED_PREV_APPROVER);
  assert.ok(reserved, '保留词必须出现在下拉里（让用户看到「有这个能力，但本期灰着」）');
  assert.equal(reserved.disabled, true, '保留词必须 disabled');
  assert.equal(reserved.reserved, true, '保留词要挂「P2 支持」标签');
  assert.equal(isSelectableSource(RESERVED_PREV_APPROVER), false);
  assert.equal(isTargetType(RESERVED_PREV_APPROVER), false);
  assert.equal(isReserved(RESERVED_PREV_APPROVER), true);

  // 直接调 toTargetJson 也不能吐出来（防御：旧数据/手改库）
  assert.equal(toTargetJson(target({ type: RESERVED_PREV_APPROVER })), null);
  assert.deepEqual(toThenJson([target({ type: RESERVED_PREV_APPROVER })]), []);

  // 校验必须报出来（红框 + 提示），文案要点名 PREV_APPROVER 与 P2
  const list = messages([target({ type: RESERVED_PREV_APPROVER })]);
  assert.equal(list.length, 1);
  assert.match(list[0] as string, /PREV_APPROVER/);
  assert.match(list[0] as string, /P2/);

  // 旧数据里真加载到 → 一行红框（issue.index = 0），其它合法的行不受牵连
  const parsed = fromThenJson([{ type: RESERVED_PREV_APPROVER }, { type: 'ADMIN' }]);
  assert.equal(parsed.targets.length, 2);
  const byIndex = issueByIndex(parsed.issues);
  assert.ok(byIndex[0]?.includes('PREV_APPROVER'), '第 1 行必须红');
  assert.equal(byIndex[1], undefined, '第 2 行（ADMIN）不该被牵连');
});

// ==================== ④ 空 then 数组 → 校验报错 ====================

test('④ 空 then 数组 → 校验报错（整组问题，index = -1）', () => {
  const issues = validateTargets([]);
  assert.equal(issues.length, 1);
  assert.equal(issues[0]?.index, -1);
  assert.match(issues[0]?.message ?? '', /不能为空/);

  // 组件刚挂载 / 用户把最后一行删光，都是这个形态
  assert.deepEqual(toThenJson([]), []);
  assert.deepEqual(fromThenJson([]), { targets: [], issues: [] });
  // null / undefined（后端字段缺省）同样是「还没配」
  assert.deepEqual(fromThenJson(null).targets, []);
  assert.deepEqual(fromThenJson(undefined).targets, []);
});

test('④-b 超过 5 个 / 不是数组 → 整组报错，不崩', () => {
  const six = Array.from({ length: MAX_TARGETS + 1 }, () => target({ type: 'ADMIN' }));
  assert.ok(messages(six).some((message) => message.includes(`最多 ${MAX_TARGETS} 个`)));

  const notArray = fromThenJson({ type: 'ADMIN' });
  assert.deepEqual(notArray.targets, []);
  assert.match(notArray.issues[0]?.message ?? '', /必须是数组/);

  // 超长数组只保留前 5 个，其余忽略并提示
  const loaded = fromThenJson(Array.from({ length: MAX_TARGETS + 2 }, () => ({ type: 'ADMIN' })));
  assert.equal(loaded.targets.length, MAX_TARGETS);
  assert.ok(loaded.issues.some((issue) => /超过/.test(issue.message)));
});

// ==================== ⑤ STATIC 多选 → uuid 数组 ====================

test('⑤ 写死 uuid 列表（卡面的 STATIC）= USER + 多个 user_ids', () => {
  const json = toTargetJson(target({ type: 'USER', userIds: [U1, U2] }));
  assert.deepEqual(json, { type: 'USER', user_ids: [U1, U2] });
  assert.equal((json as { user_ids: string[] }).user_ids.length, 2);
  assert.ok(Array.isArray((json as { user_ids: unknown }).user_ids), '必须是数组，不是逗号串');

  // 去重 + 去空 + trim（多选控件可能塞回重复项）
  assert.deepEqual(
    toTargetJson(target({ type: 'USER', userIds: [U1, ` ${U1} `, '', U2] })),
    { type: 'USER', user_ids: [U1, U2] },
  );
  // 一个都没选 → 不合法（后端 required_uuid_array 会拒空数组）
  assert.equal(toTargetJson(target({ type: 'USER' })), null);
  assert.match(messages([target({ type: 'USER' })])[0] ?? '', /至少选 1 个人/);
  // 非 uuid → 报错（后端 Uuid::parse_str 会拒）
  assert.match(messages([target({ type: 'USER', userIds: ['zhangsan'] })])[0] ?? '', /不是合法 UUID/);
  // 超过 20 个 → 报错
  const many = Array.from({ length: MAX_USER_IDS + 1 }, (_, index) => `${index}`.padStart(8, '0') + '-0000-4000-8000-000000000000');
  assert.match(messages([target({ type: 'USER', userIds: many })])[0] ?? '', new RegExp(`最多 ${MAX_USER_IDS}`));
});

// ==================== ⑥ FORM_VAR 空输入 → 校验报错 ====================

test('⑥ FORM_VAR 空输入 / 非法字段名 → 校验报错；合法名 → form_var_key', () => {
  assert.equal(toTargetJson(target({ type: 'FORM_VAR' })), null);
  assert.match(messages([target({ type: 'FORM_VAR' })])[0] ?? '', /请填表单字段名/);

  // 与后端 is_form_var_key 同口径：字母/下划线开头，只含字母数字下划线
  assert.equal(formVarKeyError('owner_id'), null);
  assert.equal(formVarKeyError('_tmp1'), null);
  assert.ok(formVarKeyError('1owner') !== null, '数字开头不合法');
  assert.ok(formVarKeyError('owner-id') !== null, '连字符不合法');
  assert.ok(formVarKeyError('a'.repeat(65)) !== null, '超 64 字不合法');
  assert.equal(formVarKeyError('a'.repeat(64)), null);

  assert.deepEqual(toTargetJson(target({ type: 'FORM_VAR', formVarKey: ' owner_id ' })), {
    type: 'FORM_VAR',
    form_var_key: 'owner_id',
  });
  // 提示文案要告诉用户「表单里存的是 uuid」
  assert.match(formVarKeyError('') ?? '', /uuid/);
});

// ==================== ⑦ 其余来源的输入控件与边界 ====================

test('⑦ 每种来源对应的输入控件（无输入的不许有输入框）', () => {
  assert.equal(inputKind('USER'), 'users');
  assert.equal(inputKind('ROLE'), 'roles');
  assert.equal(inputKind('DEPT_LEADER'), 'dept');
  assert.equal(inputKind('INITIATOR_SUPERIOR'), 'levels');
  assert.equal(inputKind('FORM_VAR'), 'formVar');
  for (const type of ['INITIATOR', 'TASK_OWNER', 'TASK_CREATOR', 'TASK_ASSIGNER', 'ADMIN']) {
    assert.equal(inputKind(type), 'none', `${type} 不该要输入`);
  }
  // 部门负责人留空 = 发起人部门 → 省掉 dept_ids 键（后端明确允许）
  assert.deepEqual(toTargetJson(target({ type: 'DEPT_LEADER' })), { type: 'DEPT_LEADER' });
  // 层数越界 / 非整数 → 报错；边界 1 与 10 合法
  for (const levels of [0, 11, 1.5]) {
    assert.match(
      messages([target({ type: 'INITIATOR_SUPERIOR', levels })])[0] ?? '',
      new RegExp(`1–${MAX_SUPERIOR_LEVELS}`),
    );
  }
  assert.deepEqual(toTargetJson(target({ type: 'INITIATOR_SUPERIOR', levels: 1 })), {
    type: 'INITIATOR_SUPERIOR',
    levels: 1,
  });
  assert.deepEqual(toTargetJson(target({ type: 'INITIATOR_SUPERIOR', levels: MAX_SUPERIOR_LEVELS })), {
    type: 'INITIATOR_SUPERIOR',
    levels: MAX_SUPERIOR_LEVELS,
  });
  // ROLE 上限 10 个
  const eleven = Array.from({ length: MAX_ROLE_CODES + 1 }, (_, index) => `role_${index}`);
  assert.match(messages([target({ type: 'ROLE', roleCodes: eleven })])[0] ?? '', new RegExp(`最多 ${MAX_ROLE_CODES}`));
});

// ==================== ⑧ 往返：JSON → 编辑期 → JSON ====================

test('⑧ 往返稳定：JSON → 目标 → JSON 逐字一致（打开老数据不改写语义）', () => {
  const raw = [
    { type: 'ROLE', role_codes: ['FINANCE_DIRECTOR'] },
    { type: 'INITIATOR_SUPERIOR', levels: 2 },
    { type: 'USER', user_ids: [U1, U2] },
    { type: 'FORM_VAR', form_var_key: 'owner_id' },
    { type: 'ADMIN' },
  ];
  const parsed = fromThenJson(raw);
  assert.deepEqual(parsed.issues, [], '合法数据不该报问题');
  assert.equal(parsed.targets.length, 5);
  assert.deepEqual(toThenJson(parsed.targets), raw);
  assert.equal(JSON.stringify(toThenJson(parsed.targets)), JSON.stringify(raw));
});

test('⑧-b 脏数据不崩：未知来源 / 非对象项 / 缺键都渲染成可修复的一行', () => {
  const parsed = fromThenJson([{ type: 'STATIC' }, null, { type: 'USER' }, 'oops']);
  assert.equal(parsed.targets.length, 4, '每一项都要占一行，别悄悄丢掉');
  // 未知来源（卡面曾写的 STATIC）→ 报「不支持」，并列出允许值
  assert.ok(parsed.issues.some((issue) => /STATIC/.test(issue.message) && /不支持/.test(issue.message)));
  // 非对象项 → 报「必须是 JSON 对象」
  assert.ok(parsed.issues.some((issue) => /必须是 JSON 对象/.test(issue.message)));
  // 缺 user_ids 的 USER → 报「至少选 1 个人」
  assert.ok(parsed.issues.some((issue) => /至少选 1 个人/.test(issue.message)));

  // 不认识的来源不会被写回 JSON（避免把必拒的数据原样抛回后端）
  assert.ok(!JSON.stringify(toThenJson(parsed.targets)).includes('STATIC'));
  // 描述文案能让人看懂这是啥（只读态 / 折叠态展示）
  assert.match(describeTarget(target({ type: RESERVED_PREV_APPROVER })), /不支持/);
  assert.equal(describeTarget(target({ type: 'USER', userIds: [U1, U2] })), '指定用户：2 人');
  assert.equal(describeTarget(target({ type: 'INITIATOR_SUPERIOR', levels: 3 })), '发起人上级（上溯 3 级）');
});
