# M5-07 分批法「前端批次成本页」· 证据包

- 卡：`t_b26f00e5`（父卡 `t_d77cbe01` M5-06 分批法全链路单测）
- 日期：2026-11-19
- 主机：99（开发机），库在 145（`172.18.10.145:5432`，用户 `cloudpivot`）
- 交付物：**一个页面 + 一层 API + 纯逻辑单测 + 一条菜单迁移**，零新接口、零新权限码

## 一句话结论

后端 8 条接口（M5-02…M5-05）在页面上全部接通：归集表 / 明细 / 三路来源+归集 / 结转试算+结转 /
在制余额 / 在制台账。真跑 HTTP 冒烟 **53 项断言全绿**、菜单不变量 **8/8 全绿**、纯逻辑单测 **24 条全绿**、
前端 typecheck + build 通过。

## 交付清单

| 文件 | 内容 |
|---|---|
| `frontend/src/api/business/erp/cost-batch-kinds.ts` | 8 条接口的**全部**出入参类型（逐字抄 4 个后端 DTO 文件）+ 词表（状态 / 要素 / 三态 / 排序 / 权限码） |
| `frontend/src/api/business/erp/cost-batch.ts` | 8 条接口封装（路径写全 `/api`，约定 9） |
| `frontend/src/views/business/erp/cost-batch/cost-batch-logic.ts` | 纯逻辑：入参拼装、整数分相加、行拍平、三态主题、闸门文案、台账自洽校验 |
| `frontend/src/views/business/erp/cost-batch/index.vue` | 页面：页签①归集表 / 页签②在制余额 / 批次抽屉（明细 · 来源 · 结转 · 台账） |
| `frontend/tests/unit/erp-cost-batch.test.ts` | 24 条纯逻辑单测 |
| `frontend/src/router/index.ts` | 路由 `business/erp/cost-batch`（手工注册，漏了 = 菜单 404） |
| `backend/app/migrations/business/V0462__cost_batch_menu.{up,down}.sql` | 菜单 1 行（`/business/erp/cost-batch`，sort 70，图标 `chart-bar`），读码**复用** `business:erp:cost-object:list`，授权**派生**自「谁已持有该读码」 |

## 页面结构（一屏说清）

```
页签 ①「批次成本」= 归集表：一行一批
    列：批次号 / 生产订单号 / 产出品 / 车间 / 计划·完工·在制 / 完工率 / 状态
        / 直接材料 / 直接人工 / 制造费用 / 合计 / 归集行 / 最后归集
    页脚：**整张表**合计（批次数 + 行数 + 四金额，不随翻页变）
    筛选：期间（月，必填，记住上次选择）/ 状态 / 车间 / 排序 / 关键字
页签 ②「在制余额」= 截止期间快照：累计归集 / 已结转 / 余额（料·工·费·合计）/ 本期投入·转出 / 提示标签
    页脚：整表合计 + 「N 批为负」红字（余额不夹零，负数=要查账）
抽屉（点批次号 / 行）：批次抬头 + 4 个子页签
    ∟ 归集明细：本期逐笔归集行（要素 / 来源类型 / 来源单号 / 物料 / 数量 / 单价 / 金额）
    ∟ 来源与归集：料 / 工 / 费三张卡（每行标 已归集 / 待归集 / 不可归集 + 原因）+「归集本期」按钮
    ∟ 完工结转：闸门逐条（为什么 + 怎么修）/ 累计·已结转·本次待结转 / 单位成本 / 历史结转单 / 「结转」按钮
    ∟ 在制台账：逐期「期初 / 投入 / 转出 / 期末」+ 汇总 + 之后期间活动 + 已结算口径 + 警告 + 口径说明
```

## 四条口径（都在代码注释里写死，页面不许弱化）

1. **期间必填**：后端 `period_of` 拒绝缺省；页面用 `todayPeriod()` 做初值 + localStorage 记忆，
   并在 `buildSheetQuery` / `buildWipQuery` 里做兜底（`ListPage` 的「重置」会把 `custom` 筛选清成
   `undefined`，直发就 400 —— 单测 `期间被清空时回落本月` 钉住）。
2. **不能归集 / 不能结转时先说「为什么」再说「怎么修」**：`block_reason + block_fix`、
   `blockers[].message + fix` 整段展示（`blockerLines`）。
3. **按钮置灰原因写在按钮旁**（`finishDisabledReason`），不是点下去才吃 409。
4. **在制余额不夹零**：负数原样显示 + 红字 + `negative_batch_count` 汇总。

## 真跑证据（文件即输出，未加工）

| 文件 | 说明 | 结果 |
|---|---|---|
| `01_migration_idempotent.out` | 克隆库（V0404）上：首次应用 1 页面 + 3 授权 → 重放 0 行 → 两条反证（sort 撞车 / `is_visible=false`）被自检拒绝 → down 删 3+1 → 再 down 0 行 → 再 up 复原 → menu_invariants 8/8 | 全绿 |
| `02_http_smoke.py` / `02_http_smoke.out` | 真起实例（`cp_m507_e2e`，迁移到 **V0462**）：登录 → 建批次 → 8 条接口逐条断言前端要用的字段 → 归集（真写）→ 结转（409 + 66 107）→ 6 条 404 口径 → 缺期间 400 | **53 PASS / 0 FAIL** |
| `02b_shape_teeth.py` / `.out` | 反证：按 DTO 字面写**嵌套** `totals.amounts` 拿到 `None`（页脚合计恒 0、编译期无感），只有真跑才抓得到 | 反证成立 |
| `03_shape_dump.py` / `.out` | 归集表 / 余额表**原始 JSON**（前沿类型对齐用） | — |
| `04_menu_invariants_fresh_full_chain.out` | 空库首启全链（V0462 由真迁移器跑）后的 menu_invariants | 8/8 全绿（569 条菜单） |
| `05_unit_tests.out` / `06_frontend_gate.out` | 纯逻辑单测 + `check:palettes` / `typecheck` / `typecheck:unit` / `build` | 24 条全绿 / 全绿 |

## 本卡抓到的真问题（真跑 / 真门禁才看得见）

**归集表出参是「扁的」，不是嵌套的。** `SheetTotalsRes` 与 `SheetRowRes` 对 `amounts` 用了
`#[serde(flatten)]`：真响应是

```json
{ "totals": { "mat": "0.0000", "labor": "0.0000", "oh": "0.0000", "total": "0.0000",
              "line_count": 0, "batch_count": 0 },
  "list": [ { "object_no": "CB-20261119-000001", ..., "mat": "0.0000", "labor": "0.0000",
              "oh": "0.0000", "total": "0.0000", "line_count": 0 } ] }
```

按 Rust 结构体字面写 `{ amounts: {...} }` 的话：**编译期完全无感，运行时页脚合计恒为 0**
（`formatMoney(undefined)` → `0.00`）。首轮冒烟就是在这 3 条断言上红的（`totals 结构` /
`SheetAmounts 四字段` / `SheetRow 字段`），修完 TS 类型后全绿。
细节：明细 / 来源 / 归集三处的 `totals` **仍是嵌套** `SheetAmounts` —— 同一页两种形状，
`cost-batch-kinds.ts` 的模块头与 `SheetTotals` 注释都写了「别混」。

**迁移里照抄 V0460 的 `ADMIN` 角色码会被 pre-push 拦下。** V0460 的授权矩阵里
有 `('SUPER_ADMIN', all_ids), ('ADMIN', all_ids)`，但本仓 `sys_role` 只有 12 个活角色、
**没有 `ADMIN`**（`scripts/verify/role_code_gate.py` 白名单里也没有）。照抄的结果是
「那行永远 0 行」+ push 直接红（`✗ 未知角色码 1 处：V0462 → 'ADMIN'@L127`）。
已改为只补 `SUPER_ADMIN`，并在迁移注释里写明「别照抄 V0460 的 ADMIN」。
这说明**别把别的迁移的授权矩阵当模板整段复制** —— 角色码是环境事实，不是格式。

## 边界（本卡没做的事）

- 零后端 Rust 改动（页面只是既有 8 条接口的消费者）；
- 零新权限码 / 零按钮权限行：读走 `cost-object:list`、写走 `cost-period:calc`，两者 V0378 已种已授；
- 零数据夹具：真跑用的库是 `make_full_suite_db.sh` 产出的干净库 `cp_m507_e2e`（用完即 drop），
  演练克隆 `cp_m507_drill` 也 drop；
- 交接：`erp.cost.enabled` 默认 `false`，演练/验收前要打开（本证据里用 psql 置 true +
  `POST /api/support/config/reload`，与 `scripts/verify/cost/cost_e2e.py` 同口径）。
