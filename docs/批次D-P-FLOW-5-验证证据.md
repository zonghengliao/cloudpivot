# P-FLOW-5 验证证据 · 流程模板库（看模板 / 启用停用 / 另存为）

> 卡面：`t_84fbfea1`（P-FLOW-5）。本文只记录**真跑出来的**结果与口径，不写计划。
> 跑的时间：2026-11-02（CST）。库：`cloudpivot_test`（真 PG + 真迁移），后端 `NIK_PROFILE=test`，前端 `pnpm dev`（:5173 代理 :8023）。

## 一、交付物清单

| 层 | 文件 | 作用 |
|---|---|---|
| 迁移 | `backend/migrations/business/V0193__flow_def_is_system.{up,down}.sql` | `biz_flow_def.is_system` 列 + 8 条预置流程按种子 UUID 打标 + `category` 归一为中文业务域 |
| 迁移 | `backend/migrations/business/V0194__flow_templates_menu.{up,down}.sql` | 菜单节点 `/flow/templates`（`business:flow:read`） |
| 后端 | `backend/src/business/flow/{dto,model,handler}.rs` | `GET /business/flow/def/templates`、`POST /def/{id}/duplicate`、`POST /def/{id}/enable` |
| 前端 | `frontend/src/views/business/flow/templates/index.vue` | 模板库页（分类 Tab / 卡片 / 只看流程图 / 另存为 / 启停） |
| 前端 | `frontend/src/api/business/flow-api.ts`、`frontend/src/router/index.ts` | 类型 + API 封装 + 路由 |
| 测试 | `backend/tests/pflow5.rs` | 12 条真库端到端（卡面要求 ≥8） |

### 1.1 为什么多了一个 `POST /def/{id}/enable`

卡面说「复用现有接口，补两个」，实测**不够**：`publish` 的语义是「把草稿校验成快照」，
它先 `latest_draft(def_id)`，而**被停用的流程没有草稿**（`current_version_id` 指向已发布版本），
于是模板库点「启用」稳定 409「没有可发布的草稿版本」——验收 ⑤「再启用 → 恢复走审批」直接跑不通。

实测证据（未加 `enable` 之前，真机点「启用」）：

```
POST /api/business/flow/def/20000000-...-0a0182/publish  status=409  duration=7ms
```

因此补了第三个接口：只做 `DISABLED → PUBLISHED` 的状态位拨回（不产生新版本、不动在跑实例、
不动 `is_default`）。`WHERE status='DISABLED' AND current_version_id IS NOT NULL` 是语义闸门，
草稿走不进来（否则等于绕过 12 条发布校验），已由单测 ⑩ 固定。

## 二、单测（12/12 通过）

```bash
PFLOW5_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test' \
  cargo test --test pflow5 -- --test-threads=1
```

```
running 12 tests
test duplicate_conflict_on_existing_code ... ok            # ⑤ 409 + 10003，不留半成品
test duplicate_creates_draft_copy_with_graph ... ok        # ④ DRAFT / is_system=false / is_default=false / 节点边完整
test duplicate_does_not_copy_instances ... ok              # 红线 4：副本 0 实例，原模板实例不变
test enable_refuses_draft_definitions ... ok               # 草稿不能靠「启用」绕过发布校验（409）
test enable_restores_default_after_offline ... ok          # ⑤ 停用→启用：默认回来、版本数不变、重复启用 409
test offline_default_leaves_biz_type_without_default ... ok# ⑥ 停用默认 → 该 biz_type 解析不到默认流程
test offline_non_default_keeps_other_default ... ok        # ⑦ 停用非默认 → 默认不受影响
test read_only_graph_endpoint_returns_nodes_edges ... ok   # ⑧ version_detail 返回完整节点+连线
test templates_grouped_by_category ... ok                  # ① 按 category 分组、顺序固定、未知分类不丢
test templates_marks_system_defs ... ok                    # ② is_system 一 true 一 false
test templates_summary_from_condition_labels ... ok        # 摘要取分流标签，网关中转边不进摘要
test templates_usage_count_from_instances ... ok           # ③ usage_count = biz_flow_instance 条数
test result: ok. 12 passed; 0 failed
```

### 2.1 单测里踩到的两个真问题（已修）

1. **夹具自杀**：`Fixture::new()` 之后立刻 `cleanup()` 会把**刚建好的发起人**一起删掉，
   后续 `create_instance` 取发起人快照必然 `401/20005`（首轮实测 2 条用例因此失败）。
   拆成 `reset()`（只清数据）+ `cleanup()`（清数据 + 删夹具用户），首尾各用一次。
2. **摘要出现自相矛盾的阈值**：首轮真机卡片显示
   `金额 < 10000 / 10000 ≤ 金额 < 100000 / 金额 ≥ 10000 / 金额 ≥ 100000` ——
   网关 A 的兜底边标签「金额 ≥ 10000」指向的是网关 B，不是一档业务分层。
   修法：摘要只取**目标节点不是 CONDITION** 的条件边（判据用目标节点类型，而不是 `is_default`：
   网关 B 的兜底边「金额 ≥ 100000」指向会签节点，是有效的一档）。修完真机为
   `金额 < 10000 / 10000 ≤ 金额 < 100000 / 金额 ≥ 100000`。

## 三、全量回归

```bash
cargo test --lib                       # 1892 passed; 0 failed（含 authz 的「每个写路由都要求权限」）
cargo clippy -- -D warnings            # 干净（lib + bin）
pnpm typecheck                         # vue-tsc --noEmit 无错
pnpm build                             # 产出 templates-BbHKDnJT.js（12.98 kB gzip 4.79 kB）
```

> `cargo clippy --all-targets` 在 **HEAD 上本来就有** 存量告警（`flow/internal_control.rs` 的 test 模块
> `useless vec` / 常量运算、`tests/testd/*`），与本次改动无关，未在卡内顺手改。

## 四、真机验收（headless Chrome + CDP，登录真账号）

验收账号为一次性 SUPER_ADMIN（口令哈希照抄 `admin`，验收后已删除）。

### ① 打开模板库：8 个标准流程分 4 组

```
[total] 共 8 个模板
[groups] 采购管理: [po_approve/启用中/标准]
         仓库管理: [stock_in_cancel/启用中/标准, stock_in_post/启用中/标准, stock_transfer/启用中/标准]
         财务管理: [payable_cancel/启用中/标准, payable_pay/启用中/标准, voucher_reverse/启用中/标准]
         任务:     [task_accept/启用中/标准]
[tabs] 全部 | 采购管理（1）| 仓库管理（3）| 财务管理（3）| 任务（1）
[tab_filter_财务] ['财务管理']      # 点 Tab 只剩该组
```

> 卡面写「6 个标准流程分 3 组」是 P-FLOW-2/3A 时的口径；V0182/V0191 之后种子流程已是 **8 条 / 4 组**
> （多出 `stock_in_cancel`、`payable_cancel`、`voucher_reverse`）。页面按库里的真实分组渲染，没有写死。

### ② 查看流程图（只读，复用 P12 的 FlowCanvas）

```
[graph_drawer] header = 采购订单审批（标准·金额分级） · 流程图
               meta   = v1 · 7 节点 / 8 连线 · 已发布（只读）
               shapes = 24（svg 图形元素）
               labels = 金额 < 10000 / 其他情况 / 10000 ≤ 金额 < 100000 / … / ▶开始入口 / ◉部门主管审批 / … / ■结束出口
```

渲染走的是 `views/business/flow/components/FlowPreview.vue`（= `FlowCanvas` 的 readonly 包装，
与「发起审批」页同一个组件），**没有重画第二套画布**。

真机还修了一个 UX 硬伤：TDesign 的 drawer 在 `header` 传纯文本时**不渲染右上角关闭图标**，
首轮实测「点 X」找不到元素、抽屉关不掉。现在抽屉底部有显式「关闭」按钮，实测
`close_button_present: True → drawer_closed_after_click: True`。

### ③ 另存为 → 生成副本并跳到编辑器

```
[dup_prefill]   {code: po_approve_custom, name: 采购订单审批（自定义）}     # 弹窗自带合法默认值
[dup_filled]    {code: po_approve_e2e125941, name: 我们的采购审批 125941}   # 用户改名后
[dup_navigated] /flow/def/01a19aa1-238e-.../edit?vid=01a19aa1-238f-...      # 复制成功直接进编辑器
[dup_edit_page] 流程编辑器 … 我们的采购审批 125941 / po_approve_e2e125941 / 草稿
[dup_card]      {status: 未启用（草稿）, system: false, actions: [查看流程图, 另存为, 启用]}
```

库里的事实（副本 vs 源模板）：

```
 flow_code            | status    | is_system | is_default | version_no | node_count | edge_count
 po_approve           | PUBLISHED | t         | t          | 1          | 7          | 8
 po_approve_e2e125941 | DRAFT     | f         | f          | 1          | 7          | 8
```

- 副本 `is_system = false` → 前端不标「标准」（客户的流程不冒充标准，O1）；
- 副本 `is_default = false` → 不抢默认（否则等于偷偷改客户在跑的规则）；
- 节点/连线 7/8 与源一致，**实例 0 条**（红线 4：只复制定义）。

### ④ 停用默认流程 → 该业务不走审批（并且有明确警告）

确认框原文（真机抓取）：

```
[warning] 这是该业务的默认流程：停用后，该业务的单据提交将直接生效、不走审批（历史审批单仍按原版本跑完）。
[text]    确认停用「采购订单审批（标准·金额分级）」？停用后不能再发起新的审批单，已在跑的审批单不受影响。
[card]    {status: 已停用, actions: [查看流程图, 另存为, 启用]}
```

停用窗口内的库事实（等价于提交单据时「解析默认流程」的 SQL 条件）：

```
select count(*) from biz_flow_def
 where biz_type='PO_APPROVE' and is_default and status='PUBLISHED' and deleted_at is null;
→ 0        -- 解析不到默认流程 ⇒ 提交直接生效、不走审批
```

### ⑤ 再启用 → 恢复走审批

```
[before_enable]       {status: 已停用}
[enable_dialog_text]  确认启用「采购订单审批（标准·金额分级）」？启用后该业务恢复走审批，用的是停用前那个已发布版本（不重新发布）。
[after_enable]        {status: 启用中, actions: [查看流程图, 另存为, 停用]}
```

```
select flow_code, status from biz_flow_def
 where biz_type='PO_APPROVE' and is_default and status='PUBLISHED' and deleted_at is null;
→ po_approve | PUBLISHED        -- 恢复走审批
```

草稿副本点「启用」走的是**发布**分支（`publish`），实测 `未启用（草稿） → 启用中`（图完整，
7 节点 8 连线通过 12 条发布校验）。

### 验收后清理（不留垃圾）

```
po_approve 恢复 PUBLISHED + 默认（原样归还）；
验收造的副本（po_approve_custom / po_approve_e2e*）连同节点/连线/版本一并删除；
一次性 SUPER_ADMIN 账号删除 → biz_flow_def 回到 8 条、sys_user 无 pflow5e2e。
```

## 五、口径与边界（写给下一位）

1. **`is_system` 按种子 UUID 打标，不按 `flow_code`**：种子插入条件是 `WHERE NOT EXISTS(flow_code=...)`，
   客户若先建了同编码流程，种子整条跳过 —— 那条是客户自己的，按编码打标会误挂「标准」。
2. **`view_version_id` = 发布版本 > 最新草稿**：客户另存为出来的副本只有草稿，不这样退，模板库里
   「查看流程图」会一片空白。
3. **模板库不分页**：一次拉全量（一条 SQL 出 `usage_count` / 节点数 / 可看图版本），避免 N+1；
   分组顺序固定 `采购管理 → 仓库管理 → 财务管理 → 任务 → 其它`（未知分类不丢，排最后）。
4. **启用/停用的权限都是 `business:flow:publish`，另存为是 `business:flow:create`**；前端 `v-privilege`
   只是隐藏按钮，服务端 `#[nik::permission]` 才是依据。
