# SO-23 衍生 · `so_approve` 快照可重发布 + V0281 种子补 `is_system`（t_1dadde81）

> 卡面：`【SO-23 衍生·BLOCKER】so_approve 快照不可重发布 + V0281 种子漏 is_system（H6 常红 / pflow1a 会红）`
> 交付物：迁移 `V0301__sales_order_flow_is_system_and_snapshot.{up,down}.sql`（**唯一产品改动**，产品代码 0 行）
> 结论：4 条验收全部真跑通过（pflow1a 12/12、干净库 BLOCKER 0、模板默认口径命中、迁移号全局唯一）。

---

## 1. 验收逐条（全部真跑，原始日志见 `raw/`）

| # | 卡面验收 | 结果 | 证据 |
|---|---|---|---|
| 1 | `cargo test --test pflow1a -- --test-threads=1` 全绿，且遍历到 `so_approve` | ✅ **12 passed / 0 failed**，日志里有 `[④] so_approve v1 → 重新发布 hash 不变（97fe9377…），nodes=7 edges=8` | `raw/pflow1a-full.txt` |
| 2 | 空库 + 全量迁移后 `erp_invariants.py` 的 H6 / H5 变绿，BLOCKER 0 | ✅ 审批组 **6/6，BLOCKER 0，WARN 0**；全量 **75/76，BLOCKER 0，WARN 1**（剩的 WARN 是 N2，卡面明示不属于本卡） | `raw/erp-invariants-group-approval.txt`、`raw/erp-invariants-full.txt` |
| 3 | 库里 `so_approve` `is_system = TRUE`，且「模板列表默认口径」能查到 | ✅ `GET /business/flow/def/templates`（默认 = `AND d.is_system`）`total=15`，`so_approve` 在列且 `is_system=True nodes=7 edges=8`；SQL 口径命中 = 1 | `raw/templates-api-probe.txt`、`raw/db-state.txt` |
| 4 | 迁移号唯一 + `applied_via` 只用 `executed`/`imported` | ✅ 本仓 `business/` 最大 V0297、bare(`origin/main`) 最大 V0300 ⇒ 取 **V0301**（跨仓唯一）；干净库 226 条迁移，`applied_via` 非 `executed/imported` = **0** | `raw/db-state.txt` |

**修前 / 修后对照**（同一份冻结迁移集，只差 V0301）：

| 观测点 | 不含 V0301（`cloudpivot_so23before`） | 含 V0301（`cloudpivot_so23fresh`） |
|---|---|---|
| 迁移数 | 225 | 226 |
| `so_approve.is_system` | `false` | `true` |
| 模板默认口径（`AND d.is_system`）命中 | **0**（前端模板库少一张卡） | **1** |
| v1 `graph_hash` | `da3c2eed…fa9a2`（按 key 字母序的边序） | `97fe9377…6e80`（canonical：边按 `(from,to,key)`） |
| `erp_invariants --group 审批` | **4/6，BLOCKER 1（H6）+ WARN 1（H5）** | **6/6，BLOCKER 0，WARN 0** |
| `pflow1a::publish_regression_keeps_graph_hash` | 红（`da3c2eed…` ≠ `97fe9377…`，SO-23 已录） | 绿 |

---

## 2. 两个缺陷与根因

### A. V0281 种子漏写 `is_system`（H6 BLOCKER / H5 WARN）

`V0281__sales_order_flow.up.sql:88` 的种子列清单止于 `owner_dept_id`，没有 `is_system`
⇒ 吃 `BOOLEAN NOT NULL DEFAULT FALSE`（列由 V0193 加）。之后没有补偿迁移。后果不是报错，
而是**静默少一张卡**：`flow_model::templates(TemplateScope::Standard)` 的 WHERE 是
`AND d.is_system`，`so_approve`（销售订单审批（标准·金额分级））直接不出现。

同错法第 5 次（V0209 / V0211 / V0228 / V0231 + V0281）。已应用的迁移存内容指纹
（nik-db `migrate::history::checksum` = 文件 sha256），改一个字下次启动就
`ChecksumMismatch` 拒绝启动，所以只能新开一条补偿（照 V0220 / V0233）。

### B. v1 的 `graph_snapshot` 边序不是 canonical 顺序（存量版本不可重放）

`FlowGraph::snapshot()` 的确定性口径是：`nodes` 按 key、`edges` 按 `(from,to,key)` 升序
（`backend/src/business/flow/graph.rs`）。V0281 种下的快照**边序按 key 字母序**
（`e_finance_mid` 打头）。实测：

* 快照内容与行数据**完全一致**（node 逐列相等、edge 集合相等，只有数组顺序不同）；
* 因此 `canonical_json` 的字节不同 ⇒ `graph_hash` 不同：
  `da3c2eed…`（存的那份） vs `97fe9377…`（按行重算的那份）；
* 于是「另存草稿 → 重新发布」算出的 hash 与存量版本对不上 ⇒
  `pflow1a::publish_regression_keeps_graph_hash` 红；`flow::maintain` 的每日自检
  也会一直报 `RowsEdited`（SO-08 / SO-13 / SO-14 / SO-22 证据里那条「常红」正是它）。

**两件事必须一起修**：只修 A 会把红挪到 pflow1a；只修 B 则 `so_approve` 不在
`is_system=TRUE` 的遍历集里，B 的问题暴露不出来。

---

## 3. 修法（`V0301`，唯一产品改动）

1. **打标**（幂等）：`UPDATE biz_flow_def SET is_system = TRUE ... WHERE flow_code = 'so_approve'
   AND id = '<V0281 种子 id>' AND NOT is_system`。
   带 id 守卫而不是只按 `flow_code`：若某客户库的 V0281 种子因角色 UUID 不匹配被整条跳过、
   又自建了同名 `flow_code`，只按 flow_code 会把别人的流程标成系统预置（越权且翻不回来）。
2. **快照按应用的 canonical 序列化重写**（幂等 + 守卫）：
   `UPDATE biz_flow_def_version SET graph_snapshot = <canonical 快照>, graph_hash = <重算值>
    WHERE id='…d0282' AND version_no=1 AND btrim(graph_hash) = 'da3c2eed…'`。
   * 快照文本**取自应用的序列化产物**（`FlowGraph::snapshot_json()` 在「另存草稿 → 重新发布」
     路径上的输出），不是手抄实测 hash —— 生成脚本 `gen_migration.py` 还会断言
     `sha256(canonical) == 97fe9377…`、节点逐列相等、边集合相等；
   * 守卫按老 hash 精确匹配：客户若已重新发布 / 手工改过图，**整段跳过**（不覆盖别人的配置，
     与 V0248 同口径）。实测见 `raw/migration-guard.txt`（hash=`abab…` 时只打标、不改图、不抛错）。
3. **四条迁移内自检（fail-closed）**：
   ① 老 hash 不许还留着；② 定义行必须是 `is_system=TRUE`；③ 本迁移写下的那份快照必须
   「可重放」（`schema_version=1`、nodes 按 key 升序、edges 按 `(from,to,key)` 升序、
   计数与 `node_count/edge_count` 一致）；④ 全库不许再有「名字含『标准』却 `is_system=FALSE`」
   （与 H5 同口径的库内兜底，V0220 先例）。
4. **不改的东西**：不改 V0281（指纹）、不改 `biz_flow_node` / `biz_flow_edge` 行（图内容本来就对）、
   不动 `status` / `current_version_id` / 版本号 —— 这不是流程改造，是把「已发布的那一份」
   按应用自己的口径重新序列化一次。

**号段**：本工作仓 `business/` 最大 V0297；`origin/main`（bare）在 DEPLOY-SYNC `t_c3087afe`
合仓时把发票号段顺延到 **V0298–V0300** ⇒ 取 **V0301**，两仓都不会撞号（动手前用
`git ls-tree -r origin/main backend/migrations/` 实测过）。

**回滚**：`V0301….down.sql` 精确逆操作（`is_system` 回 FALSE；快照/hash 还原成 V0281 字节，
带「仍是本迁移写的那份」守卫），实测双向可走见 `raw/migration-roundtrip.txt`。

---

## 4. 顺带被治好的：`p16_graph_snapshot_selfcheck_is_clean`

`flow::maintain::run_once()`（行 → 快照 → hash 三方对账）在 `so_approve` 上一直报
`RowsEdited`（SO-08 起每一轮的测试归因文档都记着这条）。V0301 把存量 v1 的快照/hash 与行数据
对齐后：

```
test business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean ... ok
test result: ok. 1 passed; 0 failed
```

见 `raw/p16-graph-selfcheck.txt`（库 `cloudpivot_p16probe`，`ERP_DB_URL` 指向它）。

---

## 5. 复现（全部真跑，环境：本机 PG `127.0.0.1:5432` 账号 `alun`，后端 `backend/target/debug/cloudpivot`）

> ⚠️ **本次没用 `scripts/verify/so23_fresh_db.sh`，原因要记下来（两个都是别人的坑）**：
>
> 1. **未提交迁移**：共享工作区 `/opt/cloudpivot` 里另一位 worker 当时正在写**未提交**的
>    `V0267__sales_menu`（SO-25 卡），且边写边改（sha256 在变）——直接把别人的在途迁移混进
>    基线，还会 `ChecksumMismatch` 拒绝启动。
> 2. **已提交但空库跑不通**：`V0267__sales_menu`（`f61cf8a`）号段 267 **低于**它依赖的
>    销售菜单页迁移（V0282/V0286/V0291/V0294/V0297/V0300），而它的自检 6.1 要求 17 个销售
>    权限码必须已有 `sys_menu` 承载行 ⇒ **空库上必然先跑 267、后建菜单行，整批迁移回滚、
>    实例起不来**（实测：`已入库 197 条，最后一条 V265`，随后
>    `V0267 自检 6.1 失败：以下销售权限码在 sys_menu 里没有承载 → …`）。证据见
>    `raw/v0267-fresh-blocker.txt`。**这不是本卡的缺陷**（本卡只加 V0301），但它会挡住任何人
>    的「空库 + 全量迁移」验收 —— 已在卡内 comment 上报，建议 SO-25 把该迁移顺延到 **≥ V0302**。
>
> 因此验收用「**冻结迁移集**」：把 `backend/migrations` 复制一份、剔掉未提交的迁移
> （只保留本卡 V0301）**和 V0267**，再用一个独立运行目录（`config/config.toml` 的
> `[database.migration].path` 改成冻结目录绝对路径）启动同一个二进制。
> 实测 `NIK_DATABASE_MIGRATION_PATH` 环境变量**不被识别**，必须改配置文件。
> 脚本：`fresh_db_frozen.sh`（基线）、`final_accept.sh`（全部验收）、`before_baseline.sh`（修前对照）；
> 全部落在本目录 `scripts/`（脚本里的 `WS` 指卡工作区 `/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81`，
> 换机器跑时改这一处即可；`gen_migration.py` 是迁移的生成器 —— 快照文本由它从应用的序列化产物写入，
> 并断言 hash / 节点 / 边集合一致）。

```bash
# ① 干净基线（226 条迁移 = 216 个 V*.up.sql + 10 个种子 S__；V0267 见上面的说明被剔除）
bash fresh_db_frozen.sh
# ② 全部验收（pflow1a / invariants / 模板接口 / p16 自检 / 迁移双向）→ raw/
bash final_accept.sh
# ③ 修前对照（同一冻结集去掉 V0301 → H6 BLOCKER + H5 WARN）
bash before_baseline.sh && bash collect_before.sh
# ④ 别人的坑：原始迁移集（含 V0267）在空库上跑不完的证据
bash v0267_evidence.sh
```

单条手跑（与卡面命令等价，只是库换成冻结基线）：

```bash
PFLOW_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_pflow1a' \
  cargo test --test pflow1a -- --test-threads=1 --nocapture
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_DATABASE_NAME=cloudpivot_so23fresh python3 scripts/verify/erp_invariants.py --group 审批
```

---

## 6. 证据清单（`raw/`）

| 文件 | 内容 |
|---|---|
| `db-state.txt` | 干净基线：226 条迁移、`applied_via` 全 `executed`、`so_approve` 打标 + v1 hash + 快照边序 + 模板口径命中 |
| `pflow1a-full.txt` | pflow1a 全套 `--nocapture`：12 passed，含 `[④] so_approve v1 → hash 不变` |
| `erp-invariants-group-approval.txt` | 审批组 6/6，BLOCKER 0 |
| `erp-invariants-full.txt` | 全量 75/76，BLOCKER 0，WARN 1（N2，卡面不属本卡） |
| `templates-api-probe.txt` | `GET /business/flow/def/templates` 默认口径 / `?scope=all` 两条都含 `so_approve` |
| `p16-graph-selfcheck.txt` | `flow::maintain` 三方对账 0 漂移（历史上因 `so_approve` 常红） |
| `migration-roundtrip.txt` | `down` → 回 V0281 字节；`up` → 回 canonical；再跑 `up` = 0 行变更 |
| `migration-guard.txt` | 客户改过图（hash=`abab…`）时只打标、不改图、不抛异常 |
| `h6-detail.txt` | H6 静态扫描逐条归因：`V0281 flow_code=so_approve → compensated_by=V0301` |
| `before-baseline.txt` | 修前对照：225 条迁移、`is_system=false`、模板口径命中 0、H6 BLOCKER + H5 WARN |
| `v0267-fresh-blocker.txt` | **不是本卡的坑**：`V0267__sales_menu`（SO-25）在空库上卡死全量迁移（197 条后 RAISE，真实报错 + 依赖的更高号段迁移名单） |

---

## 7. 遗留 / 边界

1. **N2（WARN）**：干净库 `sys_dept` 一行为空、`admin` 无部门 —— 卡面明示不属于本卡（要落组织树或让 N2 降级成 INFO）。
2. **生产库**：`V0301` 对「客户已重新发布 / 手工改过 `so_approve` 图」的库只打标、不改图（守卫实测见 `raw/migration-guard.txt`）；这类库的图仍不可重放，属客户改图后的正常状态，`flow::maintain` 会照常报漂移给人看，不会被本迁移掩盖。
3. **共享工作区 / 主干的空库基线被别人的迁移卡住**：`V0267__sales_menu`（SO-25，`f61cf8a`）
   号段早于它依赖的销售菜单页迁移，空库上 267 一跑就 RAISE，整批迁移回滚、实例起不来
   （`raw/v0267-fresh-blocker.txt`）。**任何人在 `/opt/cloudpivot` 直接起后端都会迁移失败**，
   与本卡无关，但会挡住全员「空库 + 全量迁移」验收 —— 建议尽快把该迁移顺延到 ≥ V0302
   （内容不用改）。本卡的验收因此在冻结迁移集上跑（剔除 267，其余与主干逐字节一致）。
