/**
 * P3-01 / P3-02 单测：错误码 → 人话（审批类 `650xx` + ERP 单据类 `66xxx`）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 分三层（与 `n4-request-id.test.ts` 同一套姿势）
 *
 * 1. **行为层**（`constants/error-messages.ts` 的 `resolveErrorMessage`，真跑）：
 *    卡面验收「给一个 65023 / 66034 响应 → 展示中文提示」就在这里钉死 —— 而且不止测「有中文」，
 *    还测**四条取文优先级**各自的分支（后端自足 / 后端不自足 / 无后端文案 / 表里没有）。
 * 2. **表格质量**：全部条目非空、含中文、不含裸码、长度合理（toast 会换行，太长看不清）。
 * 3. **覆盖面 + 接线**：
 *    - 读 `backend/crates/cloudpivot-kernel/src/shared/error_code.rs`，`65xxx` 与 `66xxx` 两段
 *      **每一条**常量都必须在表里（后端新增码而前端没登记 → 这里先红）；
 *    - `api/request.ts` 的两条错误路径（HTTP 失败 / 业务码非 0）都必须过这张表，且旧硬编码
 *      文案（`权限不足` / `网络异常，请检查服务是否可用`）不许再出现 —— 它们绕过了码表。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  ERROR_MESSAGES,
  FALLBACK_MESSAGE,
  NETWORK_MESSAGE,
  resolveErrorMessage,
} from '@/constants/error-messages';

const HERE = dirname(fileURLToPath(import.meta.url));
/** 仓库根：`frontend/tests/unit` → 上三级 */
const REPO_ROOT = resolve(HERE, '../../..');
const ERROR_CODE_RS = readFileSync(
  resolve(REPO_ROOT, 'backend/crates/cloudpivot-kernel/src/shared/error_code.rs'),
  'utf8',
);
const REQUEST_TS = readFileSync(resolve(HERE, '../../src/api/request.ts'), 'utf8');

// ==================== ① 行为层：65023（卡面验收） ====================

test('65023 + HTTP 409 + 后端没给 msg → 展示表里的中文人话，且不出现裸码', () => {
  // 后端在 nginx / 反代层被截断（没有 JSON 体）时就是这个形态：只有状态码 + 业务码
  const { message, source } = resolveErrorMessage({ code: 65023, status: 409, message: '' });
  assert.equal(source, 'table');
  assert.match(message, /[\u4e00-\u9fa5]/, '必须给中文提示');
  assert.match(message, /流程管理员/, '必须告诉用户找谁');
  assert.ok(!message.includes('65023'), `不许把裸码甩给用户：${message}`);
  assert.ok(!message.includes('错误码'), `不许出现「错误码」字样：${message}`);
});

test('65023 + 后端原文已自足（含「票数」与「联系」）→ 用后端原文，细节不丢', () => {
  // 后端真实返回（`engine.rs:1074`）：`已有人审批（{reviewed} 票），不能撤回；请联系流程管理员终止`
  const backend = '已有人审批（1 票），不能撤回；请联系流程管理员终止';
  const { message, source } = resolveErrorMessage({ code: 65023, status: 409, message: backend });
  assert.equal(source, 'backend');
  assert.equal(message, backend, '后端原文已说了怎么办，就不该再套一层本地文案');
  assert.match(message, /1 票/, '这一单的具体票数必须保留');
});

test('65023 + 后端原文没给动作（只有现象）→ 表给「怎么办」，后端原文作为说明保留', () => {
  const { message, source } = resolveErrorMessage({ code: 65023, status: 409, message: '已有人审批，不能撤回' });
  assert.equal(source, 'table');
  assert.match(message, /流程管理员/, '表里的动作指引要在');
  assert.match(message, /后端说明：已有人审批，不能撤回/, '后端原文不能丢');
});

test('65020 + 「已处理过」后端原文 → 表补上「不用再点」的指引，且保留轮次细节', () => {
  const backend = '节点「销售主管审批」第 2 轮已经处理过（APPROVED），不能重复完成';
  const { message } = resolveErrorMessage({ code: 65020, status: 409, message: backend });
  assert.match(message, /处理过/, '要告诉用户这条待办已经处理过');
  assert.match(message, /销售主管审批/, '节点名不能丢');
});

// ==================== ① ERP 单据类（P3-02）：库存不足 / 期间锁定 / 被引用 ====================

test('66034 负库存 + 后端点名物料与可用量 → 表给「怎么办」，单号与数量不丢', () => {
  // 后端真实报文（`stockout_handler`）：点名物料、仓库、可用量（P3-01 已定「后端原文优先」，
  // 但这条 msg 只说了现象、没说动作，所以表里的「减少数量 / 换库位 / 先补货」要补上）
  const backend = '物料 MAT-0001「钢板」在仓库「一号库」可用量 3，本次出库 5';
  const { message, source } = resolveErrorMessage({ code: 66034, status: 409, message: backend });
  assert.equal(source, 'table');
  assert.match(message, /减少数量|补货|有货的仓库/, '必须告诉用户下一步怎么做');
  assert.match(message, /MAT-0001/, '后端报文的物料编码不能丢');
  assert.ok(!message.includes('66034'), `不许把裸码甩给用户：${message}`);
});

test('66066 期间已锁定 + 后端已给动作 → 整段用后端原文（带批次号）', () => {
  const backend = '该期间成本批次 CP-202610 已结算，请先红冲后再重算分摊';
  const { message, source } = resolveErrorMessage({ code: 66066, status: 409, message: backend });
  assert.equal(source, 'backend');
  assert.match(message, /CP-202610/);
});

test('66018 单据已被引用 + 后端没文案 → 表里说清「先处理单据，或改为终止」', () => {
  const { message, source } = resolveErrorMessage({ code: 66018, status: 409 });
  assert.equal(source, 'table');
  assert.match(message, /采购单/);
  assert.match(message, /不能删除/);
  assert.ok(!message.includes('66018'));
});

test('66038 期间已结账 + 后端只说现象 → 表补「反结账 / 开账」动作，后端原文保留', () => {
  const { message, source } = resolveErrorMessage({ code: 66038, status: 409, message: '目标会计期间已结账' });
  assert.equal(source, 'table');
  assert.match(message, /反结账/);
  assert.match(message, /后端说明：目标会计期间已结账/);
});

// ==================== ② 取文优先级（四条分支逐条钉） ====================

test('码在表里 + 后端文案只说了现象（无可执行动作）→ 表给动作，后端原文当说明保留', () => {
  const { message, source } = resolveErrorMessage({ code: 66001, status: 409, message: '单位编码已存在' });
  assert.equal(source, 'table');
  assert.match(message, /换一个编码/, '表里的动作指引要在');
  assert.match(message, /后端说明：单位编码已存在/, '后端点名了是「单位」编码，不能丢');
  assert.ok(!message.includes('66001'));
});

test('表里没有的业务码 + 没有文案 + 409 → 用 HTTP 状态的人话（仍不是裸码）', () => {
  // 67555 是 67xxx（PDF）段里**不存在**的值：用来钉「查不到码」这条分支，永远不会被后续卡占用
  const { message, source } = resolveErrorMessage({ code: 67555, status: 409 });
  assert.equal(source, 'http');
  assert.match(message, /刷新/);
  assert.ok(!message.includes('67555'));
});

test('表里没有的业务码 + 后端文案 → 用后端文案，不被 HTTP 409 通用话盖掉', () => {
  const { message, source } = resolveErrorMessage({ code: 67555, status: 409, message: '导出服务暂不可用' });
  assert.equal(source, 'backend');
  assert.equal(message, '导出服务暂不可用');
});

test('后端 msg 就是个裸码 → 视为没给文案，改走码表', () => {
  const { message, source } = resolveErrorMessage({ code: 65023, status: 409, message: '65023' });
  assert.equal(source, 'table');
  assert.match(message, /流程管理员/);
});

test('后端 msg 是英文（反代 / 框架默认）→ 视为没给文案，改走 HTTP 码表', () => {
  const { message, source } = resolveErrorMessage({ code: -1, status: 500, message: 'Internal Server Error' });
  assert.equal(source, 'http');
  assert.match(message, /服务端处理失败/);
  assert.ok(!message.includes('Internal'));
});

test('没有响应（断网 / 跨域）→ 网络人话，不是码、也不是空', () => {
  const { message, source } = resolveErrorMessage({ code: -1, message: 'Network Error' });
  assert.equal(source, 'network');
  assert.equal(message, NETWORK_MESSAGE);
});

test('请求超时（axios ECONNABORTED 的文案）→ 原样透出，不当成「没给文案」', () => {
  const { message } = resolveErrorMessage({
    code: -1,
    message: '请求超时，请稍后重试',
  });
  assert.equal(message, '请求超时，请稍后重试');
});

test('啥都没有 → 兜底人话（绝不返回空串）', () => {
  const { message, source } = resolveErrorMessage({});
  assert.equal(source, 'network');
  assert.equal(message, NETWORK_MESSAGE);
  assert.equal(resolveErrorMessage({ code: -1, status: 599 }).message, FALLBACK_MESSAGE);
});

// ==================== ③ 表格质量 ====================

test('表里每条文案：非空、含中文、不含裸码、长度适合 toast', () => {
  const entries = Object.entries(ERROR_MESSAGES);
  assert.ok(entries.length >= 50, `条目太少（${entries.length}），审批类 43 个码 + HTTP 码应该都在`);
  for (const [key, text] of entries) {
    assert.ok(text.trim().length > 0, `${key} 文案为空`);
    assert.match(text, /[\u4e00-\u9fa5]/, `${key} 必须是中文人话：${text}`);
    assert.ok(!text.includes(key), `${key} 的文案里出现了裸码本身：${text}`);
    assert.ok(!/错误码|error code/i.test(text), `${key} 的文案别写「错误码」：${text}`);
    assert.ok(text.length <= 100, `${key} 文案 ${text.length} 字，toast 会换行，压到 100 字以内`);
  }
});

test('同一条码只有一个文案（表没有重复键被后写覆盖）', () => {
  const keys = Object.keys(ERROR_MESSAGES);
  assert.equal(new Set(keys).size, keys.length);
  for (const key of keys) {
    assert.match(key, /^\d+$/, `键必须是数字字符串（查表时按 String(code)）：${key}`);
  }
});

// ==================== ④ 覆盖后端 error_code.rs 的 65xxx / 66xxx 两段 ====================

/** 从 `error_code.rs` 里抠出某码段的所有 `pub const NAME: i32 = code;`（只认数字字面量）。 */
function declaredCodes(from: number, to: number): Array<{ name: string; code: number }> {
  // `= TASK_APPROVAL_PENDING` 这种别名命中不到数字，但它指向的那条已被数字条文覆盖
  // （65030 由 TASK_APPROVAL_PENDING 提供）。
  return [...ERROR_CODE_RS.matchAll(/pub const ([A-Z_0-9]+): i32 = (6[4-6]\d{3});/g)]
    .map((m) => ({ name: m[1], code: Number(m[2]) }))
    .filter((item) => item.code >= from && item.code < to);
}

test('error_code.rs 里 65xxx（审批流）每一条常量，前端表里都有（后端加码 → 这里先红）', () => {
  const declared = declaredCodes(65000, 66000);
  assert.ok(declared.length >= 40, `没解析到 65xxx 常量（实际 ${declared.length}），regex 可能过时了`);
  const missing = declared.filter((item) => !(String(item.code) in ERROR_MESSAGES));
  assert.deepEqual(
    missing.map((item) => `${item.name}(${item.code})`),
    [],
    '这些审批流码还没翻译成人话，请补进 ERROR_MESSAGES',
  );
});

test('error_code.rs 里 66xxx（ERP 单据 / 主数据）每一条常量，前端表里都有', () => {
  const declared = declaredCodes(66000, 67000);
  // 2026-11-15 实测 79 条（66001–66079 连续、无重号）；低于 75 说明 regex 或码段读歪了
  assert.ok(declared.length >= 75, `没解析到 66xxx 常量（实际 ${declared.length}），regex 可能过时了`);
  const missing = declared.filter((item) => !(String(item.code) in ERROR_MESSAGES));
  assert.deepEqual(
    missing.map((item) => `${item.name}(${item.code})`),
    [],
    '这些 ERP 码还没翻译成人话，请补进 ERROR_MESSAGES',
  );
  // 段里没有的码不该在表里凭空出现（抄错号会导致「后端从没返回过的码也有文案」）
  const declaredSet = new Set(declared.map((item) => item.code));
  const invented = Object.keys(ERROR_MESSAGES)
    .map(Number)
    .filter((code) => code >= 66000 && code < 67000 && !declaredSet.has(code));
  assert.deepEqual(invented, [], ' 表里出现了 66xxx 段内不存在的码，请核对码值');
});

// ==================== ⑤ 接线：request.ts 两条错误路径都必须过表 ====================

test('request.ts 引入码表，并在 HTTP 失败 / 业务码非 0 两条路径上调用', () => {
  assert.match(REQUEST_TS, /import { resolveErrorMessage } from '@\/constants\/error-messages'/);
  const calls = REQUEST_TS.match(/resolveErrorMessage\(/g) ?? [];
  assert.ok(calls.length >= 2, `期望至少 2 处 resolveErrorMessage（toApiError + request<T>），实际 ${calls.length}`);
});

test('request.ts 不再硬编码绕过码表的兜底文案', () => {
  // 这两句一旦回来，就意味着「没码表也能出文案」——那正是本卡要消灭的裸码/无指引形态
  assert.ok(!REQUEST_TS.includes("'权限不足'"), "403 文案应走码表（'403'），别再硬编码 '权限不足'");
  assert.ok(!REQUEST_TS.includes('网络异常，请检查服务是否可用'), '网络兜底应走 NETWORK_MESSAGE');
});
