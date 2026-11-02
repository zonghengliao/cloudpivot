/**
 * P1-FIX-3 单测：采购单详情「审批链路上面的链接不动，审批过程挪到最下面」。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零 DOM 依赖）。
 *
 * # 用户口径（两次纠正后的最终版）
 *
 * 1. **顶部那个「审批单 FLW-xxx」链接一个字符都不改**（它本来就有）；
 * 2. 抽屉最底部（明细之后、`</t-drawer>` 之前）平铺完整审批过程：节点进度 + 审批人 + 时间 + 意见 + 跳过原因；
 * 3. 老单没挂审批实例 → 底部给「未发起审批」空态说明，不留白也不报错（这类单上面本来就没有链接）；
 * 4. 拉不到审批详情 → 降级成链接提示（现有逻辑保留）；
 * 5. **不能出现两个审批单号**。
 *
 * # 为什么这么测
 *
 * 改动只有「位置」和「空态」两件事：`vue-tsc` 抓不到「块放错位置」，e2e 只看得见一张正常单。
 * 所以读 `index.vue` 源码做**顺序与逐字断言**（本仓既有的「源码即契约」姿势），
 * 再用纯函数钉住「跳过原因不许被过滤」这条留痕口径。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import { mapApprovalTasks } from '@/views/business/erp/shared/approval-progress';
import type { FlowTaskItem } from '@/api/business/flow-api';
import { progressTaskLabel } from '@/views/business/flow/components/flow-progress-tasks';

const PURCHASE_VIEW = join(
  resolve(process.cwd()),
  'src/views/business/erp/purchase/index.vue',
);
const SOURCE = readFileSync(PURCHASE_VIEW, 'utf8');

/** 抽屉模板区（`<t-drawer …>` … `</t-drawer>`）—— 断言只在这段里做，别命中列表页 */
const DRAWER_START = SOURCE.indexOf('<t-drawer');
assert.notEqual(DRAWER_START, -1, '找不到详情抽屉 <t-drawer>');
const DRAWER_END = SOURCE.indexOf('</t-drawer>', DRAWER_START);
assert.notEqual(DRAWER_END, -1, '找不到 </t-drawer>');

function drawerTemplate(): string {
  return SOURCE.slice(DRAWER_START, DRAWER_END);
}

/** 取标记在**整个源码**里的绝对下标（必须出现在抽屉模板里；找不到直接红并报清楚丢了什么） */
function pos(marker: string): number {
  const index = drawerTemplate().indexOf(marker);
  assert.notEqual(index, -1, `抽屉模板里找不到 ${marker}`);
  return DRAWER_START + index;
}

/** 统计子串出现次数（用来钉「不能出现两个审批单号」） */
function count(text: string, needle: string): number {
  return text.split(needle).length - 1;
}

// ==================== ① 顶部审批单链接原封不动 ====================

test('① 顶部「审批单 FLW-xxx」链接逐字未改（P-FLOW-3A 原样：v-if / 三元文案 / testid / 点击参数）', () => {
  // 逐字断言原文（一个字符都不许动）：用户明确要求「那一行不要改」
  const original = [
    '          <!-- P-FLOW-3A：审批进度（审批中点名单号，点开去审批单详情看时间线） -->',
    '          <t-link',
    '            v-if="detail.approval_instance_id"',
    '            theme="primary"',
    '            data-testid="purchase-detail-approval"',
    '            @click="openApprovalInstance(detail.approval_instance_id)"',
    '          >',
    "            {{ detail.status === 'SUBMITTED' ? '审批中' : '审批单' }}",
    '            {{ detail.approval_instance_no ?? detail.approval_instance_id }}',
    '          </t-link>',
  ].join('\n');

  assert.ok(
    SOURCE.includes(original),
    '顶部审批单链接被改动了（用户要求原封不动）:\n' + original,
  );
  // 它在单头里（明细之前），始终可见
  assert.ok(pos('data-testid="purchase-detail-approval"') < pos('data-testid="purchase-detail-items"'));
});

test('①-补 全抽屉只有一个审批单号：不许再在基本信息区/底部重复一份', () => {
  const tpl = drawerTemplate();

  // 单号渲染点只有顶部那一个 t-link
  assert.equal(count(tpl, 'approval_instance_no'), 1, 'approval_instance_no 只能出现一次（顶部链接）');
  assert.equal(count(tpl, 'data-testid="purchase-detail-approval"'), 1, '审批单链接 testid 只能有一个');
  // 基本信息栅格里不许出现「审批单」标签（第二次纠正：那一项不加）
  assert.equal(
    count(tpl, '<span class="detail-field__label">审批单</span>'),
    0,
    '基本信息区不许再加「审批单」字段（会出现两个单号）',
  );
  // 底部块只画过程，不再画单号链接（避免第二个单号）
  const bottom = tpl.slice(tpl.indexOf('data-testid="purchase-detail-items"'));
  assert.equal(count(bottom, 'approval_instance_no'), 0, '底部不许再出现审批单号');
});

// ==================== ② 老单没有审批实例 → 底部空态 ====================

test('② 没挂审批实例的老单：底部显示「未发起审批」空态（不留白、不报错），顶部也没有链接', () => {
  const tpl = drawerTemplate();

  // 空态在降级行之后（同一 if/else-if/else 链的 else 分支）
  const emptyStateAt = tpl.indexOf('data-testid="purchase-detail-approval-empty-state"');
  assert.notEqual(emptyStateAt, -1, '底部空态 testid 丢了');
  assert.match(tpl, /未发起审批，暂无审批过程/);

  // 空态分支必须挂在 v-else 上（有进度 / 有降级链接时都不显示）
  const chain = tpl.slice(tpl.indexOf('data-testid="purchase-detail-approval-progress"'), emptyStateAt);
  assert.match(chain, /v-else-if="approvalFallback"/, '降级行仍是 else-if');
  assert.match(tpl.slice(emptyStateAt - 200, emptyStateAt), /v-else/, '空态是 else 分支（没实例才出）');

  // 顶部链接是 v-if="detail.approval_instance_id" → 没实例时不会渲染，所以只会有底部一句说明
  assert.match(tpl, /v-if="detail\.approval_instance_id"/);
});

// ==================== ③ 拉不到详情 → 降级行保留（testid + 简化文案） ====================

test('③ 审批详情拉不到：降级行 testid 保留，文案简化成「一句提示 + 单号链接」', () => {
  const tpl = drawerTemplate();

  assert.ok(
    pos('data-testid="purchase-detail-approval-fallback"') >= 0,
    'data-testid="purchase-detail-approval-fallback" 被测试依赖，不能删',
  );
  assert.match(tpl, /v-else-if="approvalFallback"/);
  assert.match(tpl, /审批进度暂时拉不到，/);
  // 顶部已有单号 → 底部不重复「审批中 / 审批单」前缀，也不再啰嗦「点开看审批单详情」
  assert.match(tpl, /\{\{ approvalFallback\.no \}\}/);
  assert.doesNotMatch(tpl, /\{\{ approvalFallback\.label \}\}/);
  assert.doesNotMatch(tpl, /点开看审批单详情/);
});

// ==================== ④ 审批过程在明细之后（抽屉最底部） ====================

test('④ 渲染顺序：基本信息 → 收货进度 → 已生成收货单 → 明细 → 审批进度 → </t-drawer>', () => {
  const items = pos('data-testid="purchase-detail-items"');
  const progress = pos('data-testid="purchase-detail-approval-progress"');
  const fallback = pos('data-testid="purchase-detail-approval-fallback"');
  const emptyState = pos('data-testid="purchase-detail-approval-empty-state"');
  const receive = pos('data-testid="purchase-receive-progress"');
  const generated = pos('data-testid="purchase-generated-stockins"');

  assert.ok(receive < generated, '收货进度 → 已生成收货单 顺序不变（这次不动它们）');
  assert.ok(generated < items, '已生成收货单 → 明细 顺序不变');
  assert.ok(items < progress, `明细(${items}) 必须在审批进度(${progress}) 之前`);
  assert.ok(items < fallback, '明细 必须在降级行之前');
  assert.ok(progress < DRAWER_END, '审批进度必须在抽屉闭合之前');
  assert.ok(fallback < DRAWER_END, '降级行必须在抽屉闭合之前');
  assert.ok(items < emptyState && emptyState < DRAWER_END, '空态也在明细之后、抽屉之内');

  // 标题跟着进度块一起搬（不能孤零零留在中间）
  const title = pos('{{ APPROVAL_PROGRESS_TITLE }}');
  assert.ok(title > items, '「审批进度」标题必须在明细之后');
  assert.ok(title < progress, '标题紧贴进度条（标题在 FlowProgress 之前）');
  // 中间位置（基本信息与收货进度之间）不许再有进度块：进度块必须晚于「已生成收货单」
  assert.ok(progress > generated, '审批进度不许留在收货进度/已生成收货单之前（中间位置）');
});

// ==================== ⑤ 跳过原因留痕（用户要「一眼看到跳过原因」） ====================

test('⑤ 跳过原因：SKIPPED 的审批人不被藏起来，原因（comment）原样带出', () => {
  // 真数据形状（PO-20261031-000010 / FLW-20261101-FFD8140A）：超级管理员、老六 被内控判 SKIPPED
  const skipped: FlowTaskItem = {
    id: 'task-skip',
    instance_id: 'inst-1',
    instance_no: 'FLW-20261101-FFD8140A',
    title: '采购单 PO-20261031-000010',
    flow_name: '采购订单审批',
    initiator_name: '超级管理员',
    current_node_name: '部门主管审批',
    arrived_at: '2026-11-02T00:19:14Z',
    node_key: 'n_dept',
    round: 1,
    task_kind: 'APPROVAL',
    assignee_id: 'u-admin',
    assignee_name: '超级管理员',
    assignee_source: 'ROLE',
    status: 'SKIPPED',
    instance_status: 'APPROVED',
    is_added_sign: false,
    form_data: {},
    is_self_approval: true,
    is_auto: false,
    comment: '内控：制单人/发起人不可自审（P0-FIX 清理，改 SKIPPED 留痕）',
    transferred_from: '',
    created_at: '2026-11-02T00:19:14Z',
    completed_at: '2026-11-02T00:19:14Z',
  };

  const [task] = mapApprovalTasks([skipped]);
  assert.equal(task.status, 'SKIPPED', 'SKIPPED 不许被过滤掉（那是留痕）');
  assert.equal(task.assignee_name, '超级管理员');
  assert.equal(task.comment, '内控：制单人/发起人不可自审（P0-FIX 清理，改 SKIPPED 留痕）');
  assert.equal(task.at, '2026-11-02T00:19:14Z');
  // 组件侧文案：跳过 → 「已跳过」，与原因一起渲染在节点下面
  assert.equal(progressTaskLabel('SKIPPED'), '已跳过');
});
