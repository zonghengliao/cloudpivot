# HOME-5 验收证据 — 工作台「待我审批」卡（含快捷同意 / 驳回 / 加签）

实例：隔离后端 `127.0.0.1:18097`（debug，profile=test）+ 空库 `cloudpivot_home5_v4`（首启自动迁移 + seed）
+ Vite dev `127.0.0.1:5403`（`BACKEND_ORIGIN` → 18097）+ Playwright Chromium。

账号：`admin / Home5@2026Pass`（SUPER_ADMIN）、`h5viewer / Viewer@2026Pass`（EMPLOYEE，非超管，无待办）、
`h5clerk / Clerk@2026Pass`（EMP，**发起人**，用真实接口发起 12 张审批单，让 admin 有真实待办）。

---

## 一、交付内容

| 层 | 文件 | 说明 |
|----|------|------|
| 卡片组件 | `frontend/src/views/dashboard/components/ApprovalTodoCard.vue`（新） | 待办 10 条 + 今日已办 + 三个动作（同意 / 驳回 / 加签）+ 三张对话框 |
| 工作台容器 | `frontend/src/views/dashboard/components/WorkbenchTab.vue` | 在左栏「工作任务」之后插入 `<ApprovalTodoCard />`（HOME-4 预留的锚点） |
| 前端 API | `frontend/src/api/business/flow-api.ts` | `FlowTaskItem` 增 `biz_type/biz_id/biz_no`；`listDoneTasks` 增 `since` |
| 后端出参 | `backend/src/business/flow/dto.rs` / `flow_model_inbox.rs` / `handler.rs` | `TaskRes` 增 `biz_type/biz_id/biz_no`（**不新增接口**，列表 SQL 已 JOIN 实例行，顺手带出） |
| 后端查询 | `backend/src/business/flow/dto.rs` / `handler.rs` | `my_done` 支持 `since`（RFC 3339，`completed_at >= since`），用于「今日已办」；非法格式 `400 + 65005` |

**为什么加这三个字段**：卡面 ⑦ 要求「点单号按 `doc_type` 映射到业务详情页」。`/business/flow/task/todo`
原来只回 `instance_no`（`FLW-…` 审批单号）与 `title`，**没有 `biz_type`**，前端无法可靠判定单据类型。
本卡复用了审批详情页既有映射表 `views/business/flow/instance/biz-link.ts`（`biz_type` → 业务路由 + 深链键），
因此只需把实例行上已有的三列（`biz_type` / `biz_id` / `biz_no`）随待办行下发 —— 没有第二套映射、没有新接口。

**为什么加 `since`**：`done` 的 `total` 是**全量历史**已办；底部「今日已办 M 项」不能拿它充数。
加一个可选查询参数比在前端拿第一页（page_size=50）数今天更准（跨页不会少数）。

---

## 二、验收逐条对照（Playwright：`36 passed / 0 failed`，`logs/playwright.txt`）

| 卡面验收 | 结果 | 证据 |
|---|---|---|
| ① 工作台有「待我审批」卡 | ✅ | `ui/01-workbench-approval-card.png`；`[data-testid=home-approval-card]` 计数 = 1、卡标题「待我审批」 |
| ② 最多 10 条 + 底部真实总数 | ✅ | 实测 12 条待办 → 渲染 10 行、底部「待办 11 项」（另 1 张已办） |
| ③ 类型+单号 / 提交人 / 相对时间 / 金额 | ✅ | 首行「采购订单 PO-20261109-000003 · 张三（演示发起人） · 30 分钟前 · ¥12,800」；金额 `¥150,000` 千分位；**无金额单据整行不出现 ¥**；相对时间覆盖 分钟 / 小时 / 天 / 超过 7 天显示 `2026-11-01` |
| ④ 同意 → 确认框 → 接口 → 该条消失 + Toast | ✅ | `ui/03-approve-dialog.png`、`ui/04-after-approve.png`；Toast「已同意」，该行移除，底部 11→10、今日已办 1→2 |
| ⑤ 驳回必须填意见（< 5 字按钮禁用）→ 接口 → 消失 | ✅ | `ui/05-reject-dialog-valid.png`；空 / 3 字时「确认驳回」disabled，≥5 字可用；提交后该行消失，底部 10→9、今日已办 2→3 |
| ⑥ 加签 → 选人 → 接口（该条保留） | ✅ | `ui/06-addsign-dialog.png`；未选人按钮 disabled；选 `h5clerk` 后 Toast「已加签，仍需你继续审批」，该行**仍在**、总数不变 |
| ⑦ 点单号按 `doc_type` 跳业务详情 | ✅ | `ui/02-doc-jump-purchase-order.png`；PO → `/business/erp/purchase-order?po_id=<biz_id>` |
| ⑧ 无待办空态 | ✅ | `ui/08-viewer-empty.png`；「没有待我审批的单据」+ 待办 0 项 |
| ⑨ 非超管也能看到自己的待办 | ✅ | `h5viewer`（EMPLOYEE）同样渲染该卡；首页 401/403 = 0、console error = 0 |
| ⑩ typecheck + build + push | ✅ | `logs/typecheck.txt`、`logs/build.txt`、`logs/palettes.txt` 全 exit 0 |

真实接口留证：`api/api-evidence.txt`（真 token，打码；隔离实例 18097 / 库 cloudpivot_home5_v4）——
待办首行的 `biz_type/biz_no/biz_id/form_data.amount`、`done?since` vs 不传 `since` 的差值、
驳回意见 2 字被后端拦（`400 + 65005`）、同意 / 加签回执、加签后 `approver_count` 1→2。

---

## 三、复跑方法

```bash
# 1) 建库 + 起隔离后端（18097）
sudo -u postgres createdb -O alun cloudpivot_home5_v4
cd backend && NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18097 \
  NIK_DATABASE_NAME=cloudpivot_home5_v4 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_ADMIN_PASSWORD='Admin@123456' NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) ./target/debug/cloudpivot

# 2) 账号（admin 改密 / h5viewer / h5clerk）+ 审批岗（admin 挂角色）+ EMPLOYEE 发起权限
bash docs/evidence/home5-20261109/scripts/setup-accounts.sh
bash docs/evidence/home5-20261109/scripts/setup-approvers.sh

# 3) 12 张真实审批单（clerk 发起 → 待办派给 admin）+ 到达时间回填 + 夹具收尾 + 先办 1 张
python3 docs/evidence/home5-20261109/scripts/seed-instances.py
bash   docs/evidence/home5-20261109/scripts/backfill-time.sh
bash   docs/evidence/home5-20261109/scripts/fixture-post.sh
python3 docs/evidence/home5-20261109/scripts/approve-one.py

# 4) 前端 dev 指向隔离后端 + UI 验收
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18097 pnpm dev --port 5403 --strictPort
BASE=http://127.0.0.1:5403 node docs/evidence/home5-20261109/scripts/verify-ui.mjs

# 5) 真实接口证据（会消耗 1 张待办 + 1 次加签）
python3 docs/evidence/home5-20261109/scripts/api-evidence.py
python3 docs/evidence/home5-20261109/scripts/since-proof.py   # 先把一张办结任务改成 5 天前再跑
```

---

## 四、偏差与取舍（明写，不藏）

1. **后端加了 3 个出参字段 + 1 个可选查询参数，没有新增接口。**
   卡面「不要新建后端接口」指的是 `my_todo / my_done / 三个动作` 不要重造 —— 这三条都没动路径与权限码；
   只是把实例行已有的 `biz_type/biz_id/biz_no` 随待办行下发（SQL 本来就 JOIN 了 `biz_flow_instance`），
   并给 `done` 加 `since` 让「今日已办」是**真今日**而不是全量历史。均带 `#[cfg(test)] mod tests`。
2. **夹具只用 PO / 合同 / 报销三类审批单。**
   付款 / 收货类在后台有「孤儿实例回收」任务（`payable_bridge::reconcile_orphan_payments` 等）：
   验收库里没有真实付款单 / 收货单，实例建出来会被定时任务按「单据不在审批中」自动终止
   （实测：`PAY-20261109-000002` 建单 1 分钟后被回收，`付款记录已不处于审批中，付款审批自动终止`）。
   所以卡片映射表里 `PAYABLE_PAY` / `STOCK_IN_POST` 等分支没有用真单据点过；
   它们走的是同一张 `biz-link.ts` 映射表（审批详情页「关联业务」早已在用），本卡没有另写映射。
3. **加签一次一人。** 复用 `UserSelect` 单选（与审批中心 / 审批详情同款），后端收 `user_ids[]`；
   加签成功后该条保留（多一个人会签），Toast 明说「仍需你继续审批」。
4. **「今日已办」用本地零点**（`dayjs().startOf('day').toISOString()`）转 UTC 传给后端；
   跨时区用户的「今天」按自己浏览器时区算，与工作台其它卡片口径一致。
5. **非超管 403 的坑已修在组件里**：`t-dialog` 即使隐藏也会挂载内容，`UserSelect` 挂载时会预取
   `GET /system/user/page`。首版非超管一进首页就吃一个 403（Playwright 实测 console error 1）；
   已给加签框的 `UserSelect` 加 `v-if="addSignVisible"`，只在真正打开时才挂载 → 403 归零。
6. **145 真机未用于本卡 curl。** 145 的 `admin` 口令 2026-11-09 20:00 已被用户本人改过
   （见 `docs/evidence/2026-11-09-db-backup`），且 145 上部署的后端**早于本次改动**（没有 `biz_*` 字段），
   拿它 curl 只能验到旧行为。本卡证据用隔离实例 + 真 token + 真库真流程，命令与返回码全部落盘（`api/api-evidence.txt`）。
7. **`cargo fmt` 会顺带格式化工作区里其它卡未提交的 Rust 文件**（`serial_handler.rs` / `state_machine.rs` 等）。
   本卡 commit 只 `git add` 自己改的 6 个后端/前端文件，不把别人的在途改动带进提交。
