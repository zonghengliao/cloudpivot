/**
 * `notify-node-config.ts` 单测 —— P-FLOW-2 前端配置面板的三条硬口径。
 *
 * 运行：`pnpm test:unit`
 *
 * # 这份测试挡住什么
 *
 * 1. **模板二选一**：`notifyConfigPatch` 产出的 `config.notify` 里**永远**只有
 *    `template_code` 或 `inline` 其中一个 —— 后端对「两个都给 / 都不给」都是 65034。
 * 2. **来源 × 键**：`USER` 只带 `user_ids`、`ROLE` 只带 `role_codes`… 带错键后端 65034，
 *    所以这里逐类型断言「除自己那个键之外一个都不许出现」。
 * 3. **往返稳定**：`notifyConfigOf(patch(cfg))` 必须回到同一份状态（撤销 / 重开面板不漂移）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DEFAULT_NOTIFY_CONFIG,
  MAX_NOTIFY_TARGETS,
  emptyTarget,
  notifyConfigOf,
  notifyConfigPatch,
  notifyIssues,
  renderSample,
  targetIssue,
  type NotifyConfig,
  type NotifyTargetType,
} from '../../src/views/business/flow/components/notify-node-config';

/** 造一份可用的配置（默认值 + 给定改动） */
function config(over: Partial<NotifyConfig> = {}): NotifyConfig {
  return {
    ...DEFAULT_NOTIFY_CONFIG,
    targets: DEFAULT_NOTIFY_CONFIG.targets.map((item) => ({ ...item })),
    channels: [...DEFAULT_NOTIFY_CONFIG.channels],
    ...over,
  };
}

test('模板二选一：引用模式不带 inline，内联模式不带 template_code', () => {
  const byCode = notifyConfigPatch(config({ template_mode: 'code', template_code: 'FLOW_TODO' })).config.notify;
  assert.equal(byCode.template_code, 'FLOW_TODO');
  assert.equal('inline' in byCode, false, '引用模式不许带 inline（后端 65034）');

  const byInline = notifyConfigPatch(config({ template_mode: 'inline' })).config.notify;
  assert.equal('template_code' in byInline, false, '内联模式不许带 template_code（后端 65034）');
  assert.ok(byInline.inline && typeof byInline.inline === 'object');
});

test('每个来源只带自己那个键', () => {
  const types: NotifyTargetType[] = [
    'USER',
    'ROLE',
    'DEPT_LEADER',
    'INITIATOR',
    'INITIATOR_SUPERIOR',
    'TASK_OWNER',
    'FORM_VAR',
    'ADMIN',
  ];
  const ownKey: Record<string, string | null> = {
    USER: 'user_ids',
    ROLE: 'role_codes',
    DEPT_LEADER: 'dept_ids',
    INITIATOR_SUPERIOR: 'levels',
    FORM_VAR: 'form_var_key',
  };
  for (const type of types) {
    const target = {
      ...emptyTarget(type),
      user_ids: ['11111111-1111-1111-1111-111111111111'],
      role_codes: ['FINANCE_MANAGER'],
      dept_ids: ['22222222-2222-2222-2222-222222222222'],
      levels: 2,
      form_var_key: 'buyer_id',
    };
    const notify = notifyConfigPatch(config({ targets: [target], template_mode: 'inline' })).config.notify;
    const rows = notify.targets as Array<Record<string, unknown>>;
    assert.equal(rows.length, 1);
    const keys = Object.keys(rows[0]).filter((key) => key !== 'type');
    const want = ownKey[type];
    if (want === null || want === undefined) {
      assert.deepEqual(keys, [], `${type} 不该带任何额外键，实际 ${keys.join(',')}`);
    } else {
      assert.deepEqual(keys, [want], `${type} 只该带 ${want}，实际 ${keys.join(',')}`);
    }
  }
});

test('往返稳定：patch 之后再读回来是同一份状态', () => {
  const source = config({
    when: 'ON_TIMEOUT',
    targets: [
      { ...emptyTarget('ROLE'), role_codes: ['FINANCE_MANAGER', 'PURCHASE_MANAGER'] },
      { ...emptyTarget('INITIATOR_SUPERIOR'), levels: 3 },
    ],
    template_mode: 'inline',
    inline_title: '{{instance.title}} 需要你确认',
    inline_content: '单号 {{instance.biz_no}}',
    channels: ['INBOX', 'EMAIL'],
    level: 'URGENT',
    force: true,
    skip_if_empty: 'PASS',
  });
  const patched = notifyConfigPatch(source).config.notify;
  const back = notifyConfigOf({ config: { notify: patched } });
  assert.equal(back.when, 'ON_TIMEOUT');
  assert.equal(back.targets.length, 2);
  assert.deepEqual(back.targets[0].role_codes, ['FINANCE_MANAGER', 'PURCHASE_MANAGER']);
  assert.equal(back.targets[1].levels, 3);
  assert.equal(back.template_mode, 'inline');
  assert.equal(back.inline_title, '{{instance.title}} 需要你确认');
  assert.deepEqual(back.channels, ['INBOX', 'EMAIL']);
  assert.equal(back.level, 'URGENT');
  assert.equal(back.force, true);
  assert.equal(back.skip_if_empty, 'PASS');
});

test('空的 targets 数组会退回一行默认来源（面板不能「一行都没有」）', () => {
  const back = notifyConfigOf({ config: { notify: { when: 'ON_ENTER', targets: [], template_code: 'FLOW_TODO' } } });
  assert.equal(back.targets.length, 1, '空数组必须补一行，否则面板没地方加人');
  assert.equal(back.targets[0].type, 'INITIATOR');
});

test('校验：条数 / 模板 / 必填三处的文案与后端一致', () => {
  const tooMany = config({
    targets: Array.from({ length: MAX_NOTIFY_TARGETS + 1 }, () => emptyTarget('INITIATOR')),
  });
  assert.ok(notifyIssues(tooMany).some((item) => item.includes(String(MAX_NOTIFY_TARGETS))));

  assert.ok(
    notifyIssues(config({ template_mode: 'code', template_code: '' })).some((item) => item.includes('模板')),
    '引用模式没选模板必须报',
  );
  assert.ok(
    notifyIssues(config({ template_mode: 'code', template_code: 'flow_todo' })).some((item) => item.includes('大写字母')),
    '编码格式要与后端一致',
  );
  assert.ok(
    notifyIssues(config({ template_mode: 'inline', inline_title: '  ' })).some((item) => item.includes('标题')),
    '空标题必须报',
  );
  assert.deepEqual(notifyIssues(config()), [], '默认配置必须完全干净');

  assert.equal(targetIssue({ ...emptyTarget('USER') }), '请至少选一个用户');
  assert.equal(targetIssue({ ...emptyTarget('ROLE') }), '请至少选一个角色');
  assert.equal(targetIssue({ ...emptyTarget('FORM_VAR') }), '请填表单里存用户 id 的字段名');
  assert.equal(targetIssue({ ...emptyTarget('INITIATOR') }), null);
});

test('样例预览：已知变量替换、未知变量显示成〔path〕而不是空串', () => {
  const out = renderSample('{{instance.title}} / {{form.unknown_field}}');
  assert.ok(out.includes('华东金属'), 'instance.title 必须换成样例值');
  assert.ok(out.includes('〔form.unknown_field〕'), '未知变量要看得见，不能静默变空串');
  assert.equal(renderSample('没有变量'), '没有变量');
});
