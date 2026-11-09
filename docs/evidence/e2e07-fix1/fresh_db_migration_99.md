# 全新库（纯迁移链）验证：V0359 在排序里不炸、且幂等

环境：99 本机 PG（`alun/alun`），库 `cloudpivot_e2e07fix1`，由
`RECREATE=1 ERP_DB=cloudpivot_e2e07fix1 bash scripts/verify/erp_clean_db.sh` 从零建库 +
**用真实二进制起一次实例**跑完全部迁移（这是「迁移链」而不是手工 psql 的证明）。

```
$ ls backend/migrations/*/V*.up.sql | wc -l          → 281
$ ls backend/migrations/*/S__*.sql  | wc -l          → 11

$ psql -d cloudpivot_e2e07fix1 -Atc \
    "select kind, count(*), count(*) filter (where success is false) from schema_migrations group by kind"
versioned|281|0        ← 仓里 281 个版本化迁移，全部应用成功、0 失败
seed|11|0

$ psql -d cloudpivot_e2e07fix1 -Atc \
    "select count(*) from schema_migrations where version = '359'"
1                      ← V0359 已进表（version 列存整数，所以不是 '0359'/'V0359'）

$ psql -d cloudpivot_e2e07fix1 -Atc "
    select count(*) from biz_flow_def d
      join biz_flow_def_version v on v.id = d.current_version_id
      join biz_flow_node n on n.version_id = v.id
     where d.status='PUBLISHED' and n.node_type='APPROVAL'
       and n.assignee_type='ROLE' and n.approval_mode='ALL'"
0                      ← 全新库里本来就没有该错配（见下）
```

## 为什么全新库里 V0359 是「空转」，而 145 上有 7 个节点要改

迁移种下的流程节点 `approval_mode` 全是 `NULL`。**已发布版本**的冻结发生在
`bootstrap` 的 `flow::sign_freeze`（P3-06）：候选人数 ≥2 → `ALL`，<2 → `ANY`。
全新库里角色还没有成员（种子只建角色，不建人）⇒ 候选人数 = 0 ⇒ 一律 `ANY`，
于是 V0359 找不到 `ALL` 节点，按设计打印 NOTICE 跳过（**幂等、不报错**）。

145 / 144 这类「角色早就有人在用」的库，才是在冻结那一步把「采购经理」冻成 `ALL` 的现场
（145 实测 7 个，见 `all_nodes_before_after_145.txt`）。这正是本卡要修的历史包袱。

## 顺带：不变量脚本在「空库」上的行为（一条重要的边界）

```bash
$ bash scripts/verify/flow_countersign_invariants.sh cloudpivot_e2e07fix1
① ALL 节点无兜底:         0  ✅
② 角色型审批误配 ALL:    0  ✅
③ ALL 节点角色坏号:       0  ✅
④ ALL 节点候选人为 0:     6  ❌  （只能靠 empty_assignee 兜：FALLBACK_ADMIN=超管会签、SKIP=静默跳过）
    ↳ budget_approve / n_fin_gm / empty_assignee=FALLBACK_ADMIN
      …（6 个 COUNTERSIGN 节点全部命中）
结果: 1 条不变量不通过 ❌（④ALL候选人为0）
rc=1

$ FLOW_CS_ALLOW_EMPTY=1 bash scripts/verify/flow_countersign_invariants.sh cloudpivot_e2e07fix1
结果: 4 条不变量全绿 ✅，但**有降级警告**：④候选人为0(6) —— 降级 ≠ 通过
rc=0
```

**为什么空库 ④ 会红**：全新库里角色一个成员都没有，6 个跨角色会签节点解析出的候选人是 0，
只能靠 `empty_assignee=FALLBACK_ADMIN` 退回超管 —— 而「超管全员会签」正是本卡要消灭的形态
（多一个超管夹具账号就重演死锁）。所以脚本默认判红，空库/装机初期用
`FLOW_CS_ALLOW_EMPTY=1` 显式降级（结论会写「有降级警告」，不会假装全绿）。

**结论**：
- V0359 放在迁移链里不破坏**全新安装**（281+11 条全绿、0 失败）；
- 它在**存量库**上按目标清单逐条打补丁（145 实测 7 个节点、5 个流程发新版）；
- 两端都幂等：重放 = 0 变更（145 的 down→up→再 up 往返见 `migration_roundtrip_145.txt`）。

