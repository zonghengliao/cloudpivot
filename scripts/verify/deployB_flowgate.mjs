/**
 * ⑥ 审批流未配置 → 409 + 66033 取证（配合 deployB_flowgate.sh：
 *   shell 侧临时把 po_approve 置 DISABLED，本脚本提交一张 DRAFT 采购单，
 *   shell 侧再恢复 PUBLISHED 并复查 8/8。只做一次提交，被拒则账/data 一分不动。）
 */
const BASE = process.env.DEPLOYB_BASE_URL || 'http://127.0.0.1';
const PW = process.env.NIK_ADMIN_PASSWORD || 'Admin@123456';

const lr = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST', headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: PW }),
});
const lb = await lr.json();
if (lb.code !== 0) { console.error('登录失败', lb); process.exit(2); }
const H = { Authorization: `Bearer ${lb.data.access_token}`, 'Content-Type': 'application/json' };

const pr = await fetch(`${BASE}/api/business/erp/purchase-order/page?page=1&page_size=100`, { headers: H });
const pb = await pr.json();
const list = pb?.data?.list || [];
const draft = list.find((p) => p.status === 'DRAFT');
if (!draft) { console.error('没有 DRAFT 采购单可提交', list.map((p) => `${p.order_no}:${p.status}`).join(',')); process.exit(3); }
console.log(`目标采购单：${draft.order_no} status=${draft.status} id=${draft.id}`);

const r = await fetch(`${BASE}/api/business/erp/purchase-order/${draft.id}/submit-approval`, {
  method: 'POST', headers: H, body: JSON.stringify({}),
});
const body = await r.json().catch(() => ({}));
console.log(`POST submit-approval → status=${r.status} code=${body.code}`);
console.log(`msg=${body.msg}`);
const okResp = r.status === 409 && body.code === 66033 && /po_approve/.test(String(body.msg));
console.log(okResp ? '[PASS] 审批流未配置 → 409 + 66033，报文点名 po_approve' : '[FAIL] 未按 409 + 66033 拒绝');

// 复核单据状态没被推动（仍是 DRAFT）
const r2 = await fetch(`${BASE}/api/business/erp/purchase-order/${draft.id}`, { headers: H });
const b2 = await r2.json().catch(() => ({}));
const st = b2?.data?.status;
console.log(`复核单据状态：${st}（应为 DRAFT）`);
process.exit(okResp && st === 'DRAFT' ? 0 : 1);
