# SO-37 佣金前端（规则配置 + 结算查询）· 验收证据

卡号 `t_32ac0e6e`｜ARCH-7 §15.4｜上游后端：SO-36（计提/结算，`03dd1e5`）+ SO-36b（规则 CRUD，`93ddf8a`）

本卡交付**前端两页 + 销售订单销售员选择器 + 路由 + 菜单收尾迁移**，后端一行未改
（规则 CRUD 由 SO-36b 并行交付，本卡按 DTO 逐字对齐并回归）。

---

## 一、交付物

| 文件 | 说明 |
|------|------|
| `frontend/src/api/business/erp/commission.ts` | 8 个接口（规则 4 + 结算 4），字段逐字抄 `commission_rule_dto.rs` / `commission_dto.rs` |
| `frontend/src/views/business/erp/commission-rule/index.vue` | 佣金规则页：列表 9 列 + 服务端分页 + 3 过滤；新建/编辑弹窗（范围 / 对象 / 类型 / 费率 / 起止金额 / 有效期 / 状态 / 备注）；停用·启用·删除 |
| `frontend/src/views/business/erp/commission-rule/rule-logic.ts` | 纯逻辑（校验 / 入参 / 展示口径 / 空态） |
| `frontend/src/views/business/erp/commission/index.vue` | 佣金结算页：列表 8 列 + 4 过滤 + 本页合计；生成结算单（期间 + 销售员）；明细**展开懒加载**（订单 / 应收 / 规则 / 基数 / 佣金）；审批 / 发放 |
| `frontend/src/views/business/erp/commission/commission-logic.ts` | 纯逻辑（状态机 / 期间候选 / 生成入参 / 冲回口径 / 合计） |
| `frontend/src/router/index.ts` | 两条路由：`business/erp/commission-rule`、`business/erp/commission` |
| `frontend/src/views/business/erp/sales-order/{index.vue,sales-order-query.ts}` + `api/business/erp/index.ts` | 销售订单表单加**销售员选择器**（留空 = 不指定，不默认建单人）；详情抽屉显示销售员 |
| `backend/migrations/business/V0312__commission_menu_page.{up,down}.sql` | 把 V0310 留空的 path/component/可见性补齐、归位「销售管理」目录（sort 80/90）、title 收敛成短标签；往返自检 |
| `frontend/tests/unit/erp-commission.test.ts` | 16 条纯逻辑用例（含「源码即契约」：状态边 / 7 个权限码逐字比对后端与迁移） |
| `scripts/verify/so37_instance.sh` / `so37_curl_evidence.sh` | 隔离实例 + 58 条真实 token curl 断言 |

---

## 二、口径（写错会算错钱，全部抄后端）

| 项 | 真口径 | 出处 |
|----|--------|------|
| 范围 ↔ 对象 | `ALL ⇒ scope_id = null`；`SALESPERSON / MATERIAL_CATEGORY ⇒ 必填` | `ck_biz_cmrule_scope_id` |
| 费率语义 | `PERCENT` 是**百分数**（5 = 5%）；`FIXED` 是每笔金额 | `ck_biz_cmrule_rate` |
| 金额区间 | `min_amount ≥ 0`、`max_amount ≥ min_amount`（null = 不封顶） | `ck_biz_cmrule_amount` |
| 有效期 | 两端都含，null = 长期 | `ck_biz_cmrule_effective` |
| 状态机 | `DRAFT → APPROVED → PAID`，**无回退边**；跳步 / 回退 409 | `commission_dto::LEGAL_EDGES` |
| 佣金基数 | 应收**已收款**（`received_amount`）− 已认领；**有符号**（退货 / 退款落负） | `commission_handler` |
| 凭证 | `APPROVED` 计提（借 5601 / 贷 2241.02）；`PAID` 付款（借 2241.02 / 贷 1002） | `post_commission_voucher` |

前端三条「防算错钱」的硬约束（都有单测）：

1. 切 `calc_type` **清空 `rate`**（否则 5% 秒变 5 元/单）；
2. 切成 `ALL` **清空 `scope_id`**（否则库层 CHECK 400）；
3. 生成结算单时销售员留空 = **不发该字段**（后端按「有可计佣差额的人」各生成一张），不是发空串。

---

## 三、门禁

```
pnpm typecheck            ✔ 干净（vue-tsc --noEmit）
vite build                ✔ EXIT=0（outDir 指到工作区外 dist-so37，**未覆盖生产 dist**）
pnpm check:palettes       ✔ 全部通过 ✓（色阶 330 条断言 / 22 套方案）
pnpm test:unit            ✔ 1029 条中 1028 通过
                          唯一失败是 flow-biz-link.test.ts「后端 FlowBizType 16 ≠ 前端 15」——
                          属 HEAD 既有缺陷（SO-14 加了 RECEIPT_REFUND，前端词表没跟），与本卡无关
cargo test --lib          —（本卡不改后端；SO-36/SO-36b 各自已跑全绿）
```

> `pnpm build` 的字面命令不能在 `/opt/cloudpivot/frontend` 直接跑：那是生产 nginx 的 `root`，
> 会把半成品发上线。等价做法：`pnpm typecheck`（同一条命令的前半段）+ `vite build --outDir <工作区外>`。

---

## 四、真机验收（`ui_verify.log`，39/39 通过）

环境：隔离库 `cloudpivot_so37`（克隆 `cloudpivot_so36`，端口 18244，二进制 05:39 含 SO-36b 修复）
+ 本卡 dist 的静态托管（5237，`/api` 反代 18244）+ Chromium（Playwright 1.64）。

| 断言组 | 条数 | 结果 |
|--------|------|------|
| 规则页基础：列表接口 200 / 三条夹具规则在列 / 范围对象显示**中文名**（非裸 UUID）/ 百分比带 % / 封顶空显示「不封顶」/ 分页条 | 6 | ✔ |
| 规则页筛选：发 `scope_type=SALESPERSON` 且筛后只剩销售员级 | 2 | ✔ |
| 新建弹窗：切「按物料」出现物料选择器 / 费率落到 15 / 请求体字段齐全 / **后端 200** / 新规则出现在首行 | 5 | ✔ |
| 结算页 + 生成结算单：列表 200 / 期间默认当月 / POST 200 / 请求体只带 `period_code`(全员) / 结果提示「已生成 2 张」/ 列表出现 `CM-` 单号 / 销售员列 / 本页合计条 | 8 | ✔ |
| 明细展开：按需拉详情 200 / 渲染明细区块 / 订单号 / 应收号 / 命中规则名 / 列头齐全 / 明细合计 | 7 | ✔ |
| 审批：弹窗写明「借 5601 / 贷 2241.02」→ PATCH 200 → 状态列「已审批」 | 3 | ✔ |
| 发放：弹窗写明「借 2241.02 / 贷 1002」+ 发放说明输入框 → PATCH 200 → 「已发放」→ 终态不再显示发放按钮 | 5 | ✔ |
| 销售订单页：列表 200 / 表单出现销售员选择器 / 能选中并回显「SO36 销售员甲（so36_sp1）」 | 3 | ✔ |
| **合计** | **39** | **39/39 ✔** |

截图（同目录）：

| 文件 | 内容 |
|------|------|
| `01-commission-rule-list.png` | 佣金规则列表（范围 / 类型 / 费率 / 起止·封顶 / 有效期 / 状态 / 操作） |
| `02-commission-rule-create-dialog.png` | 新建弹窗（按物料 + 15% + 有效期 + 状态） |
| `03-commission-rule-created.png` | 新建成功后列表首行 |
| `04-commission-settle-dialog.png` | 生成结算单弹窗（期间 + 销售员 + 备注） |
| `05-commission-settle-result.png` | 生成结果（已生成 2 张草稿结算单） |
| `06-commission-items.png` | **两行结算单同时展开**：逐笔订单 / 应收 / 命中规则 / 基数 / 佣金 + 底部合计 |
| `07-commission-approved.png` | 审批后状态「已审批」 |
| `08-commission-paid.png` | 发放后状态「已发放」、终态按钮消失 |
| `09-sales-order-salesperson.png` | 销售订单表单里的销售员选择器 |

---

## 五、接口面证据（`curl_transcript.txt`，58/58 通过）

真 token（`admin`）打隔离实例，覆盖：

- 规则列表 / `scope_type` 过滤 / `keyword` 过滤（规则名 + 范围名 + 备注）/ `status` 过滤；
- 新建（回单含 `scope_name`、`rate` 是 `"15.0000"` 字符串、`effective_to: null`）；
- **五条库层约束的 400**：`ALL` 带 `scope_id`、`PERCENT > 100`、`max_amount < min_amount`、
  `effective_to < effective_from`、`scope_id` 指向不存在的账号；
- 编辑（部分更新：只改 `rate`，`rule_name` 保持）、停用（`status=INACTIVE` 且能在 INACTIVE 过滤里查到）、
  软删（列表回到 3 条）；
- 生成结算单（2 张）、分页 `item_count`、详情明细（订单号 / 应收号 / 规则名 / Σ佣金 = 单头）、
  `DRAFT→PAID` 409、`DRAFT→APPROVED` 出 `PZ-` 凭证、`APPROVED→DRAFT` 409、`APPROVED→PAID`、
  已审批 / 已发放不被重算（进 `skipped`）；
- 冲回：2026-12 把 AR-B 已收 1000 → 600，生成 `-40.0000` 佣金、明细基数为负、备注含「冲回/退」；
- 权限：销售员账号建规则 / 看规则列表 → 403；
- 菜单：`/auth/menus` 里两个节点 path/component 与前端路由逐字一致。

`db_state.txt`：库内终态（菜单两行、4 条规则含页面新建那条、2 张结算单、3 条明细、
4 张 `COMMISSION` 凭证含 5601/2241.02/1002 分录）。

### 迁移往返（`migration_probe.txt`）

`V0312` 走 `schema_migrations kind=versioned / applied_via=executed`（真实实例记账已核对）。
探针库 `cloudpivot_so37p` 是 `cloudpivot_so36` 的克隆，**该克隆已带 V0312**
（共享工作区里另一个实例先启动过，运行时加载了同一份迁移文件），所以探针记录的是：

| 步骤 | 结果 |
|------|------|
| up（幂等重放） | 6 条 UPDATE 各 **0 行**（`IS DISTINCT FROM` 生效） |
| 终态 | 两个页面：`path` / `component` 正确、`parent=销售管理`、sort 80/90、`is_visible=t` |
| down | 两节点各**还原 1 行** → path/component/parent 归 NULL、sort 88/92、`is_visible=f`、长标题还原 |
| down（第二次） | 各 **0 行**（幂等） |
| 再 up | 各**应用 1 行** → 回到终态 |

`down` 里额外断言「V0310 的 7 个节点（2 页面 + 5 按钮）一个都没少」——回滚只还原菜单形状，
不带走权限码。

---

## 六、复现

```bash
# 1) 夹具 + 实例（克隆已在 V0311 的库；启动时自动跑 V0312）
runuser -u postgres -- createdb -T cloudpivot_so36 -O alun cloudpivot_so37
bash scripts/verify/so37_instance.sh up
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so37 -f scripts/verify/so36_fixture.sql
bash scripts/verify/so37_curl_evidence.sh          # 58 条断言

# 2) 真机（dist 在仓库外，避免覆盖生产 dist）
cd frontend && pnpm typecheck && NODE_OPTIONS=--max-old-space-size=6144 \
  ./node_modules/.bin/vite build --outDir /root/.hermes/profiles/frontend-developer/cache/scratch/dist-so37 --emptyOutDir
node <工作区>/serve-dist.mjs <dist-so37> 5237 18244    # 静态 + /api 反代
node <工作区>/verify.mjs                                # 39 条 DOM/接口断言 + 截图
```

---

## 七、残留 / 需下游注意

1. **销售员候选要 `system:user:list`**（物料候选要 `business:erp:material:list`），而
   `SALES_CLERK` / `SALES_SUPERVISOR` 在 V0267 的矩阵里**都没有**这两个码。当前实现是
   **降级**：候选拉不到 → 换成手填 UUID + 黄色提示，页面不炸、流程不断。
   彻底解法是后端加一条轻量「销售员候选」接口（如 `GET /business/erp/sales-order/salespersons`），
   把权限挂到 `sale-order:list` 上——建议单开卡。
2. **结算单不写 `biz_status_log`**（SO-36 决策），状态流转靠 `LEGAL_EDGES` + 乐观锁 + 库层 CHECK；
   审批中心看不到佣金结算的流转轨迹，需要与 DocKind 词表扩展同批再做。
3. `MATERIAL_CATEGORY` 本期 `scope_id` = `biz_material.id`（基础版无分类表），页面文案已写清
   「本期绑定的是具体物料」。
4. `pnpm test:unit` 的 `flow-biz-link.test.ts` 失败属 HEAD 既有缺陷（SO-14 加 `RECEIPT_REFUND`
   未同步前端词表），建议单开卡给 backend-developer/frontend-developer 收口。
