/**
 * P3-07 后端小补真机验收（隔离实例：后端 :18313 → 库 cloudpivot_p307be，`setup.sh` 已把账号/开关/应付单备好）。
 *
 * 验收点（卡面「要做什么」+「验收」）：
 *  ① 审批通过 + 付款仍 PENDING           → payment-info 的 payments[] 里 awaiting_executor = **true**
 *  ② 同一个人（非审批参与人）读实例详情    → 403 + 65004（= 前端兜底方案拿不到状态的原因，本卡要消灭它）
 *  ③ 该人执行付款                        → 200；再查 awaiting_executor = **false**、付款 APPROVED、未付归零
 *  ④ 实例仍在跑（审批未过）时             → awaiting_executor = **false**（单元用例②的真机版）
 *  ⑤ 开关关闭回归（审批通过即记账）        → 付款直接 APPROVED，awaiting_executor = **false**（单元用例③）
 *  ⑥ 每一位付款行都真的带这个字段（前端契约：下发即为准，缺席才探实例）
 *
 * 跑法：`node verify.mjs`
 */
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const RAW = join(HERE, 'raw');
mkdirSync(RAW, { recursive: true });

const API = 'http://127.0.0.1:18313/api';
const tokenOf = (f) => readFileSync(join(HERE, f), 'utf8').trim();

const ADMIN = tokenOf('admin.token');
const PAY = tokenOf('pay.token');   // 付款发起人（FINANCE_MANAGER）
const EXEC = tokenOf('exec.token'); // 执行人：同权限但**不是**审批实例参与人

const AP1 = 'abe70001-0000-4000-8000-000000000101'; // 开关打开用
const AP2 = 'abe70001-0000-4000-8000-000000000102'; // 开关关闭回归用
const CFG_APPROVE_EXECUTE = 'e2b00000-0000-4000-8000-000000000006';

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} | ${name}${detail ? ' | ' + detail : ''}`);
  return ok;
}
const save = (file, obj) => writeFileSync(join(RAW, file), JSON.stringify(obj, null, 2));

async function api(path, { method = 'GET', token, body } = {}) {
  const res = await fetch(`${API}${path}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = await res.json().catch(() => ({}));
  return { status: res.status, json };
}

/** 找到某张应付单下的那笔付款的 payment-info 行 */
async function paymentRow(payableId, paymentNo, token = ADMIN) {
  const info = await api(`/business/erp/payable/${payableId}/payment-info`, { token });
  const row = (info.json?.data?.payments ?? []).find((p) => p.payment_no === paymentNo);
  return { info, row };
}

/** 发起付款 → 审批人（admin）通过；返回中间态信息 */
async function payAndApprove(payableId, amount, remark) {
  const pay = await api(`/business/erp/payable/${payableId}/pay`, {
    method: 'POST', token: PAY, body: { amount, pay_method: '银行转账', remark },
  });
  const instanceId = pay.json?.data?.approval_instance_id;
  const todo = await api('/business/flow/task/todo?page_size=50', { token: ADMIN });
  const task = (todo.json?.data?.list ?? []).find((t) => t.instance_id === instanceId);
  const approve = task
    ? await api(`/business/flow/task/${task.id}/approve`, { method: 'POST', token: ADMIN, body: { comment: `P307BE 验收：同意（${remark}）` } })
    : { status: 0, json: {} };
  return { pay, instanceId, task, approve };
}

const summary = {};

// ============ ④ + ①：实例 RUNNING → false；审批通过 → true ============
{
  const pay = await api(`/business/erp/payable/${AP1}/pay`, {
    method: 'POST', token: PAY, body: { amount: '1200.0000', pay_method: '银行转账', remark: 'P307BE 卡面验收（开关打开）' },
  });
  save('01_pay.json', pay);
  const instanceId = pay.json?.data?.approval_instance_id;
  const paymentNo = pay.json?.data?.payment_no;
  check('①-0 发起付款 → 202 + outcome=PENDING_APPROVAL（走了审批流）',
    pay.status === 202 && pay.json?.data?.outcome === 'PENDING_APPROVAL',
    `http=${pay.status} payment=${paymentNo} instance=${instanceId}`);

  // ④ 审批还没结束（实例 RUNNING）→ 字段必须下发且为 false
  const before = await paymentRow(AP1, paymentNo, EXEC);
  save('02_info_instance_running.json', before.info);
  check('④-1 审批未通过（实例 RUNNING）→ awaiting_executor 明确下发为 false',
    before.row?.awaiting_executor === false && before.row?.approval_status === 'PENDING',
    `awaiting_executor=${JSON.stringify(before.row?.awaiting_executor)} status=${before.row?.approval_status}`);

  // admin（= 审批人）通过审批
  const todo = await api('/business/flow/task/todo?page_size=50', { token: ADMIN });
  const task = (todo.json?.data?.list ?? []).find((t) => t.instance_id === instanceId);
  check('①-1 审批人待办里拿到这条付款审批任务', !!task, `task=${task?.id}`);
  const approve = task
    ? await api(`/business/flow/task/${task.id}/approve`, { method: 'POST', token: ADMIN, body: { comment: 'P307BE 验收：同意付款' } })
    : { status: 0, json: {} };
  save('03_approve.json', approve);
  check('①-2 审批通过 → 实例 APPROVED，但付款仍 PENDING（approve_execute 生效：账没动）',
    approve.json?.data?.instance_status === 'APPROVED',
    `instance_status=${approve.json?.data?.instance_status}`);

  const info = await api(`/business/erp/payable/${AP1}/payment-info`, { token: EXEC });
  save('04_info_awaiting_executor.json', info);
  const row = (info.json?.data?.payments ?? []).find((p) => p.payment_no === paymentNo);
  summary.payment_id = row?.id;
  summary.instance_id = instanceId;
  check('①-3 ★ payment-info 里那笔 awaiting_executor = true（本卡核心）',
    row?.awaiting_executor === true,
    `awaiting_executor=${JSON.stringify(row?.awaiting_executor)} status=${row?.approval_status}`);
  check('①-4 同一行：付款仍 PENDING、应付未付仍是 1200（钱没付出去）',
    row?.approval_status === 'PENDING' && info.json?.data?.payable?.paid_amount === '0.0000',
    `status=${row?.approval_status} paid=${info.json?.data?.payable?.paid_amount} unpaid=${info.json?.data?.payable?.unpaid_amount}`);
  check('⑥ 字段对每一笔付款都下发（前端 contract：下发即以它为准）',
    (info.json?.data?.payments ?? []).every((p) => typeof p.awaiting_executor === 'boolean'),
    `payments=${(info.json?.data?.payments ?? []).length} keys=${Object.keys(row ?? {}).join(',')}`);

  // ② 反证：执行人不是审批参与人 → 实例详情 403（正是前端兜底方案失效的场景）
  const detail = await api(`/business/flow/instance/${instanceId}`, { token: EXEC });
  save('05_instance_detail_exec.json', detail);
  check('②-1 执行人（非参与人）读实例详情 → 403 + 65004（老兜底方案拿不到状态）',
    detail.status === 403 && detail.json?.code === 65004,
    `http=${detail.status} code=${detail.json?.code} msg=${detail.json?.msg}`);
  check('②-2 但同一时刻 payment-info 对他 200（后端字段绕过了实例可见性）',
    info.status === 200, `http=${info.status}`);

  // ③ 执行 → awaiting_executor 翻回 false
  const exec = await api(`/business/erp/payable/payment/${row?.id}/execute`, { method: 'POST', token: EXEC });
  save('06_execute.json', exec);
  check('③-1 执行付款 → 200 + data.message（应付回写 + 付款凭证）',
    exec.status === 200 && exec.json?.code === 0 && typeof exec.json?.data?.message === 'string',
    `http=${exec.status} msg=${exec.json?.data?.message}`);

  const after = await api(`/business/erp/payable/${AP1}/payment-info`, { token: EXEC });
  save('07_info_after_execute.json', after);
  const rowAfter = (after.json?.data?.payments ?? []).find((p) => p.payment_no === paymentNo);
  check('③-2 执行后再查：awaiting_executor = false + 付款 APPROVED',
    rowAfter?.awaiting_executor === false && rowAfter?.approval_status === 'APPROVED',
    `awaiting_executor=${JSON.stringify(rowAfter?.awaiting_executor)} status=${rowAfter?.approval_status}`);
  check('③-3 账真的动了：未付 1200 → 0，付款凭证已生成',
    after.json?.data?.payable?.paid_amount === '1200.0000' && !!rowAfter?.voucher_no,
    `paid=${after.json?.data?.payable?.paid_amount} unpaid=${after.json?.data?.payable?.unpaid_amount} voucher=${rowAfter?.voucher_no}`);
}

// ============ ⑤ 开关关闭回归：审批通过即记账 → awaiting_executor = false ============
{
  const cfg = await api(`/support/config/${CFG_APPROVE_EXECUTE}`, { token: ADMIN });
  const payload = { ...cfg.json.data, config_value: 'false' };
  const off = await api(`/support/config/${CFG_APPROVE_EXECUTE}`, { method: 'PUT', token: ADMIN, body: payload });
  check('⑤-1 关掉 approve_execute', off.json?.data?.config_value === 'false', `value=${off.json?.data?.config_value}`);

  const { pay, approve } = await payAndApprove(AP2, '800.0000', 'P307BE 开关关闭回归');
  const paymentNo = pay.json?.data?.payment_no;
  const { row, info } = await paymentRow(AP2, paymentNo, ADMIN);
  save('08_info_switch_off.json', { pay, approve, info });
  check('⑤-2 审批通过即自动记账：付款直接 APPROVED + paid=800',
    row?.approval_status === 'APPROVED' && info.json?.data?.payable?.paid_amount === '800.0000',
    `status=${row?.approval_status} paid=${info.json?.data?.payable?.paid_amount}`);
  check('⑤-3 已记账的付款 awaiting_executor = false（不会误显示「执行付款」）',
    row?.awaiting_executor === false,
    `awaiting_executor=${JSON.stringify(row?.awaiting_executor)}`);

  // 复原开关（留在开启态，方便人工复核）
  const back = await api(`/support/config/${CFG_APPROVE_EXECUTE}`, { method: 'PUT', token: ADMIN, body: { ...payload, config_value: 'true' } });
  check('⑤-4 开关复原为 true', back.json?.data?.config_value === 'true', `value=${back.json?.data?.config_value}`);
}

save('99_summary.json', { results, summary });

const fails = results.filter((r) => !r.ok);
console.log(`\n===== P3-07 后端验收：PASS ${results.length - fails.length} / FAIL ${fails.length} =====`);
if (fails.length) {
  for (const f of fails) console.log(`FAILED: ${f.name} | ${f.detail}`);
  process.exit(1);
}
