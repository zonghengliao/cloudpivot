# FIN-ASSET-ST02 资产停用 / 启用接口设计（设计卡 `t_408342b2`）

> 上游：LNK-09（`t_f874341d`，commit `5f3dfe9`/`7193679`）把 `FIXED_ASSET` 接进联动引擎，只接了
> **两处真实存在的状态推进**（处置、作废处置）。本卡把「停用 / 启用（`IN_USE ⇄ IDLE`）」这个
> **功能缺口**定口径、定契约、定事务顺序、定折旧口径，并拆出 backend / frontend 两张实现卡。
>
> 本卡**不改代码、不加迁移**，只落这一份设计 + 两张卡。

---

## 0. 结论摘要（先看这张表）

| # | 结论 | 一句话依据 |
|---|------|-----------|
| **D1** | **停用 / 启用算「状态变更触发点」，要接联动引擎** | 状态机里这两条是 **mandatory** 边、`doc_kind` 词表有 `IDLE`/`ENABLE`、`spec::TRIGGER_SPECS` 已向配置页列出 `FIXED_ASSET/IDLE` 并允许保存规则 —— 缺的是触发源，不是触发点资格 |
| **D2** | 单一接口 `POST /api/business/erp/asset/{id}/status`，`target_status ∈ {IDLE, IN_USE}` | 状态机是唯一权威：两个接口 = 两份校验 + 两份幂等 + 两套前端签名；一个 `target_status` 让「同态 409」只写一处。**绝不接受 `DISPOSED`**（处置必须走审批 + 落凭证，不能开后门） |
| **D3** | 权限**复用** `business:erp:asset:update`，**不新增权限码** | 本动作不落凭证 / 不改金额 / 不改折旧口径 / 不动部门资金；`update` 本身已允许改部门与使用人（比停用"重"）。新增权限码要 1 张迁移 + `sys_menu` 行 + 角色授权 + 菜单不变量脚本 + 一轮权限矩阵回归 —— 成本远大于收益 |
| **D4** | **`IDLE` 继续计提折旧（口径不变）** | 代码 4 处已钉死（扫描 SQL / `in_depreciation_scope` / 边标签 / `scan_caption`）+ 准则「停用 ≠ 减少」+ 反面推演：若停用即停提，停用就成了**无凭证、无审批的损益调节器**。详见 §4 |
| **D5** | 通知幂等键加**本次事件标识**（变动行 id）：`LNK:FIXED_ASSET:<status>:<asset_id>:<event_ref>`（≤128 字符） | 现有键「状态 + 卡片」对**可逆边**必然失效（停用→启用→再停用，第二次停用不发通知）。本卡第一次引入可逆边，必须同时修掉；顺带修 LNK-09 遗留的真 bug（处置→作废→再处置，第二次不发通知）。字段名用 `event_ref`，与并行在做的 LNK-10（`depreciation_linkage::event_ref`）同名同义。见 §5.3 |
| **D6** | 新增错误码 `ERP_ASSET_STATUS_ILLEGAL = 66046`（**只动 Rust 源码 2 处，无迁移**） | 复用 `66040` 会让前端 `ASSET_ERROR_HINTS[66040]`（"该资产已有在跑的处置审批…"）错配到「已停用」场景 —— 排障会直接指错方向。错误码不是权限码，加号不需要迁移 |
| **D7** | 停用 / 启用**不走审批、不落凭证、不受会计期间限制** | 不产生分录 ⇒ 与结账无关；内控靠「权限码 + `biz_status_log` + `biz_fixed_asset_change`」三重留痕。要升格为审批动作的前提是 D4 被推翻（动损益），那时必须换权限码 + 加流程 |

---

## 1. 现状核实（可复现）

```bash
# ① 停用/启用没有任何 HTTP 入口（除 dto 常量外零引用）
grep -rn "STATUS_IDLE" backend/src | grep -v asset_dto.rs        # 只命中 LIVE_STATUSES / 单测
grep -rn "apply_asset_status" backend/src | grep -v asset_model.rs # 调用方只有单测

# ② 资产接口清单（14 个，其中分类 5 个 + 卡片 9 个），没有一个改状态
grep -n "nik::post\|nik::put\|nik::get\|nik::delete" backend/src/business/erp/asset_handler.rs

# ③ 状态机是齐的
grep -n "FIXED_ASSET_EDGES\|FIXED_ASSET_REASON_EDGES\|FIXED_ASSET_BASE" -A8 \
  backend/src/business/shared/state_machine.rs

# ④ 配置页已经允许在 IDLE 上配规则（缺的只是触发源）
grep -n "ENTITY_FIXED_ASSET" -A6 backend/src/business/flow/linkage/spec.rs

# ⑤ 联动引擎的"调用点"是**按文件去重**统计的（13 个文件，asset_handler.rs 不在其中）
#    判定入口：flow/linkage/tests_wiring.rs::ENGINE_ENTRIES（5 个字面量）
```

**⑤ 的实况**（截至 `7193679` 实测，13 个文件）：`erp/asset_linkage.rs`、`contract_linkage.rs`、`expense_linkage.rs`、
`payable_linkage.rs`、`period_linkage.rs`、`purchase_linkage.rs`、`stockin_handler_post.rs`、
`stockout_handler.rs`、`stockout_linkage.rs`、`stocktake_linkage.rs`、`transfer_linkage.rs`、
`voucher_linkage.rs`、`task/task_handler_notice.rs`。

> ⚠️ **卡面第 4 条要纠正**：卡面写「把 `tests_wiring` 的调用点计数 +1」。
> 实际计数器是 **文件级去重 + 只认 `linkage::engine::*` 直调**，而本卡的新入口写在
> `asset_handler.rs` 里、调的是 `asset_linkage::run_in_tx`（间接调用）⇒ **13 仍是 13，硬改成 14 会立红**。
> 真正要 +1 的是同一个文件里的**函数内调用次数断言**：
> `handler.matches("asset_linkage::run_in_tx(").count()` **1 → 2**、
> `handler.matches("asset_linkage::run_deferred_after_commit(").count()` **1 → 2**。
> 见 §5.4。

---

## 2. 接口契约（可直接照抄）

### 2.1 路由与注解

```rust
/// 停用 / 启用（IN_USE ⇄ IDLE）。状态只走状态机，不落凭证。
#[nik::permission(
    path = "/business/erp/asset/{id}/status",
    method = "POST",
    permission = "business:erp:asset:update"
)]
#[nik::post("/business/erp/asset/{id}/status")]
pub async fn asset_status_change(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StatusChangeReq>,
) -> ResResult<AssetDetailRes>
```

- 路径不含 `/api`（由 `[router] prefix` 注入）；前端必须写全 `/api/business/erp/asset/{id}/status`。
- 与既有路由无冲突：`GET/PUT /asset/{id}` 方法不同、`POST /asset/{id}/dispose{/cancel}` 路径不同。
- 权限码复用 `business:erp:asset:update`（V0229 种子里已有，V0258 角色矩阵已授权）⇒ **零迁移、零菜单改动**。

### 2.2 请求

```json
POST /api/business/erp/asset/0199c2f4-1b6e-7000-8000-000000000071/status
{
  "target_status": "IDLE",
  "reason": "生产线搬迁，设备暂不外借",
  "change_date": "2026-11-07"
}
```

| 字段 | 类型 | 必填 | 规则 |
|------|------|------|------|
| `target_status` | string | 是 | 只接受 `IDLE` / `IN_USE`。其它值（含 `DISPOSED` / `CANCELLED`）→ **400** `PARAM_INVALID`，文案点名「处置请走 `POST /asset/{id}/dispose`」 |
| `reason` | string | 是 | 走既有 `asset_dto::require_reason`：非空、≤255 字。**不另设「≥2 字」下限** —— `transfer` / `custodian` / `dispose/cancel` 三个同类接口都是「非空即可」，同类字段两套规则比少一条下限更坏。前端 `requireReasonError` 也同步保持「非空」 |
| `change_date` | string `YYYY-MM-DD` | 否 | 默认今天（`voucher_today()` = CST）。**只进留痕、不参与任何计算**；与 `transfer`/`custodian` 同口径，不做未来日期校验（单独限制会造成同一字段两套规则；要限制就三个接口一起改，另开卡） |

响应：`AssetDetailRes`（与 `transfer` / `custodian` / `dispose` / `dispose/cancel` 完全同形 —— 前端拿到就能直接刷详情与时间线）。

### 2.3 错误码

| HTTP | code | 场景 | 文案要点（`{no}` = `asset_no`，`{label}` = 中文状态名） |
|------|------|------|------------------|
| 400 | `PARAM_INVALID` | 缺/非法 `target_status`、`reason` 为空或超长 | 说清合法值只有 `IDLE` / `IN_USE` |
| 401 | `AUTH_TOKEN_MISSING` | 令牌无用户 id | 与 `asset_dispose` 同款 |
| 404 | `RESOURCE_NOT_FOUND` | 卡片不存在 / 已软删 | 复用 `not_found()` |
| 409 | **`ERP_ASSET_STATUS_ILLEGAL`（66046，新增）** | ① 目标态 == 当前态（重复停用 / 重复启用）② 卡片不处于 `IN_USE`/`IDLE`（已处置 / 已作废）③ CAS 回写 0 行（并发抢改） | ① `资产 {no} 当前已经是「{label}」：重复停用不是状态变更（不会重复通知）` ② `资产 {no} 当前是「{label}」：只有「在役 / 闲置」的卡片能停用 / 启用；已处置的请先作废处置` |

> **为什么不用 66040**：`66040 = ERP_ASSET_DISPOSAL_STATE`，前端 `ASSET_ERROR_HINTS[66040]`
> 固定映射成「该资产已有在跑的处置审批：请先在流程中心处理完，或作废该审批」。
> 复用它做「重复停用」= 给运维递一条**指向错误方向**的提示。
> 新增码的成本只有 2 处源码：`backend/src/shared/error_code.rs` 的常量 + `erp_error_codes_are_stable`
> 测试表各加一行（66046），**不涉及迁移、种子、授权**（这正是它和 D3 成本判断不同的地方）。

### 2.4 前端（`api/business/erp/asset.ts`）

```ts
/** 停用 / 启用的请求体（逐字抄 `StatusChangeReq`）。 */
export interface AssetStatusChangePayload {
  target_status: 'IDLE' | 'IN_USE';
  reason: string;
  change_date?: string;
}

/** 停用 / 启用（`IN_USE ⇄ IDLE`）：不落凭证、折旧照提。 */
export function changeAssetStatus(
  id: string,
  payload: AssetStatusChangePayload,
): Promise<AssetDetail> {
  return post(`/api/business/erp/asset/${id}/status`, payload);
}
```

---

## 3. 事务顺序（照这个顺序写，别调换）

```
asset_status_change(claims, id, req)
 ⓪ 解析 id（parse_uuid_text）· operator = operator_of(&claims)
    spec = asset_dto::validate_status_change(&req, voucher_today())?
        → target_status ∈ {IDLE, IN_USE}；reason = require_reason(...)；change_date = req.change_date.unwrap_or(today)
 ① tx = db().begin()
 ② row = asset_model::asset_lock_in(&tx, id)?          // SELECT ... FOR UPDATE：并发两次停用只有一个进来
 ③ 同态 / 不可停用判定 → 409 ERP_ASSET_STATUS_ILLEGAL（含 asset_no + status_label）
      row.status == spec.target_status              → 同态拒绝
      !asset_dto::disposable(row.status)            → 已处置/已作废拒绝
 ④ state_machine::transition(&tx, ENTITY_FIXED_ASSET, id, &row.asset_no,
        &asset_bridge::asset_bridge_edges(), &row.status, &spec.target_status,
        operator, Some(&reason), None)?             // 边校验（mandatory 边）+ 写 biz_status_log
 ⑤ affected = asset_model::apply_asset_status(&tx, id, &row.status, &spec.target_status, operator)?
      affected == 0 → 409（CAS 被抢：状态在锁后仍被改，说明有别的写入路径，必须报错不能静默）
 ⑥ event_ref = asset_model::insert_change(&tx, id, &ChangeInsert {
        change_type: CHANGE_IDLE | CHANGE_ENABLE,   // 停用 IDLE / 启用 ENABLE
        change_date: spec.change_date,
        from_dept_id/name, to_dept_id/name: None,   // 变动的是状态，不是部门
        from_user_id/name, to_user_id/name: None,   // 也不是使用人
        from_status: Some(&row.status), to_status: Some(&spec.target_status),
        reason: &reason, voucher_id: None, approval_instance_id: None,
        operator_id: operator, operator_name: Some(operator_name),
        remark: Some("停用/启用：不落凭证，折旧口径不变"),
    })?
 ⑦ change = asset_linkage::run_in_tx(&tx, id, &row.asset_no, &spec.target_status, operator, event_ref)?
 ⑧ tx.commit()?
 ⑨ asset_linkage::run_deferred_after_commit(change).await   // 通知永不回滚业务
 ⑩ Ok(Res::ok(detail_of(id).await?))
 Err(any) → tx.rollback(); Err
```

### 3.1 为什么是这个顺序（每条都有前人踩过的理由）

- **②锁卡在最前**：`FOR UPDATE` 把「同一张卡的两个并发动作」串行化；否则两个请求都读到 `IN_USE`、
  都通过同态校验、都写 `biz_status_log`（两张卡变一张 → 两条状态日志 + 两条通知）。
- **④ 在 ⑤ 之前**：与 `asset_bridge::execute`（②状态机 → ③回填）和 `asset_dispose_cancel`
  （②状态机 → ③恢复）**同序**。`transition` 是唯一状态写入口且会因「边被配置禁用」而拒绝 ——
  让它先判，CAS 只负责落地。**不要为了"看起来更严谨"反过来写**，那会让三处资产状态推进出现两套顺序。
- **⑥ 在 ⑦ 之前**：`event_ref`（= 变动行 id）要进通知幂等键（§5.3）。变动行是 append-only 事件本体，
  先落行、再跑联动，`run_in_tx` 才有稳定的「本次事件」标识。
- **⑦ 在事务内、⑨ 在提交后**：事务内动作失败 → 整笔回滚（不会「卡已停用、联动没跑」）；
  通知失败 → 只记日志（一封站内信不该把停用退回去）—— 与 LNK-09 一处不能差。
- **不做期间结账校验**：不落凭证 ⇒ 与 `biz_accounting_period` 无关。**别照抄处置的 period 检查**。

### 3.2 不拦「处置审批在跑」

`asset_dispose` 会拒绝「已有在跑的处置审批」，但**停用 / 启用不拦**，与 `transfer` / `custodian` 一致：

- 处置审批的单据快照（`disposal_form_data`）里**没有 status**，不会因为停用而失真；
- 状态机允许 `IDLE → DISPOSED`（mandatory 边），审批通过时 `asset_bridge::execute` 用
  `IN_USE|IDLE → DISPOSED` 正常推进，留痕 `from_status=IDLE` 忠实；
- 若只给停用加这道门，就会出现「转移/换人不拦、停用拦」的不对称 —— 评审必然问「凭什么」。

（前端已有的 `canDisposeAsset(status, pendingDisposal)` 会隐藏「处置」按钮，方向相反、不冲突。）

---

## 4. 折旧口径：`IDLE` **继续**计提折旧（本卡最重要的结论）

### 4.1 答案

**停用不改折旧口径。资产处于「闲置（IDLE）」时，仍按月计提折旧，金额、部门归集、计提起始月全部不变。**
停用只改两件事：① 状态标签；② 与状态相关的动作可见性（仍可转移 / 换人 / 处置）。

### 4.2 依据（代码 4 处 + 准则 + 内控，逐条可核）

| # | 依据 | 位置 | 内容 |
|---|------|------|------|
| 1 | 折旧扫描 SQL 只认状态 | `backend/src/business/erp/asset_model.rs::scan_assets_for_period` | `AND a.status IN ('IN_USE','IDLE')` |
| 2 | 口径函数显式返回 `true` | `backend/src/business/erp/depreciation_model.rs::in_depreciation_scope` | `status != STATUS_DISPOSED && asset_dto::disposable(status)`；单测 `assert!(in_depreciation_scope("IDLE"))` |
| 3 | 状态机边标签写死 | `backend/src/business/shared/state_machine.rs::FIXED_ASSET_BASE` | `BaseEdge::mandatory("IN_USE", "IDLE", "停用（闲置，照提折旧）", true)` |
| 4 | 页面口径文案 | `backend/src/business/erp/asset_dto.rs::scan_caption` | 「在役 + 闲置、已到计提起始日、未提足、本期处置的也算」 |
| 5 | 设计基线 | `docs/architecture/ARCH-6-财务扩展设计.md` **§5.5.2** 筛选 SQL（注：卡面写的 §5.3 是「卡片字段口径：原值/残值/购入模式」，折旧筛选 SQL 在 §5.5.2；两处都读过，口径一致） | `AND a.status IN ('IN_USE','IDLE')  -- 在役 / 闲置都要提` |
| 6 | 准则口径 | 《企业会计准则第 4 号——固定资产》 | 「对所有固定资产计提折旧」，仅豁免「已提足折旧仍继续使用」与「单独计价入账的土地」；「当月减少的固定资产当月仍计提，从下月起不提」——**停用 / 闲置不构成"减少"，减少只由处置（出售/报废/毁损/盘亏）构成**。条文号请财务复核（本卡只按口径引用，未逐字核对条序） |

### 4.3 反面推演：如果停用即停提，会发生什么

1. **无凭证、无审批的损益调节器**：本接口不落凭证、不走审批（D7）。若停用即停提，
   任何人都能在期末把资产停用一个月 → 少提一笔折旧 → 利润变好看 → 下月启用。
   **这条通道一开，折旧就不再是会计估计，而是按钮。**
2. **与已发布设计冲突**：§4.2 的 1/2/3/4/5 五处都要一起改，其中 3 是 **mandatory 边标签**
   （配置层不许关），改它等于改内控声明，必须重开设计评审。
3. **"停用"语义会漂移**：中文里"停用设备"通常指"暂不使用"，不是"停止计提"；
   真要停止计提，会计动作是**变更折旧年限/方法**或**转入在建工程**，那是另一条业务。
4. **跨期反冲无出口**：若停 1 个月停提，再启用时是否补提？补提就对不上"当月减少"规则，
   不补提就永久少提 —— 两者都要新增红冲/补提路径，成本远超本卡。

### 4.4 边界（写清楚，免得被当漏洞用）

- 停用期间折旧费用**仍按卡片 `use_dept_id` 归集**（停用不改部门；改部门只走 `transfer`）。
  因此「把某部门的资产停用」**不会**把折旧挪到别的成本中心 —— 这是有意的。
- **不支持**「闲置资产折旧改计入管理费用-闲置资产」：那需要分类级科目参数 + 凭证规则 + 期间口径改造，
  属 v2（若业务坚持，另开设计卡）。
- 停用**不改** `start_depreciation_date` / `useful_life_months` / `accumulated_depreciation` /
  `last_depreciation_period` —— 缓存列的唯一写入者仍是折旧过账与红冲（`asset_handler.rs` 文件头硬约束②）。

---

## 5. 联动接线

### 5.1 触发点定性（D1）

| 状态变更 | 触发点 | 目标态 | 接不接 |
|----------|--------|--------|--------|
| 建卡（`NULL → IN_USE`） | `asset_handler::asset_create`（写 `log_event`，不是流转） | `IN_USE` | **不接**（建行动作；导入 N 张卡不能扇出 N 条通知 —— 与 LNK-09 同口径） |
| **停用** | **`asset_status_change` 第 ⑦ 步** | `IDLE` | **接** |
| **启用** | **`asset_status_change` 第 ⑦ 步** | `IN_USE` | **接** |
| 处置 | `asset_bridge::execute` 第 ⑤ 步 | `DISPOSED` | 已接（LNK-09） |
| 作废处置 | `asset_dispose_cancel` 第 ⑤ 步 | `IN_USE` | 已接（LNK-09） |

排除清单不变：`spec::FIXED_ASSET_EXCLUDED_ACTIONS = [GENERATE_VOUCHER, REVERSE_VOUCHER]`。
停用/启用本来不落凭证，为什么还要排除？—— **一个实体一张排除清单**（`asset_linkage.rs` 的设计原则），
否则「停用会不会落凭证」就取决于规则怎么配；统一排除 = 资产域任何状态变更都不由通用引擎落账。

### 5.2 `run_in_tx` 签名扩展

```rust
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    asset_id: Uuid,
    asset_no: &str,
    status: &str,
    operator: Option<Uuid>,
    event_ref: Uuid,        // ← 新增：本次事件标识（= 本次写入的变动行 id，幂等键用它）
) -> std::result::Result<AssetStatusChange, ApiError>
```

`AssetStatusChange` 增加 `event_ref: Uuid` 字段；`ctx()` 里

```rust
draft.idempotency_key = Some(format!(
    "LNK:FIXED_ASSET:{}:{}:{}", self.status, self.asset_id, self.event_ref
));
```

三个调用点同步：`asset_bridge::execute`（④ 的 `insert_change` 返回值要**接住**，当前是丢弃的）、
`asset_dispose_cancel`（同上）、新增的 `asset_status_change`。

> **长度校验**：`sys_notice.idempotency_key` 是 `VARCHAR(128)`（`V0043__create_notice.up.sql`）。
> 本键最长 `16 + 8 + 1 + 36 + 1 + 36 = 98` 字符，安全；但要求照 LNK-10 的做法在单测里
> `assert!(key.len() <= 128)` 钉住，将来谁给状态名加长解释也不必再查一次列宽。

### 5.3 为什么必须改幂等键（顺带修 LNK-09 的遗留 bug）

现状：键 = `LNK:FIXED_ASSET:<status>:<asset_id>`，而 `uk_sys_notice_idem` 是
**`idempotency_key` 单列唯一**（`migrations/business/V0049__notice_indexes.up.sql`，
写入侧 `notice/model.rs` 用 `ON CONFLICT (idempotency_key) WHERE ... DO NOTHING`）。后果：

| 场景 | 现状 | 本卡要求 |
|------|------|----------|
| 停用 → 启用 → **再停用** | 第二次停用**不发通知**（撞第一次的键） | 发（真的又发生了一次） |
| 处置 → 作废处置 → **再处置** | 第二次处置**不发通知** —— LNK-09 遗留 bug | 发 |

LNK-09 的单元测试 `asset_linkage::tests::notice_idempotency_key_is_per_status_and_asset` 里
「同状态同卡片 → 同键」那条断言**正是这个 bug 的固化**。实现时必须改为事件维度：
「同一 `event_ref` → 同键（重放不重复发）/ 不同 `event_ref` → 不同键」。

> **同型先例**：并行在做的 LNK-10（折旧批次接线，`erp/depreciation_linkage.rs`）已经用
> `event_ref`（= 本次凭证 id）做同一件事，并带 `assert!(key.len() <= 128)`。本卡沿用同名同义字段，
> 不要造第二个概念（`change_id` / `event_id` / `tick` 都别用）。

### 5.4 `spec::TRIGGER_SPECS` 与 `tests_wiring` 的改动清单（精确到断言）

**(a) `flow/linkage/spec.rs` → `FIXED_ASSET.wiring` 整段替换为**（去掉「暂无触发源」免责句）：

```
已接线（三处状态推进）：① 处置 — erp/asset_bridge.rs::execute 第 ⑤ 步（处置凭证 + 状态机
IN_USE|IDLE → DISPOSED + 卡片回填 + 变动留痕之后、commit 之前）调 asset_linkage::run_in_tx →
通用引擎 run_excluding(spec::FIXED_ASSET_EXCLUDED_ACTIONS)（GENERATE_VOUCHER / REVERSE_VOUCHER
排除：处置凭证由 asset_bridge 专用路径出 1~3 张、红冲由 reverse_disposal_vouchers 做）；
② 作废处置 — erp/asset_handler.rs::asset_dispose_cancel 第 ⑤ 步（红冲 + DISPOSED → IN_USE +
恢复在役 + 留痕之后）调同一个 hook；③ 停用 / 启用 — erp/asset_handler.rs::asset_status_change
第 ⑦ 步（状态机 IN_USE ⇄ IDLE + CAS 回写 + 变动留痕之后）调同一个 hook。三处都在各自事务
commit 之后调 asset_linkage::run_deferred_after_commit → linkage::engine::run_deferred。
建卡（NULL → IN_USE）是建行动作、不接。通知幂等键含本次变动行 id
（LNK:FIXED_ASSET:<状态>:<卡片 id>:<变动行 id>），所以可逆边（停用/启用、处置/作废处置）
重复发生会各发一条。
```

**(b) `flow/linkage/tests_wiring.rs::wired_triggers_match_call_sites`**：

- 文件级计数 **本卡不 +1**（`asset_handler.rs` 只间接调 `asset_linkage::run_in_tx`，不在 `ENGINE_ENTRIES`
  直调集合里）。把说明文字补一句原因即可。
- ⚠️ **数字以开工时的仓库实况为准**：并行在做的 LNK-10 新增了直调文件 `erp/depreciation_linkage.rs`，
  它落地后 `assert_eq!(actual.len(), 13)` 会变成 **14**。开工先 `git log -3` + 跑一次该测试看实际值，
  **按当时的实际值写死，不要照抄本卡的 13/14**，也不要为了让数字对上而改动无关文件。
- **不要**给 `FIXED_ASSET.proofs` 加 `asset_handler.rs` —— 反向守卫会因「登记了但没直调」当场红。

**(c) `tests_wiring.rs::asset_wires_generic_engine_and_excludes_voucher`**：

| 断言 | 现值 | 改为 |
|------|------|------|
| `handler.matches("asset_linkage::run_in_tx(").count()` | 1 | **2** |
| `handler.matches("asset_linkage::run_deferred_after_commit(").count()` | 1 | **2** |
| 新增 | — | `handler.contains("pub async fn asset_status_change(")`、`handler.contains("asset_dto::STATUS_IDLE")`、`handler.contains("asset_dto::CHANGE_IDLE")`、`handler.contains("asset_dto::CHANGE_ENABLE")` |
| 新增（顺序守卫） | — | 从 `handler.find("pub async fn asset_status_change(")` 切片，断言 `run_in_tx < tx.commit() < run_deferred_after_commit` |

> ⚠️ **字符串计数的坑**：`asset_status_change` 函数体及注释里**不许出现** `asset_linkage::run_in_tx(`
> 的额外字面量（注释里写一遍就 +1，count 断言假红）。要提就在文件头/文档里提，别在函数注释里带括号。

**(d) `docs/architecture/P1-FIX-5C-联动触发点接线清单.md`**：总表 `FIXED_ASSET` 行的
「停用/启用无入口」备注改为「三处已接（处置 / 作废处置 / 停用启用）」，§3.2 补一段指向本文件。

### 5.5 通知文案（不改 `AssetStatusChange::ctx` 的取数口径）

标题/正文由 `spec::status_label_of(ENTITY_FIXED_ASSET, status)` 生成 ⇒ 停用 = 「资产闲置」、
启用 = 「资产在役」。**不给"停用"单独造一个标题**：`IN_USE` 既可能来自「启用」也可能来自
「作废处置」，两者在状态语义上确实是同一个变化，配置页的规则也按状态挂 —— 想让二者文案不同，
就得给 `run_in_tx` 加第 7 个参数，收益不值。

---

## 6. 前端交互（`views/business/erp/asset/index.vue`）

### 6.1 操作列按钮

| 当前状态 | 按钮 | 条件 | 权限 | `data-testid` |
|----------|------|------|------|---------------|
| `IN_USE` | 停用 | `canIdleAsset(row)` | `v-privilege="[PERM.update]"` | `asset-btn-idle-<asset_no>` |
| `IDLE` | 启用 | `canEnableAsset(row)` | `v-privilege="[PERM.update]"` | `asset-btn-enable-<asset_no>` |
| `DISPOSED` | 无 | — | — | （已处置只能作废处置） |

位置：放在「换人」之后、「处置」之前（状态维护 → 处置，按动作破坏性递增）。

### 6.2 弹窗（一个弹窗两种语义，复用「作废处置」的形态）

- 标题：`停用资产` / `启用资产`；
- `t-alert`：
  - 停用 →「停用后资产变为「闲置」：**仍按月计提折旧（口径不变）**，仍可转移 / 处置；恢复使用点「启用」。」
  - 启用 →「启用后资产回到「在役」，继续按月计提折旧。」
- 字段：`原因`（`t-textarea`，必填，maxlength 255，`data-testid="asset-status-reason"`）、
  `变动日期`（`t-date-picker`，可空=今天，`data-testid="asset-status-date"`，提示「仅留痕，不参与折旧计算」）；
- 确认按钮：停用 `theme: 'warning'`、启用 `theme: 'primary'`；
- 成功提示：`已停用：资产转为「闲置」（折旧照提）` / `已启用：资产恢复「在役」`；
- 失败：复用 `errorText(error)`（会自动补 66046 的「怎么办」）。

### 6.3 `asset-logic.ts`（纯逻辑，必须可被 `tsx --test` 直接跑）

```ts
/** 是否可停用（只有在役的卡）。 */
export function canIdleAsset(asset: Pick<AssetRow, 'status'>): boolean {
  return asset.status === 'IN_USE';
}
/** 是否可启用（只有闲置的卡）。 */
export function canEnableAsset(asset: Pick<AssetRow, 'status'>): boolean {
  return asset.status === 'IDLE';
}
// ASSET_ERROR_HINTS 增一行：
66046: '该资产当前状态不允许本次停用 / 启用：已处置的请先作废处置，重复点击不会重复通知（刷新即可）',
```

时间线：`changeSummary` 已按 `from_status/to_status` 渲染，`IDLE`/`ENABLE` 留痕（dept/user 四组为 `None`）
自动输出「在役 → 闲置」/「闲置 → 在役」，**不需要改 `changeSummary`**（这点已核过源码）。

### 6.4 前端单测（`frontend/tests/unit/fin2-asset.test.ts` 增补）

1. `canIdleAsset`：`IN_USE→true`、`IDLE/DISPOSED→false`；`canEnableAsset` 反之；
2. `ASSET_ERROR_HINTS[66046]` 命中且不为空；
3. `changeSummary` 对 `{from_status:'IN_USE', to_status:'IDLE', dept/user 全 null}` 输出含「在役 → 闲置」；
4. 源码断言（照既有 `readFileSync` 模式）：`api/business/erp/asset.ts` 含 `/asset/${id}/status`；
   `views/business/erp/asset/index.vue` 含 `asset-btn-idle-` 与 `asset-btn-enable-` 两个 testid。

---

## 7. 实现卡拆分（已建卡，见 §9 的 id）

### 卡 A（backend，`backend-developer`）

**动**：`asset_dto.rs`（`StatusChangeReq` + `validate_status_change` + 单测）、`asset_handler.rs`
（`asset_status_change` + 按 §3 顺序）、`asset_linkage.rs`（`run_in_tx` + `event_ref`、幂等键、单测改事件维度）、
`asset_bridge.rs` / `asset_dispose_cancel`（接住 `insert_change` 的返回值传下去）、
`shared/error_code.rs`（66046 + 稳定表）、`flow/linkage/spec.rs`（wiring 文案）、
`flow/linkage/tests_wiring.rs`（§5.4 的断言）、`asset_linkage_tests.rs`（真库用例）、
`docs/architecture/P1-FIX-5C-…清单.md`。

**开工前置（R6）**：`git status` + `git log -3`。工作树里若还有 LNK-10 的未提交改动
（`spec.rs` / `depreciation_model.rs` / 未跟踪的 `depreciation_linkage.rs`），**只 add 自己的文件**，
`spec.rs` 用最小文本替换，别整段重写；`tests_wiring.rs` 的文件级计数按当时实况写死。

**不改**：`scan_assets_for_period` / `in_depreciation_scope` / `LIVE_STATUSES` / `FIXED_ASSET_EDGES`
（D4 的结论就是"不改"）、不加迁移、不动权限种子。

**验收（硬性）**：

1. `cargo test --lib` 全绿（基线 3217，本卡预期 +8~+12，**数字以实跑为准**）+ `cargo fmt` / `clippy -D warnings` 本卡零告警；
2. **真库 6 条**（`asset_linkage_tests.rs` 风格，`purge` 幂等清理）：
   - `st02_idle_triggers_once`：配 `FIXED_ASSET/IDLE` 通知规则 → 停用 → `status=IDLE` + `biz_status_log` 1 行（`IN_USE→IDLE`）+ `biz_fixed_asset_change` 1 行（`change_type=IDLE`、dept/user 全 NULL、reason 落库）+ 通知 **1** 条；
   - `st02_enable_triggers_once`：再启用 → `status=IN_USE` + change 行 `change_type=ENABLE` + 通知 1 条；
   - `st02_repeat_idle_conflicts`：**重复停用 → 409 / code=66046**，通知计数**不变**，change 行**不变**，`biz_status_log` **不变**；
   - `st02_excluded_actions_not_run`：在 `IDLE` 上配 `GENERATE_VOUCHER` 规则 → 停用后**凭证数 0**（排除清单真的拦住了）；
   - `st02_reversible_edges_notify_twice`：停用 → 启用 → **再停用** = 通知 **3** 条（D5 的键改造生效）；
   - `st02_dispose_restore_dispose_notifies_twice`：处置 → 作废处置 → **再处置** = `DISPOSED` 通知 **2** 条（LNK-09 遗留 bug 回归）；
3. **变异红 2 组**：① 删掉 §3 第 ③ 步的同态判定 → `st02_repeat_idle_conflicts` 立红；② 幂等键去掉 `event_ref` → `st02_reversible_edges_notify_twice` 立红（left 2 / right 3）。还原后重跑全绿；
4. 真机 HTTP 一次：`POST …/status` → 200 + `status=IDLE`；再次 → 409/66046；`GET /asset/{id}` 的 `changes` 里有 `IDLE` 行；凭证表**零新增**；夹具清理干净、端口释放。

### 卡 B（frontend，`frontend-developer`，**依赖卡 A**）

**动**：`api/business/erp/asset.ts`（类型 + `changeAssetStatus`）、`views/business/erp/asset/asset-logic.ts`
（两个 `can*` + 66046 文案）、`views/business/erp/asset/index.vue`（2 个按钮 + 1 个弹窗 + 2 个 handler）、
`tests/unit/fin2-asset.test.ts`（§6.4 四条）。

**验收**：`pnpm typecheck` 零错、`pnpm test:unit` 全绿（含新增 4 处断言）、`pnpm lint` 干净；
手工/脚本核对：`IN_USE` 卡只见「停用」、`IDLE` 卡只见「启用」、`DISPOSED` 卡两个都不见；
停用成功后列表状态 Tag 变「闲置」、详情时间线出现「在役 → 闲置」。

> 卡 B 一旦在 `index.vue` 里写 `v-privilege="[PERM.update]"`，**不要**新增 `PERM.idle`（D3）。

---

## 8. 风险与未决（如实记录）

| # | 风险 | 处置 |
|---|------|------|
| R1 | **`asset_handler.rs` 是热点文件**（1200+ 行，处置/转移/换人都在里面，LNK-09 刚改过） | 新 handler 追加在 `asset_dispose_cancel` **之后**（避免打断既有切片断言）；实现卡开工前 `git log -3 -- asset_handler.rs` 确认无并发改动；本卡完成时在 metadata 打 `hotspot` 标记 |
| R2 | 错误码 66046 需同步前端文案，否则前端只显示后端 message | 卡 B 的 `ASSET_ERROR_HINTS` 增行（§6.3） |
| R3 | 幂等键格式变更会影响**历史通知**的去重语义 | 只影响**新**事件（历史行已入库，键不变即可）；`asset_linkage` 单测①的断言要一起改，否则实现者会以为旧断言是对的而回退 |
| R4 | 停用/启用**不走审批**是本期有意的简化 | 若财务要求内控升级：新增 `business:erp:asset:idle` 权限码 + `asset_idle` 审批流程 + `run_in_tx` 挪到审批回调（与处置同款），届时 D3 的成本结论随之失效 —— 这是**另一张设计卡** |
| R5 | 准则条文号未逐字核对 | §4.2 第 6 条只按口径引用；实现/评审阶段请财务复核条序，口径结论（IDLE 照提）不受影响 |
| **R6** | **与 LNK-10（折旧批次接线）并发**：本卡要改的 `flow/linkage/spec.rs` 与 `tests_wiring.rs` 正好是 LNK-10 在改的文件；设计定稿时工作树里已有 LNK-10 未提交的改动（`spec.rs` / `depreciation_model.rs` 已改、`erp/depreciation_linkage.rs` 未跟踪） | 卡 A 开工先 `git status` + `git log -3`：只 `git add` 自己的文件（**绝不 `git commit -a`**，会把别人的半成品卷进来）；`spec.rs` 只替换 `FIXED_ASSET.wiring` 那一段文本，不整段重写；`tests_wiring.rs` 的文件级计数按当时实况（13 或 14）写死，见 §5.4(b) |

---

## 9. 交付物

- 本文件：`docs/architecture/FIN-ASSET-ST02-资产停用启用接口设计.md`
- 实现卡：卡 A「【FIN-ASSET-ST02-BE】资产停用/启用接口 + 联动接线」→ `backend-developer`
- 实现卡：卡 B「【FIN-ASSET-ST02-FE】资产页停用/启用按钮 + 原因弹窗」→ `frontend-developer`（依赖卡 A）
