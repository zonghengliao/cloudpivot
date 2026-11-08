/**
 * ATT-08 单测：附件必传闸门的纯逻辑 + 接口 / 页面契约。
 *
 * # 钉死的口径
 *
 * 1. **判定来自后端**：`can_proceed = false` → 按钮禁用 + 红字；`true` → 都不画。
 * 2. **文案来自后端**：红字用 `missing[].hint`（后端拼），前端只兜底不重写。
 * 3. **降级不禁用**：查询失败（403/404/500）时按钮照常可点、不画红字 ——
 *    一次抖动不该锁死所有人，真正的闸门在后端。
 * 4. **接口契约**：`GET /api/business/attachment/check`（**必须带 `/api` 前缀**，
 *    漏了会命中 SPA fallback 返回 HTML）+ `doc_type` / `doc_id` / `at` 三个参数。
 * 5. **页面契约**：发票详情页的开票按钮必须同时受「权限」与「闸门」约束，
 *    并挂上红字组件；附件面板的 `changed` 必须触发重查（验收②「上传后自动变亮」）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  degradedGate,
  gateButtonTip,
  gateFromCheck,
  pendingGate,
  summarizeDetail,
  summarizeMissing,
  type GateCheckLike,
  type GateMissingLike,
} from '../../src/views/business/erp/shared/attachment-gate-pure';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

function missing(overrides: Partial<GateMissingLike> = {}): GateMissingLike {
  return {
    role_label: '发票影像',
    need: 'pdf/ofd/xml',
    have: 0,
    min: 1,
    total: 0,
    shortfall: 1,
    hint: '缺少发票影像（需 pdf/ofd/xml 格式）',
    message:
      '发票必须上传发票影像（pdf/ofd/xml）：至少要 1 个，当前 0 个（该单据共 0 个附件），还差 1 个',
    ...overrides,
  };
}

function blockedCheck(overrides: Partial<GateCheckLike> = {}): GateCheckLike {
  return {
    can_proceed: false,
    at_label: '提交',
    missing: [missing()],
    hint: '缺少发票影像（需 pdf/ofd/xml 格式）',
    message:
      '发票必须上传发票影像（pdf/ofd/xml）：至少要 1 个，当前 0 个（该单据共 0 个附件），还差 1 个',
    ...overrides,
  };
}

// ==================== ① 满足 / 不满足 ====================

test('闸门：can_proceed = true → 按钮可点、不画红字、不挂 tooltip（验收②）', () => {
  const gate = gateFromCheck({
    can_proceed: true,
    at_label: '提交',
    missing: [],
    hint: null,
    message: null,
  });
  assert.equal(gate.canProceed, true);
  assert.equal(gate.hint, null);
  assert.equal(gate.tooltip, null);
  assert.equal(gate.resolved, true);
  assert.equal(gate.degradedReason, null);
  assert.equal(gate.atLabel, '提交');
});

test('闸门：can_proceed = false → 禁用 + 红字原样用后端文案（验收①）', () => {
  const gate = gateFromCheck(blockedCheck());
  assert.equal(gate.canProceed, false);
  assert.equal(gate.hint, '缺少发票影像（需 pdf/ofd/xml 格式）');
  assert.equal(gate.resolved, true);
  // tooltip 两行：为什么点不了 + 后端 400 原文（还差几个 / 现有什么格式）
  assert.ok(gate.tooltip);
  const lines = gate.tooltip!.split('\n');
  assert.equal(lines.length, 2);
  assert.equal(lines[0], '无法提交：缺少发票影像（需 pdf/ofd/xml 格式）');
  assert.ok(lines[1].includes('还差 1 个'), lines[1]);
});

test('闸门：格式不对时红字必须点名「格式不符，需要 pdf/ofd/xml」（验收③）', () => {
  const gate = gateFromCheck(
    blockedCheck({
      missing: [
        missing({
          total: 1,
          have: 0,
          hint: '发票影像格式不符，需要 pdf/ofd/xml（当前 jpg）',
        }),
      ],
      hint: '发票影像格式不符，需要 pdf/ofd/xml（当前 jpg）',
    }),
  );
  assert.equal(gate.canProceed, false);
  assert.ok(gate.hint!.includes('格式不符，需要 pdf/ofd/xml'), gate.hint!);
  assert.ok(gate.hint!.includes('当前 jpg'), gate.hint!);
});

test('闸门：多条缺口 → 红字用「；」、tooltip 报文用换行', () => {
  const gate = gateFromCheck(
    blockedCheck({
      missing: [
        missing(),
        missing({ role_label: '银行回单', hint: '缺少银行回单（需 pdf/jpg/png 格式）' }),
      ],
      hint: undefined,
      message: undefined,
    }),
  );
  assert.equal(gate.canProceed, false);
  assert.equal(gate.hint, '缺少发票影像（需 pdf/ofd/xml 格式）；缺少银行回单（需 pdf/jpg/png 格式）');
  assert.ok(gate.tooltip!.includes('\n'));
  assert.ok(gate.tooltip!.includes('发票必须上传发票影像'));
});

test('闸门：后端没给 hint 时用 missing[].hint 兜底，绝不自己编格式说明', () => {
  const gate = gateFromCheck({
    can_proceed: false,
    at_label: '确认',
    missing: [missing({ hint: '缺少盘点表或盈亏说明（需 pdf/jpg/png 格式）' })],
    hint: null,
    message: null,
  });
  assert.equal(gate.hint, '缺少盘点表或盈亏说明（需 pdf/jpg/png 格式）');
  assert.ok(gate.tooltip!.startsWith('无法确认：'), gate.tooltip!);
});

test('闸门：连 missing 都为空也不能「灰着没解释」', () => {
  const gate = gateFromCheck({
    can_proceed: false,
    at_label: '',
    missing: [],
    hint: null,
    message: null,
  });
  assert.equal(gate.canProceed, false);
  assert.equal(gate.hint, '缺少必传附件');
  assert.ok(gate.tooltip!.startsWith('无法提交：'), gate.tooltip!);
});

// ==================== ② 降级 ====================

test('降级：查询失败不禁用按钮、不画红字（403/404/500 都不该锁死提交）', () => {
  const gate = degradedGate('HTTP 500 附件必传规则校验失败');
  assert.equal(gate.canProceed, true, '降级必须放行按钮');
  assert.equal(gate.hint, null);
  assert.equal(gate.tooltip, null);
  assert.equal(gate.resolved, false);
  assert.equal(gate.degradedReason, 'HTTP 500 附件必传规则校验失败');
  // 没有原因也要能用
  assert.equal(degradedGate(null).canProceed, true);
});

test('初始态：还没有单据 / 还在查 → 不禁用、不画红字', () => {
  const gate = pendingGate();
  assert.equal(gate.canProceed, true);
  assert.equal(gate.hint, null);
  assert.equal(gate.tooltip, null);
  assert.equal(gate.resolved, false);
});

// ==================== ③ tooltip 定序 ====================

test('tooltip：权限优先于附件缺口，两者都没有时返回空串（不挂空 tooltip）', () => {
  const blocked = gateFromCheck(blockedCheck());
  assert.equal(gateButtonTip(blocked, '缺少权限 business:erp:invoice:status'), '缺少权限 business:erp:invoice:status');
  assert.equal(gateButtonTip(blocked, ''), blocked.tooltip);
  assert.equal(gateButtonTip(pendingGate(), null), '');
  assert.equal(gateButtonTip(gateFromCheck({ can_proceed: true, at_label: '提交', missing: [] }), ''), '');
});

test('兜底拼接：空数组 → null（不产出空字符串）', () => {
  assert.equal(summarizeMissing([]), null);
  assert.equal(summarizeDetail([]), null);
  assert.equal(summarizeMissing([missing({ hint: '  ' })]), null);
});

// ==================== ④ 接口与页面契约 ====================

test('接口契约：check 走 `/api` 前缀 + doc_type / doc_id / at 三参数', () => {
  const api = readSource('api/business/erp/attachment.ts');
  assert.ok(
    api.includes("'/api/business/attachment/check'"),
    '必须写全 /api 前缀（漏了会命中 SPA fallback 返回 HTML）',
  );
  const start = api.indexOf('export function checkDocAttachments');
  assert.ok(start > 0, '必须导出 checkDocAttachments');
  const body = api.slice(start, start + 600);
  assert.ok(body.includes('doc_type: docType'), body);
  assert.ok(body.includes('doc_id: docId'), body);
  assert.ok(body.includes('at,'), body);
  // 动作词表必须与后端 CHECK 逐字一致
  assert.ok(api.includes("type AttachmentRequiredAt = 'SUBMIT' | 'CONFIRM' | 'ACTIVATE'"), api);
});

test('页面契约：详情页开票按钮受权限 + 闸门双重约束，并挂红字与 tooltip', () => {
  const page = readSource('views/business/erp/invoice/index.vue');
  assert.ok(
    page.includes(':disabled="!hasStatusPermission || !attachGate.canProceed"'),
    '开票按钮必须同时受权限与闸门约束',
  );
  assert.ok(page.includes('<AttachmentGateHint :hint="attachGate.hint"'), '按钮旁必须有红字组件');
  assert.ok(page.includes(':content="issueGateTip"'), 'disabled 按钮必须包 tooltip 说明原因');
  assert.ok(page.includes('@changed="refreshAttachGate()"'), '上传/删除附件后必须重查（验收②）');
  assert.ok(page.includes('doc-type="INVOICE"'), '发票详情页必须能上传发票影像（否则规则无法满足）');
});

test('页面契约：列表行「开票」先查闸门，缺附件就点名并打开详情', () => {
  const page = readSource('views/business/erp/invoice/index.vue');
  const start = page.indexOf('async function issueInvoice');
  assert.ok(start > 0, 'issueInvoice 必须是异步（先查闸门）');
  const body = page.slice(start, start + 1200);
  assert.ok(body.includes("checkDocAttachments('INVOICE', row.id, 'SUBMIT')"), body);
  assert.ok(body.includes('if (!check.can_proceed)'), body);
  assert.ok(body.includes('await openDetail(row)'), body);
});

test('组件与组合式函数必须存在且被页面引用（漏 import 会运行期炸）', () => {
  const page = readSource('views/business/erp/invoice/index.vue');
  assert.ok(page.includes("from '@/views/business/erp/shared/use-attachment-gate'"));
  assert.ok(page.includes("from '@/views/business/erp/shared/AttachmentGateHint.vue'"));
  assert.ok(page.includes("from '@/views/business/erp/shared/DocAttachmentPanel.vue'"));
  // 组合式函数必须用请求序号丢弃乱序响应（连传两个文件时按钮状态不能被回退）
  const composable = readSource('views/business/erp/shared/use-attachment-gate.ts');
  assert.ok(composable.includes('seq'), '必须有请求序号');
  assert.ok(composable.includes('if (mine !== seq) return;'), '必须丢弃乱序响应');
});
