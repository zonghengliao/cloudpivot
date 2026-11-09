# V0359-FIX 证据包：会签改签迁移支持多版本流程（144 部署失败根因）

> 卡：`t_626aaa8a`（P0 生产隐患） · 环境：145 测试机（`cloudpivot_dev` / 三个 preview 库）
> 生产快照：`/root/db-backup/cloudpivot-20261109-221715.dump`（144，295 条迁移，1.5 MB）
> 结论：**V0367 已在 144 备份副本上跑通真实迁移器**，17 个错配节点全部改为或签，`po_approve` 走 v5、不撞 DRAFT v3/v4。

---

## 一、根因（不是「V0359 逻辑错」，是「单版本假设」）

`V0359__flow_all_role_approval_to_any.up.sql` 用**当前版本号 + 1** 发新版：

```sql
SELECT d.id, v.id, v.version_no, ... INTO c_def, c_v1, c_v1_no, ...
  FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id
 WHERE d.flow_code = t.flow_code;
...
INSERT INTO biz_flow_def_version (id, def_id, version_no, ...)
VALUES (c_v2, c_def, c_v1_no + 1, 'PUBLISHED', TRUE, ...);   -- ← 假设「当前 = 最高」
```

生产（144）`po_approve` 的实际版本分布是 **多版本 + 草稿**：

| flow_code | 版本 | 状态 | is_current | def.current_version_no |
|---|---|---|---|---|
| contract_approve | v1 | PUBLISHED | t | 1 |
| payable_pay | v1 ARCHIVED / v2 PUBLISHED | | v2 | 2 |
| **po_approve** | **v1 ARCHIVED / v2 PUBLISHED / v3 DRAFT / v4 DRAFT** | | **v2** | **2** |

⇒ V0359 对 `po_approve` 想发 **v3**，而 `v3` 已被一个草稿占住：

```
ERROR:  duplicate key value violates unique constraint "uk_biz_flow_def_version"
DETAIL:  Key (def_id, version_no)=(20000000-0000-4000-8000-0000000a0182, 3) already exists.
```

单事务内前两个流程（`contract_approve v2`、`payable_pay v3`）已经发版成功，撞库后**整体回滚** ⇒
迁移器报 `数据库迁移失败` ⇒ `FATAL: 服务启动失败` ⇒ 144 起不来。
应急处理把 V0359 手工记为已应用（`source='manual-skip-deploy-incident'`），但**它从未真跑**，
所以生产上「岗位审批仍是全员会签」的隐患仍在（当前 PUBLISHED 版本里仍有 **17** 个 `APPROVAL+ROLE+ALL` 节点）。

复现日志：`repro_v0359_violation.txt`（在 144 备份副本上原样重放，`bad_nodes_after_rollback=17`）。

---

## 二、修法：`V0367__flow_all_role_approval_to_any_multiversion.up.sql`

**不改 V0359**（145 / 144 都有它的 `schema_migrations` 记录，改一个字节 → 校验和失败 → 下次升级直接起不来）。
新增 V0367，口径与 V0359 完全一致（只把 `APPROVAL + ROLE + ALL` 改成 `ANY`），只改实现：

| 维度 | V0359（原） | V0367（修正） |
|---|---|---|
| 新版本号 | `c_v1_no + 1`（当前版本 + 1） | **`COALESCE(MAX(version_no), 0) + 1`**（该流程最大版本 + 1，含 DRAFT） |
| 目标清单 | 硬编码 7 个 `(flow_code, node_key)` | **按形状动态扫描**：`is_current + PUBLISHED` 且仍有 `APPROVAL+ROLE+ALL` 节点 |
| 幂等 | 目标节点不是 ALL 就跳过（按 flow_code 逐个判） | 处理完的流程新版里已无错配节点 ⇒ **第二次执行目标清单为空（0 个流程）** |
| 节点行复制 | 漏 `fallback_user_id / fallback_role_id / subprocess_def_id` | **补齐这 3 列**（否则新版本的行与快照 JSON 不同构） |
| `COUNTERSIGN` | 不动 | 不动（`node_type = 'APPROVAL'` 是判据，6 个跨角色会签节点原样保留） |
| 自检 | 节点/边/绑定/hash/is_current | 同左 + 版本号占用、定义指向、行数=补丁数（SQL 里 `GET DIAGNOSTICS` 对账） |

动态扫描把 V0359 漏掉的流程一并覆盖（V0359 起草时只扫了 145 dev 的 8 个流程）：

```
asset_disposal n_gm · budget_approve n_gm · contract_approve n_gm,n_pm,n_pm_hi ·
payable_pay n_dept · po_approve n_dept,n_pm · pr_approve n_pm · prt_approve n_pm ·
so_approve n_gm · stock_in_cancel n_review · stock_in_post n_wh ·
stock_take_post n_fin,n_gm,n_wh · stock_transfer n_approve          = 12 流程 / 17 节点
```

---

## 三、验证矩阵（四条路径，全部实跑）

| # | 场景 | 库 | 结果 |
|---|---|---|---|
| ① | **复现**：144 备份 + V0359 | `cloudpivot_preview3` | `duplicate key ... uk_biz_flow_def_version`，整体回滚，`bad_nodes_after_rollback=17` |
| ② | **多版本修复**（psql 单事务） | `cloudpivot_preview` | 12 流程 / 17 节点修复；`po_approve` **v2 → v5**（跳过 DRAFT v3/v4）；收口自检 `APPROVAL+ROLE+ALL=0` |
| ③ | **真实迁移器 + 全量升级**（144 备份副本） | `cloudpivot_preview2` | 启动期 `应用 5 个迁移`、`health=200`；无 unique violation；再启动 `应用 0 个迁移` |
| ④ | **幂等**（145 正常库） | `cloudpivot_dev` | `V0367 完成：0 个流程需要处理` |
| 附 | **反证**：先删手工跳过记录再部署 | `cloudpivot_preview3` | `FATAL: 服务启动失败 ... uk_biz_flow_def_version` ⇒ 证明 ⑦ 不能删记录（见 §五） |
| 附 | **down / up 往返** | `cloudpivot_preview` | down：删 12 个版本、`po_approve` 复位到 **v2 PUBLISHED**（不是 DRAFT）、错配节点回到 17；up：重新发 v5、收口 0（`down_up_roundtrip.txt`） |

### ③ 真实迁移器日志（`preview2_runner_run1_apply.txt`）

```
2026-11-09T14:51:42.957191Z NOTICE: V0367 asset_disposal：v2 已发布（改 1 个节点 [n_gm] / hash=a852eff1… / 节点 3 / 边 2 / 绑定 1 行）
2026-11-09T14:51:42.981125Z NOTICE: V0367 po_approve：v5 已发布（改 2 个节点 [n_dept,n_pm] / hash=27fab60c… / 节点 9 / 边 11 / 绑定 5 行）
2026-11-09T14:51:43.004566Z NOTICE: V0367 完成：共处理 12 个流程 / 17 个「角色型审批被配成全员会签」节点
2026-11-09T14:51:43.005072Z NOTICE: V0367 收口自检通过：PUBLISHED 当前版本里 APPROVAL+ROLE+ALL = 0
2026-11-09T14:51:43.011681Z  INFO nik_web::app: 数据库迁移完成: 应用 5 个迁移
```

第 2 次启动（`preview2_runner_run2_idempotent.txt`）：`数据库迁移完成: 应用 0 个迁移`。

### 修复前后（`po_approve`，`verify_preview2_runner_path.out`）

| node_key | node_type | v2（旧 PUBLISHED） | v5（新 PUBLISHED） |
|---|---|---|---|
| n_dept | APPROVAL / ROLE | **ALL** | **ANY** |
| n_pm | APPROVAL / ROLE | **ALL** | **ANY** |
| n_fin | APPROVAL / ROLE | ANY（本来就对） | ANY（未动） |
| n_board | COUNTERSIGN / ROLE | ALL | **ALL（会签，刻意不动）** |

* `is_current` 每流程恰好 1 行：`flows_with_wrong_current = 0`
* DRAFT 版本未被改动：`po_approve v3/v4`、`stock_take_post v2` 的 `approval_mode` 仍是 `ALL`（草稿不是运行态）
* 图完整性：18 个当前版本的 `node_count/edge_count` 与实际行数**全部一致**（见 ⑧）
* hash 自洽：39 个版本 `mismatched = 0`（4 个 DRAFT 无快照，符合 `ck_biz_flow_def_version_published`）
* 两条路径（psql 单事务 / 真实迁移器）产出的验证输出**逐字节相同**（`diff` 无差异）

---

## 四、文件清单

| 文件 | 内容 |
|---|---|
| `repro_v0359_violation.txt` | ① 复现：144 备份副本上 V0359 撞唯一约束 + 回滚后 17 个错配节点还在 |
| `preview2_runner_run1_apply.txt` | ③ 真实迁移器首启（V0365/V0366/V0367 + 2 个可重复种子） |
| `preview2_runner_run2_idempotent.txt` | ③ 二次启动 `应用 0 个迁移` |
| `dev145_psql_idempotent.txt` | ④ 145 正常库跑 V0367 = 0 个流程 |
| `verify_preview_psql_path.out` / `verify_preview2_runner_path.out` | ②③ 的 9 组验证 SQL 输出（两份完全相同） |
| `down_up_roundtrip.txt` | 附：down（删 12 版、复位到 v2）→ 再 up（重发 v5）的往返验证 |
| `sql/q_targets.sql` | 动态目标清单 + `COUNTERSIGN` 分组统计（实施前扫描） |
| `sql/q_verify.sql` | 验证 ①–⑨（版本列表 / is_current 唯一 / 前后对比 / 图完整性 / hash 自洽） |
| `scripts/v0367_repro.sh` | 复现脚本（恢复 144 备份 → 跑 V0359） |
| `scripts/v0367_accept_runner.sh` | 真实迁移器验收脚本（`--role=cloudpivot` 恢复 → 启动两次） |

---

## 五、144 生产重跑迁移的操作步骤（**重要纠正**）

卡面 ⑦ 原写「删掉手工跳过的 `schema_migrations` 记录，再重新部署」。**这一步是错的，会导致再次起不来** ——
已用 `v0367_drop_record` 实跑证明：删掉记录后迁移器会重跑 V0359（359 < 367，先于 V0367 执行）⇒
`FATAL: 服务启动失败: 配置错误: 数据库迁移失败: unique constraint violated (uk_biz_flow_def_version)`。

**正确步骤（推荐线：先备份 → 灰度验证 → 上线）**

```bash
# 0) 备份（也可用 scripts/ops/backup_db.sh）
ssh root@172.18.10.144 'pg_dump -Fc -d cloudpivot -f /root/db-backup/cloudpivot-$(date +%Y%m%d-%H%M%S).dump'

# 1) 把最新备份恢复到一台**非生产**库（145），先跑一遍新代码的迁移（本卡已验证的方法）
#    若这一步不绿，不要动 144。
scp root@172.18.10.144:/root/db-backup/cloudpivot-<最新>.dump root@172.18.10.145:/tmp/
ssh root@172.18.10.145 'su - postgres -c "dropdb --if-exists cloudpivot_preview; createdb -O cloudpivot cloudpivot_preview"'
ssh root@172.18.10.145 'su - postgres -c "pg_restore --role=cloudpivot -d cloudpivot_preview /tmp/cloudpivot-<最新>.dump"'
#    ⚠️ 恢复必须带 --role=cloudpivot（或事后 REASSIGN OWNED）：用 --no-owner 以 postgres 恢复会让
#       所有表属主变成 postgres，应用账号 cloudpivot 没权限，报出来的却是空消息
#       `数据库迁移失败: query failed: : db error`（本卡踩过，见 §六）。
#    然后在该库上启动新二进制（CWD 指向含新迁移的 backend/），确认 应用 N 个迁移 + health 200。

# 2) 上线：拉代码 + 构建 + 重启。**保持那条手工跳过记录不动** —— 它是阻止 V0359 重跑的唯一开关。
ssh root@172.18.10.144 'cd /opt/cloudpivot && git pull && bash scripts/deploy/deploy.sh'
#    迁移器会按版本序执行 pending 的 V0365 / V0366 / V0367；
#    V0359 因已有记录被跳过（迁移器还会顺手给它补上 checksum，属正常行为）。

# 3) 验收（生产）
ssh root@172.18.10.144 "psql -d cloudpivot -tAc \"
  select version, name, success from schema_migrations where version in ('359','365','366','367') order by version;
  select 'bad=' || count(*) from biz_flow_def d
    join biz_flow_def_version v on v.def_id=d.id and v.is_current and v.version_status='PUBLISHED'
    join biz_flow_node n on n.version_id=v.id
   where d.deleted_at is null and n.node_type='APPROVAL' and n.assignee_type='ROLE' and n.approval_mode='ALL';
  select flow_code, version_no, version_status from biz_flow_def d
    join biz_flow_def_version v on v.def_id=d.id where d.flow_code='po_approve' order by 2;\""
#    期望：359/365/366/367 四条 success=t；bad=0；po_approve 出现 v5 PUBLISHED（v3/v4 仍是 DRAFT）

# 4)（可选，审计用）把手工跳过记录的 source 标注清楚，证明「本应由 V0367 补齐」
#    UPDATE schema_migrations SET source = 'manual-skip-deploy-incident;superseded-by-V0367'
#     WHERE version='359' AND source='manual-skip-deploy-incident';
```

**如果 144 上 V0367 意外失败（预演里不会发生）**：因为每条迁移各自一个事务，V0367 失败只回滚它自己，
`V0365/V0366` 会留在已应用状态，服务仍起不来 —— 此时保留备份、把日志里的 NOTICE 贴给后端，
**不要**改 `migrations/` 里的旧文件来「绕过」，也不要手工插 `schema_migrations` 的 V0367 行（那会让隐患继续潜伏）。

---

## 六、踩坑（写给下一次做备份演练的人）

1. **`pg_restore --no-owner` 以 postgres 身份恢复 = 应用账号没权限**。现象是迁移器在
   `SELECT ... FROM schema_migrations` 之后立刻失败，而错误只有 `query failed: : db error`（消息为空），
   极易误判成「迁移 SQL 有 bug」。用 `pg_restore --role=cloudpivot`（或 `REASSIGN OWNED BY postgres TO cloudpivot`）。
2. **`createdb -T cloudpivot_dev` 会因并发连接失败**（145 dev 常有别的 agent 连着）。用 dump/restore 代替模板克隆。
3. **V0359 这类「按流程硬编码发版本号」的迁移，在多版本/带草稿的库里必然翻车**。
   任何「归档旧版 → 发新版」的迁移都必须 `MAX(version_no)+1`，并且用形状（而不是 flow_code 清单）筛目标。
4. **手工 `INSERT INTO schema_migrations` 应急是「止疼药」不是「解药」**：
   它让服务起来，但业务隐患（全员会签 → 停用一个人就卡死）还在，必须补一个真正幂等的迁移来收口。

---

## 七、验证边界（诚实登记）

| 项 | 结果 |
|---|---|
| `cargo test --lib` | **4017 passed / 0 failed**（`EXIT=0`） |
| `cargo test --test flow_all_role_multiversion_migration` | **7 passed**（本卡新增的静态不变量） |
| 全量 `cargo test` | **跑不了**：编译 `tests/linkage` 时 `error[E0061]` —— 共享工作区里另一位 worker 的在途改动给 `flow_model_inbox::page_my_tasks` 加了第 8 个参数（`completed_since`），而 `tests/linkage/common.rs:750` 仍按 7 个参调用。`git show origin/main:backend/src/business/flow/flow_model_inbox.rs` 仍是 7 参 ⇒ **提交树自洽**，这是工作区脏导致的编译期失败，与 V0367（只新增迁移 + 新测试文件）无关；对方提交时会同步改调用点。 |

数据前提（144 生产备份副本实测，决定了「归档旧版」不会搁浅在途单）：

* 18 个流程全部 `status=PUBLISHED` / `is_system=t`；
* **当前版本上 0 张在途审批单**（`biz_flow_instance.version_id = current_version_id` 计数全 0）；
* 所有当前版本 `graph_snapshot.nodes` 数量 == `biz_flow_node` 行数（18/18 一致，见 verify ⑧）。
* 迁移器记录的 `checksum` 就是迁移文件的 sha256：`V0367` 在 preview2 里记录为
  `5480dec73130bdad…`，与 `sha256sum backend/migrations/business/V0367__…up.sql` **完全相同**
  ⇒ 预演跑的就是已提交的那份文件。
