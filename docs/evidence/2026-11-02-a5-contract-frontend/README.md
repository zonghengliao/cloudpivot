# A5 合同管理前端页面 · 真机验收证据（2026-11-02）

卡：`t_9090973d`【A5】合同管理前端页面（列表 + 表单 + 状态流转 + 8 条单测）
代码提交：`0447212 feat(erp): A5 合同管理页面（列表 + 表单 + 状态流转 + 11 条单测）`

## 一、门禁数字（本机实跑）

| 项 | 命令 | 结果 |
|---|---|---|
| 类型检查 | `pnpm typecheck`（`vue-tsc --noEmit`） | **0 错误** |
| 构建 | `pnpm build`（`vue-tsc && vite build`） | **成功**（产物含 `contract-form-name` 的 chunk：`assets/index-D9Zl6weU.js`） |
| 单测（全量） | `pnpm test:unit`（`tsx --test "tests/unit/**/*.test.ts"`） | **473 passed / 0 failed**（本卡新增 11 条，基线 462） |
| 单测（本卡） | `pnpm test:unit` 里的 `erp-contract.test.ts` | **11/11 pass**（卡面要求 ≥8） |

> 构建在**仓库外的一份克隆**里跑（`<scratch>/a5/repo`，`node_modules` 软链自 `/opt/cloudpivot/frontend`），
> **没有**在 `/opt/cloudpivot/frontend` 里 `pnpm build` —— 那目录是 nginx 的生产 `dist`，
> 里面还躺着同事未提交的采购页改动。

本卡新增的 11 条单测（文件 `frontend/tests/unit/erp-contract.test.ts`）对应卡面验收 4–12：

| 用例 | 钉住什么 |
|---|---|
| ④ 列表渲染 | `contractListState` 行数 / 总数 / `isEmpty`（含 `list:null` 兜底） |
| ⑤ 列表空态 | 「还没有合同」vs「没有匹配的合同」两种文案 + 筛选态判定 |
| ⑥ 状态徽标 | 5 个状态的文案 + `t-tag` 主题（灰/绿/蓝/橙）+ 圆点必须是主题变量；并比对后端 `status_label` 逐字一致、边表里没有 `CANCELLED` |
| ⑦ 状态按钮 | `DRAFT→[登记签订]`、`SIGNED→[生效,终止]`、`ACTIVE→[完成,终止]`、终态无按钮；**逐条 `readFileSync` 比对 `state_machine.rs::CONTRACT_EDGES` / `CONTRACT_REASON_EDGES`**；`next_statuses` 优先；流转体只在需 reason 的边带 reason |
| ⑧ 表单：名称 | 空 / 超 200 字被拦，且不牵连其它字段 |
| ⑨ 表单：乙方 | 未选被拦 |
| ⑩ 表单：日期 | 止 < 起、签订晚于生效被拦；同日合法；缺日期被拦 |
| ⑪ 编辑入口 | 非 DRAFT 无编辑 / 删除入口；`can_edit` 优先；模板里不许另写 `row.status === 'DRAFT'` |
| ⑫ 金额显示 | `¥1,234,567.89`（千分位 + ¥），列表与详情都走 `contractAmountText` |
| ⑬ 请求体 | 空筛选只发分页；区间映射到 `sign_date_*` / `expire_date_*`；建单/改单不带 `contract_no` / `status`；清空日期显式传 `null` |
| ⑭ 编辑回填 | `Decimal` 字符串 → number；`null` 日期 → 空串；类型下拉只放开「采购合同」 |

## 二、真机（隔离实例 + 独立库）

环境（全程本机，不动生产 144）：

```
后端：/opt/cloudpivot/backend/target/debug/cloudpivot（19:01 构建，含 A2 合同 6 接口）
      NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18085
      NIK_DATABASE_NAME=cloudpivot_a5（从 cloudpivot_a4_inv 克隆）
前端：<scratch>/a5/repo/frontend/dist（本卡代码构建） 由 serve.mjs 静态服务 + /api 反代
      → http://127.0.0.1:5215
驱动：Playwright（chromium headless，1600×1000），verify.mjs 28 条断言
```

前置验证（开工先验，卡面要求）：

| 检查 | 结果 |
|---|---|
| `GET /api/business/erp/contract/page` 真实 token | 200 + `{list,page,page_size,total}` |
| 权限码 `erp:contract:{list,create,update,delete,status}` | V0197 已种 + 接口注解命中（admin 超管短路；业务码 403 未出现） |
| `GET /api/business/erp/purchase-order/page?contract_id=` 反查 | 200（用它做「关联采购单」，不改后端） |
| V0200 迁移 | 已应用（`schema_migrations` 记录 `erp_contract_menu_visible` / 200），菜单 `is_visible=t` |

### 真机操作步骤（verify.mjs 实跑顺序，28/28 PASS）

1. `/login` 用 `admin` 登录 → 跳到 `/dashboard`；
2. 直接打开 `/business/erp/contract`：标题「合同管理 - 云枢 CloudPivot」（**不是 404 页**）、筛选区渲染、空库给空态提示；
3. 点「新建合同」→ 填名称、金额 `888888.88`、选乙方（示例供应商）、日期取默认（今天 / 今天+1 年）→ 保存：
   接口 200，返回单号 `HT-20261102-0000xx`，列表出现该行走；
4. 金额列读回 `¥888,888.88`（千分位 + ¥）；
5. 点行内「登记签订」（DRAFT 的唯一合法边）→ 弹窗确认 → `PATCH /status` 200；
6. 点「生效」→ `PATCH /status` 200 → 徽标变 `已生效` 且 class 含 `t-tag--success`（绿）；
7. 已生效行：**没有**「删除」、**没有**「编辑」入口（`count=0`）；
8. 点「终止」→ 弹窗必填原因（`A5 真机验收：模拟供应商违约终止`）→ `PATCH /status` 200 → 徽标 `已终止` + `t-tag--warning`（橙）；
9. 点「查看」→ 详情抽屉：字段铺开、金额 `¥888,888.88`、状态徽标、状态流转区（终态无按钮，给说明文案）、关联采购单区块（0 张 → 空态说明）；
10. 再建一张草稿 → 点「删除」→ 确认弹窗 → `DELETE` 200 → 列表消失；**刷新后仍不在**（软删生效）。

控制台 / 网络：**页面级 JS 异常 0、合同页自身 4xx/5xx 0**。
唯一的 401 是全局既有的 `/api/business/notice/stream?token=`（SSE）：`alun 0.4.2` 认证层**没有**
`NIK_AUTH_QUERY_TOKEN_PATHS` 白名单实现，所以每个页面（`/dashboard`、`/supplier` 同样）都会挂一条 ——
用 `probe-sse.mjs` 单独取证，见 `sse-401-global.txt`。与本卡无关，但值得单开一张卡。

## 三、截图（6 张，`docs/evidence/2026-11-02-a5-contract-frontend/`）

| 文件 | 内容 |
|---|---|
| `01-list-empty.png` | 空库列表 + 空态文案 |
| `02-form-create.png` | 新建合同表单（字段齐全、类型=采购合同、校验口径提示） |
| `05-list-with-rows.png` | 列表有数据（含状态列「草稿」、操作列 查看/编辑/删除/登记签订） |
| `03-status-active.png` | 生效后：状态徽标「已生效」绿色，操作列只剩 查看/完成/终止 |
| `06-status-terminated.png` | 终止后：徽标「已终止」橙色 |
| `04-detail-drawer.png` | 详情抽屉（字段铺开 + 终态说明 + 关联采购单空态） |

`checks.json` 是 28 条断言的原始记录，`verify.log` 是控制台输出原文。

## 四、复现

```bash
# 1) 库
su postgres -c "createdb -T cloudpivot_a4_inv -O alun cloudpivot_a5"
# 2) 后端（合同 6 接口 + V0200）
cd /opt/cloudpivot/backend && NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18085 \
  NIK_DATABASE_NAME=cloudpivot_a5 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  ./target/debug/cloudpivot
# 3) 前端 dist + 反代（本目录的 serve.mjs，改端口自便）
node serve.mjs <clone>/frontend/dist 5215 18085
# 4) 真机断言（需先改 admin 初始密码为 A5@Verify2026）
node verify.mjs http://127.0.0.1:5215 ./evidence
```

## 五、卡面 vs 实际的出入（4 处，均已按已落库 DDL/代码实现）

| 卡面 | 实际 | 处置 |
|---|---|---|
| 5 态含 `CANCELLED`；DRAFT → [生效][取消] | `ck_biz_contract_status` 白名单无 `CANCELLED`，草稿作废走软删；DRAFT 的边是 `→SIGNED`（登记签订），`SIGNED→ACTIVE` 才是生效 | 按真实状态机渲染；验收时多一步「登记签订」 |
| 金额 `≥ 0` | `ck_biz_contract_amount CHECK (amount > 0)`，0 元合同后端 400 | 前端同样拦 `amount > 0`（不让用户白填） |
| 税率、付款条件 | `V0195` 23 列里没有 `tax_rate` / `payment_terms` | 表单不提供（注释与卡里都写明） |
| `meta.auth: 'erp:contract:list'` | 本仓 `RouteMeta` 只有 `permission` | 用 `meta.permission`（与全站一致） |
| 删除「仅 DRAFT 且未被引用」 | 后端 `DELETE` 只做引用守卫（任何状态都能删） | 前端按卡面只给 DRAFT 显示删除；被引用时后端 409+66018 文案原样弹出 |
