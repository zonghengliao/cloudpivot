/**
 * `flow-node-caps.ts` / `task-node-config.ts` / `flow-actions.ts` / `flow-error-codes.ts` 单测
 * —— P-FLOW-1C 的四条硬口径。
 *
 * 运行：`pnpm test:unit`
 *
 * # 这份测试挡住什么
 *
 * 1. **节点能力表**：新增两种节点后，`NodeProps` / `FlowCanvas` 读的是同一张表；
 *    `graph.ts::needsAssignee` 与表里的 `canHaveAssignee` 必须一致（两处各写一份，单测钉住）。
 * 2. **R18 在 UI 上不可违反**（卡面验收 2）：不是「人手点 4 遍」，而是把 4 种操作顺序
 *    连同全部枚举切换**穷举**跑一遍，断言每一步的结果都过 `checkMatrix`。
 * 3. **流程任务只有一个主按钮**（决策 2）：`applyFlowMainButton` 的收敛与文案。
 * 4. **错误码逐字对齐**：65034–65039 与 `error_code.rs` 相等（否则 UI 会标错码）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import {
  CAPS,
  assigneeKindOptionsOf,
  capabilitiesOf,
  isNotifyNode,
  isTaskNode,
  needsAssigneeOf,
  timeoutActionOptionsOf,
  type NodeType,
} from '../../src/views/business/flow/components/flow-node-caps.ts';
import {
  DEFAULT_TASK_CONFIG,
  ON_COMPLETE_OPTIONS,
  applyNeedReview,
  applyOnComplete,
  approveOptionDisabled,
  checkMatrix,
  dueIssue,
  needReviewLocked,
  needReviewTooltip,
  approveBizTypeIssue,
  taskConfigOf,
  taskConfigPatch,
  type OnComplete,
  type TaskConfig,
} from '../../src/views/business/flow/components/task-node-config.ts';
import {
  applyFlowMainButton,
  flowMainKey,
  flowMainLabel,
  flowMainNote,
} from '../../src/views/business/task/components/flow-actions.ts';
import {
  FLOW_NODE_CONFIG_MISMATCH,
  FLOW_NOTIFY_TEMPLATE_INVALID,
  FLOW_PREVIEW_INVALID,
  FLOW_TASK_ALREADY_DONE,
  FLOW_TASK_NODE_INVALID,
  FLOW_TASK_STATE_CONFLICT,
  FLOW_TEMPLATE_VAR_INVALID,
  TASK_NODE_CODES,
  taskNodeErrorText,
} from '../../src/views/business/flow/components/flow-error-codes.ts';
import { NODE_TYPES, needsAssignee } from '../../src/views/business/flow/graph.ts';

/** 读 `backend/src/shared/error_code.rs` */
function errorCodeSource(): string {
  return readFileSync(
    fileURLToPath(new URL('../../../backend/src/shared/error_code.rs', import.meta.url)),
    'utf8',
  );
}

/** 从 rust 源码里抠一个 `pub const NAME: type = 值;` */
function rustConst(source: string, name: string): string {
  const matched = new RegExp(`pub const ${name}:\\s*\\w+\\s*=\\s*([^;]+);`).exec(source);
  assert.ok(matched, `后端源码里找不到常量 ${name}`);
  return (matched[1] as string).trim();
}

/** 全部 9 种节点类型（表的键） */
const ALL_TYPES = Object.keys(CAPS) as NodeType[];

/** 一份合法的起点配置 */
const baseConfig = (): TaskConfig => ({ ...DEFAULT_TASK_CONFIG, title_template: '{{instance.biz_no}} 催货' });

// ==================== ① 节点能力表 ====================

test('能力表覆盖 9 种节点，两种新节点各自的 config 分区正确', () => {
  assert.equal(ALL_TYPES.length, 9);
  for (const type of ['START', 'APPROVAL', 'COUNTERSIGN', 'CONDITION', 'PARALLEL', 'CC', 'END', 'TASK_NODE', 'NOTIFY_NODE'] as NodeType[]) {
    assert.ok(ALL_TYPES.includes(type), `能力表缺 ${type}`);
  }
  assert.equal(CAPS.TASK_NODE.needsConfig, 'task');
  assert.equal(CAPS.NOTIFY_NODE.needsConfig, 'notify');
  // 出入口不可编辑（固定节点）
  assert.equal(CAPS.START.editable, false);
  assert.equal(CAPS.END.editable, false);
  // 任务节点阻塞等人（要有人干活），通知节点不阻塞
  assert.equal(CAPS.TASK_NODE.blocksOnHuman, true);
  assert.equal(CAPS.NOTIFY_NODE.blocksOnHuman, false);
  // 只有条件节点的出边能带条件
  assert.deepEqual(ALL_TYPES.filter((type) => CAPS[type].canHaveConditionOut), ['CONDITION']);
});

test('capabilitiesOf 对未知类型兜底（不抛错），大小写不敏感', () => {
  const unknown = capabilitiesOf('SOMETHING_NEW');
  assert.equal(unknown.editable, true);
  assert.equal(unknown.needsConfig, null);
  assert.equal(capabilitiesOf('task_node').needsConfig, 'task');
  assert.equal(capabilitiesOf(null).needsConfig, null);
});

test('graph.ts 的节点清单与能力表一致（含新增两种；needsAssignee 同口径）', () => {
  const types = NODE_TYPES.map((item) => item.type);
  assert.ok(types.includes('TASK_NODE'));
  assert.ok(types.includes('NOTIFY_NODE'));
  // 每种类型都能在能力表里查到（防「拖得进来但属性面板不知道它」）
  for (const type of types) {
    assert.ok(type in CAPS, `能力表缺 ${type}`);
  }
  for (const type of ALL_TYPES) {
    assert.equal(needsAssignee(type), CAPS[type].canHaveAssignee, `${type} 的两处判据不一致`);
  }
  assert.equal(isTaskNode('TASK_NODE'), true);
  assert.equal(isNotifyNode('NOTIFY_NODE'), true);
  assert.equal(needsAssigneeOf('TASK_NODE'), true);
});

test('TASK_NODE 的指派来源：非 TASK_ACCEPT 流程灰掉三个任务来源', () => {
  const normal = assigneeKindOptionsOf('TASK_NODE', 'PURCHASE_ORDER');
  const disabled = normal.filter((item) => item.disabled).map((item) => item.value);
  assert.deepEqual(disabled.sort(), ['TASK_ASSIGNER', 'TASK_CREATOR', 'TASK_OWNER']);
  // TASK_ACCEPT 流程里三个都能用
  const taskAccept = assigneeKindOptionsOf('TASK_NODE', 'TASK_ACCEPT');
  assert.equal(taskAccept.filter((item) => item.disabled).length, 0);
  // 审批节点上不受影响
  assert.equal(assigneeKindOptionsOf('APPROVAL', 'PURCHASE_ORDER').filter((item) => item.disabled).length, 0);
});

test('TASK_NODE 的超时动作灰掉「自动通过 / 自动驳回」（活没干完不能算干完）', () => {
  const options = timeoutActionOptionsOf('TASK_NODE');
  const disabled = options.filter((item) => item.disabled).map((item) => item.value);
  assert.deepEqual(disabled.sort(), ['AUTO_APPROVE', 'AUTO_REJECT']);
  assert.equal(timeoutActionOptionsOf('APPROVAL').filter((item) => item.disabled).length, 0);
});

// ==================== ② R18：4 种操作顺序都产不出非法组合（卡面验收 2） ====================

test('顺序①：先选 APPROVE → 再想勾验收（禁用）→ 仍是 APPROVE + 不验收', () => {
  let config = baseConfig();
  config = applyOnComplete(config, 'APPROVE');
  assert.equal(config.on_complete, 'APPROVE');
  assert.equal(config.need_review, false);
  // 勾选框此时是禁用的（UI 点不到）；即便脏数据硬勾，applyNeedReview 也会把组合修正回合法
  assert.equal(needReviewLocked(config), true);
  config = applyNeedReview(config, true);
  assert.equal(checkMatrix(config), null);
  assert.equal(config.on_complete, 'VERIFY');
  assert.equal(config.need_review, true);
});

test('顺序②：先勾「需验收」→ 再选 APPROVE（灰掉）→ 落成 APPROVE + 不验收仍合法', () => {
  let config = baseConfig();
  config = applyNeedReview(config, true);
  assert.equal(config.need_review, true);
  // 「完成后审批」此时灰掉
  assert.equal(approveOptionDisabled(config), true);
  config = applyOnComplete(config, 'APPROVE');
  assert.equal(config.on_complete, 'APPROVE');
  assert.equal(config.need_review, false);
  assert.equal(checkMatrix(config), null);
});

test('顺序③：选 VERIFY → 自动勾验收并锁定 → 想取消验收（禁用）→ 落成 DIRECT + 不验收', () => {
  let config = baseConfig();
  config = applyOnComplete(config, 'VERIFY');
  assert.equal(config.on_complete, 'VERIFY');
  assert.equal(config.need_review, true);
  assert.equal(needReviewLocked(config), true);
  assert.match(needReviewTooltip(config), /不能关/);
  config = applyNeedReview(config, false);
  assert.equal(config.on_complete, 'DIRECT');
  assert.equal(config.need_review, false);
  assert.equal(checkMatrix(config), null);
});

test('顺序④：DIRECT → 勾验收 → VERIFY → APPROVE → 回 DIRECT（每一步都合法）', () => {
  let config = baseConfig();
  const steps: Array<[string, () => void]> = [
    ['勾验收', () => (config = applyNeedReview(config, true))],
    ['选 VERIFY', () => (config = applyOnComplete(config, 'VERIFY'))],
    ['选 APPROVE', () => (config = applyOnComplete(config, 'APPROVE'))],
    ['选 DIRECT', () => (config = applyOnComplete(config, 'DIRECT'))],
  ];
  for (const [name, run] of steps) {
    run();
    assert.equal(checkMatrix(config), null, `第 ${name} 步之后矩阵非法了`);
  }
});

test('穷举：任意状态 × 任意一次操作，结果都过 R18 矩阵', () => {
  const onCompletes: OnComplete[] = ['DIRECT', 'VERIFY', 'APPROVE'];
  for (const onComplete of onCompletes) {
    for (const needReview of [false, true]) {
      const start: TaskConfig = { ...baseConfig(), on_complete: onComplete, need_review: needReview };
      for (const next of onCompletes) {
        assert.equal(checkMatrix(applyOnComplete(start, next)), null, `${onComplete}/${needReview} → 选 ${next}`);
      }
      for (const checked of [false, true]) {
        assert.equal(checkMatrix(applyNeedReview(start, checked)), null, `${onComplete}/${needReview} → 勾选 ${checked}`);
      }
    }
  }
});

test('checkMatrix 自身能判出两条违例（否则上面的穷举是空的）', () => {
  assert.match(checkMatrix({ ...baseConfig(), on_complete: 'VERIFY', need_review: false }) ?? '', /VERIFY/);
  assert.match(checkMatrix({ ...baseConfig(), on_complete: 'APPROVE', need_review: true }) ?? '', /点两次/);
  assert.equal(checkMatrix({ ...baseConfig(), on_complete: 'DIRECT', need_review: true }), null);
});

test('APPROVE 不再是阶段闸：只剩 approve_biz_type 必填校验（P-FLOW-1B 收口）', () => {
  const approve = { ...baseConfig(), on_complete: 'APPROVE' as OnComplete };
  // 选了 APPROVE 就报警的那条 1A 文案已删除：配好 biz_type 时**不留任何提示**
  assert.equal(approveBizTypeIssue({ ...approve, approve_biz_type: 'PURCHASE_ORDER' }), null);
  // 没配 biz_type 才是必填问题（且只提必填，不提「版本不支持」）
  const missing = approveBizTypeIssue({ ...approve, approve_biz_type: null });
  assert.ok(missing && missing.includes('必须指定') && missing.includes('审批流'));
  assert.equal(approveBizTypeIssue({ ...approve, approve_biz_type: '   ' }) !== null, true);
  // 非 APPROVE 一律不提示
  assert.equal(approveBizTypeIssue({ ...baseConfig(), on_complete: 'DIRECT', approve_biz_type: null }), null);
  assert.equal(approveBizTypeIssue({ ...baseConfig(), on_complete: 'VERIFY', need_review: true }), null);
  // 下拉 hint 不再写版本号，改成「需指定审批流 biz_type」
  const option = ON_COMPLETE_OPTIONS.find((item) => item.value === 'APPROVE');
  assert.equal(option?.value, 'APPROVE');
  const hint = option?.hint ?? '';
  assert.ok(!hint.includes('P-FLOW-1B'));
  assert.match(hint, /biz_type/);
});

test('源码里不再有 1A 阶段闸的死码（删干净 = 验收 2/3）', () => {
  const dir = fileURLToPath(new URL('../../src/views/business/flow/components/', import.meta.url));
  const files = ['task-node-config.ts', 'TaskNodeConfig.vue', 'flow-error-codes.ts', 'TaskPreview.vue'];
  for (const file of files) {
    const source = readFileSync(`${dir}${file}`, 'utf8');
    for (const banned of ['stageGateError', 'stageGateText', 'P-FLOW-1A 版本尚未开放', '发布会被拒绝']) {
      assert.ok(!source.includes(banned), `${file} 里还残留「${banned}」`);
    }
  }
  // R18 只剩两条（与后端 node_config.rs::check_matrix 恰好对齐）
  assert.equal(checkMatrix({ ...baseConfig(), on_complete: 'APPROVE', need_review: false }), null);
  assert.match(checkMatrix({ ...baseConfig(), on_complete: 'APPROVE', need_review: true }) ?? '', /点两次/);
  assert.match(checkMatrix({ ...baseConfig(), on_complete: 'VERIFY', need_review: false }) ?? '', /VERIFY/);
});

// ==================== ③ config.task 的反解 / 打包 ====================

test('taskConfigOf 对脏数据兜底（缺键 / 未知枚举 / 非对象都不抛错）', () => {
  assert.deepEqual(taskConfigOf(null).on_complete, 'DIRECT');
  assert.deepEqual(taskConfigOf({}).title_template, '');
  const dirty = taskConfigOf({
    config: { task: { title_template: 'x', on_complete: 'WHATEVER', need_review: 'yes', due: { mode: 'NOPE' } } },
  });
  assert.equal(dirty.on_complete, 'DIRECT');
  assert.equal(dirty.need_review, false); // 只有 true 才算 true
  assert.equal(dirty.due, null);
  assert.equal(dirty.on_reject, 'BACK_TO_PREV'); // 与后端默认值一致
  assert.equal(dirty.on_cancel, 'TERMINATE');
});

test('taskConfigPatch：非 APPROVE 不带 approve_biz_type，空描述不落键（否则 65034）', () => {
  const patch = taskConfigPatch({ ...baseConfig(), description_template: null, approve_biz_type: 'TASK_ACCEPT' });
  const task = patch.config.task;
  assert.equal('description_template' in task, false);
  assert.equal('approve_biz_type' in task, false);
  assert.equal('due' in task, false);
  assert.equal(task.title_template, baseConfig().title_template);
});

test('taskConfigPatch：APPROVE 带 approve_biz_type；RELATIVE 期限只带 amount+unit', () => {
  const patch = taskConfigPatch({
    ...baseConfig(),
    on_complete: 'APPROVE',
    approve_biz_type: 'TASK_ACCEPT',
    due: { mode: 'RELATIVE', amount: 3, unit: 'WORKDAY' },
  });
  assert.equal(patch.config.task.approve_biz_type, 'TASK_ACCEPT');
  assert.deepEqual(patch.config.task.due, { mode: 'RELATIVE', amount: 3, unit: 'WORKDAY' });
  // 切到 ABSOLUTE 时不该把 amount / unit 带上（后端 deny_unknown_keys 会 65034）
  const absolute = taskConfigPatch({ ...baseConfig(), due: { mode: 'ABSOLUTE', date: '2026-11-15', amount: 3 } });
  assert.deepEqual(absolute.config.task.due, { mode: 'ABSOLUTE', date: '2026-11-15' });
});

test('dueIssue：三种模式的必备键与上限', () => {
  assert.equal(dueIssue(null), null);
  assert.match(dueIssue({ mode: 'RELATIVE', amount: 0, unit: 'DAY' }) ?? '', /≥1/);
  assert.match(dueIssue({ mode: 'RELATIVE', amount: 3 }) ?? '', /单位/);
  assert.equal(dueIssue({ mode: 'RELATIVE', amount: 3, unit: 'WORKDAY' }), null);
  assert.match(dueIssue({ mode: 'ABSOLUTE', date: '2026/11/15' }) ?? '', /yyyy-MM-dd/);
  assert.match(dueIssue({ mode: 'FIELD', field: 'instance.biz_no' }) ?? '', /form\./);
  assert.equal(dueIssue({ mode: 'FIELD', field: 'form.delivery_date' }), null);
});

// ==================== ④ 流程任务只有一个主按钮（决策 2） ====================

/** 一条按钮的最小形状 */
const action = (key: string, label: string) => ({ key, label, to: key, theme: 'primary' as const });

/** `DOING` 态普通任务原本的两个完成类按钮（白名单 + need_review 过滤后的样子） */
const doingActions = () => [action('submit-review', '提交验收'), action('direct-done', '直接完成'), action('cancel', '取消')];

test('非流程任务：按钮组原样返回（普通任务行为一个字不变）', () => {
  const list = doingActions();
  assert.deepEqual(applyFlowMainButton(list, null), list);
});

test('流程任务 DIRECT：只剩「完成」+ 取消（不再并列「提交验收」）', () => {
  const list = applyFlowMainButton(doingActions(), { on_complete: 'DIRECT', need_review: false });
  assert.deepEqual(list.map((item) => item.key), ['direct-done', 'cancel']);
  assert.equal(list[0]?.label, '完成');
  assert.equal(flowMainKey({ on_complete: 'DIRECT', need_review: false }), 'direct-done');
});

test('流程任务 VERIFY：只剩「提交验收」', () => {
  // need_review=true 时 `direct-done` 本来就被白名单过滤掉，这里两种输入都要收敛到同一个结果
  const list = applyFlowMainButton(doingActions(), { on_complete: 'VERIFY', need_review: true });
  assert.deepEqual(list.map((item) => item.key), ['submit-review', 'cancel']);
  assert.equal(list[0]?.label, '提交验收');
  assert.equal(flowMainLabel({ on_complete: 'VERIFY', need_review: true }), '提交验收');
});

test('流程任务 APPROVE：主按钮是「完成」，副文案「完成后需审批」', () => {
  const list = applyFlowMainButton(doingActions(), { on_complete: 'APPROVE', need_review: false });
  assert.deepEqual(list.map((item) => item.key), ['direct-done', 'cancel']);
  assert.equal(list[0]?.label, '完成');
  assert.equal(flowMainNote({ on_complete: 'APPROVE', need_review: false }), '完成后需审批');
  // 其余两种没有副文案
  assert.equal(flowMainNote({ on_complete: 'DIRECT', need_review: false }), null);
  assert.equal(flowMainNote({ on_complete: 'VERIFY', need_review: true }), null);
});

// ==================== ⑤ 错误码 65034–65039 与 error_code.rs 逐字对齐 ====================

test('65034–65039 码值与 error_code.rs 一致', () => {
  const source = errorCodeSource();
  const pairs: Array<[number, string]> = [
    [FLOW_NODE_CONFIG_MISMATCH, 'FLOW_NODE_CONFIG_MISMATCH'],
    [FLOW_TASK_NODE_INVALID, 'FLOW_TASK_NODE_INVALID'],
    [FLOW_NOTIFY_TEMPLATE_INVALID, 'FLOW_NOTIFY_TEMPLATE_INVALID'],
    [FLOW_TEMPLATE_VAR_INVALID, 'FLOW_TEMPLATE_VAR_INVALID'],
    [FLOW_TASK_STATE_CONFLICT, 'FLOW_TASK_STATE_CONFLICT'],
    [FLOW_PREVIEW_INVALID, 'FLOW_PREVIEW_INVALID'],
    // P-FLOW-1B：任务完成撞「本轮已通过」的幂等码
    [FLOW_TASK_ALREADY_DONE, 'FLOW_TASK_ALREADY_DONE'],
  ];
  for (const [actual, name] of pairs) {
    assert.equal(actual, Number(rustConst(source, name)), `${name} 码值不一致`);
  }
  assert.deepEqual([...TASK_NODE_CODES], [65034, 65035, 65036, 65037, 65038, 65039]);
  assert.equal(FLOW_TASK_ALREADY_DONE, 65020);
});

test('65038 的本地兜底文案指向「联系流程发起人」（不是「刷新重试」）', () => {
  const text = taskNodeErrorText(FLOW_TASK_STATE_CONFLICT, null);
  assert.equal(text.source, 'local');
  assert.match(text.text, /流程发起人/);
  // 65035 的兜底也不再写「本版本不支持」
  const invalid = taskNodeErrorText(FLOW_TASK_NODE_INVALID, null);
  assert.ok(!invalid.text.includes('还不支持'));
  assert.match(invalid.text, /已发布/);
});

test('taskNodeErrorText：后端原文优先，本地文案兜底（并标明来源）', () => {
  const fromBackend = taskNodeErrorText(65037, '模板变量 {{order.amount}} 不在白名单');
  assert.equal(fromBackend.source, 'backend');
  assert.equal(fromBackend.text, '模板变量 {{order.amount}} 不在白名单');

  const fromLocal = taskNodeErrorText(65035, null);
  assert.equal(fromLocal.source, 'local');
  assert.match(fromLocal.text, /65035/);

  const unknown = taskNodeErrorText(0, '');
  assert.equal(unknown.source, 'unknown');
});
