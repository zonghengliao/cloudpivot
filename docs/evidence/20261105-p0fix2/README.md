# 【P0-FIX-2】数据权限空转 —— 机制、授权、真机证据

> 卡片：`t_577713e9`｜标的库：生产 `cloudpivot`（144）+ 隔离库 `cloudpivot_p0fix2`（99）
> 结论：`sys_role_dept` 从 **0 行 → 3 行**，5 个业务角色拿到部门级范围；行级数据权限
> 从「空转」变成「可执行且已验证」；新增巡检 N1（WARN）把这件事钉进回归。

---

## 1. 实现机制（代码位置 + scope 类型）

### 1.1 两个载体

| 载体 | 语义 |
|---|---|
| `sys_role.data_scope`（V0060） | `1` 全部 / `2` 本部门 / `3` 本部门及以下 / `4` 仅本人 / `5` 自定义部门集合 |
| `sys_role_dept`（V0061） | `(role_id, dept_id)`，**仅 `data_scope=5` 生效**，一行一个部门 |

### 1.2 一次请求的链路

```
AuthClaims.sub
  └─ business/shared/principal.rs::resolve(user_id)          ← 缓存 300 s（perm:user_scope:{epoch}:{uid}）
       ├─ RESOLVE_SQL（principal.rs:289-328）                 ← 一次往返：4 表 join + 递归 CTE
       │    ├ my_roles  : 有效角色（软删/停用/过期绑定都不算）
       │    ├ subtree    : me.dept_id 的子树（WITH RECURSIVE，应用层预解析，SQL 里不内嵌）
       │    └ UNION ALL  : ME / SUPER / ALL / DEPT / DEPT_CHILD / CUSTOM(← sys_role_dept) / SELF / DEPT_ROLE
       ├─ merge_scope_rows（principal.rs:396-471）             ← 多角色取并集；部门类解析不出部门 → 降级 SELF（S10/S11）
       └ ScopedPrincipal{ is_super, kind, dept_ids, self_only }
            └─ shared/scope.rs::filter_for（:436-527）
                 ├ 超管 / ALL      → `AND 1 = 1`
                 ├ Dept            → `AND {a}.{dept} = ?`
                 ├ DeptAndChild/Custom → `AND {a}.{dept} = ANY(?::text::uuid[])`
                 ├ SelfOnly        → `AND {a}.{owner} = ?`
                 └ 部门 ∪ 本人     → `AND ({a}.{dept} = ANY(?) OR {a}.{owner} = ?)`
                      └─ scope::paginate（唯一分页入口）/ filter_for（单条、导出、批量）
```

不再有第二条读路径：分页只有 `scope::paginate`（登记表）与 `paginate_unscoped`（显式豁免表）两个入口，
`paginate_unscoped` 对登记表会直接报错（`scope.rs:659-665`）。

### 1.3 病根

```
生产实测（修复前）：select count(*) from sys_role_dept;                 -- 0
                    select data_scope, count(*) from sys_role … ;       -- 1 | 12
```
`data_scope` 的列默认值是 1（`V0060:17`），而**没有任何迁移/种子给业务角色设过范围**，
`sys_role_dept` 也从未写过一行。于是全库每个人解析出来的都是 `kind=All`，
注入片段恒为 `AND 1 = 1` —— 功能没报错、界面一切正常，数据权限等于没做。

---

## 2. 哪些表受控 / 哪些不受控 / 为什么

### 2.1 受控（会注入数据范围）

**登记表 11 张**（`shared/scope.rs::SCOPED_TABLES`，启动自检 + 自检接口 `uncovered-tables` 逐张校验）：

| 表 | 部门列 | 归属列 |
|---|---|---|
| `sys_user` | `dept_id` | `id`（SELF 的「本人」是这一行） |
| `sys_dept` | `id` | —（SELF 退化为「本人所在部门这一行」） |
| `sys_import_task` / `sys_export_task` | — | `create_by` |
| `operation_logs` / `audit_logs` / `auth_logs` / `security_logs` / `password_change_logs` | — | `user_id` |
| `sys_notice` | — | `sender_id` |
| `sys_file` | — | `uploaded_by` |

**业务本地锚点 4 张**（不登记：可见性是 OR 语义「范围 ∪ 参与关系」，登记了会让自检误判）：

| 表 | 部门列 | 归属列 | 可见性 |
|---|---|---|---|
| `biz_task` | `dept_id`（建单时取负责人部门快照） | `owner_id` | 范围 ∪ 参与关系（负责人/创建人/分派人/协作人/我参与） |
| `biz_project` | `dept_id` | `owner_id` | 范围 ∪ 负责人/创建人/成员 |
| `biz_budget` | `dept_id` | — | 纯部门过滤 |
| `biz_expense_report` | `dept_id` | — | 纯部门过滤 |

> **只有这 5 张有 `dept_id` 的表会被部门范围真的裁到**（`sys_user` / `biz_task` / `biz_project` /
> `biz_budget` / `biz_expense_report`）。巡检 N1 就是盯这 5 张。

### 2.2 不受控（显式 `ScopeTarget::unscoped`，46 处锚点）

- **ERP 单据与主数据**：`biz_purchase_order(_item)` / `biz_stock_in` / `biz_stock_out` /
  `biz_stock_take` / `biz_stock_transfer` / `biz_inventory(_location/_txn)` / `biz_payable` /
  `biz_voucher` / `biz_contract` / `biz_material_serial` / `biz_asset*` / `biz_unit` /
  `biz_warehouse` / `biz_location` / `biz_material` / `biz_supplier` / `biz_account` /
  `biz_accounting_period` / `biz_seq` / 台账、报表、预算映射表。
- **全局元数据与配置**：`sys_role` / `sys_menu` / `sys_config` / `sys_dict_*` / `sys_job(_log)` /
  `sys_storage_config|policy` / `sys_notice_template` / `sys_help_doc` / `sys_change_log` / `sys_announcement`。

**为什么不做**：① 会计/库存类单据一旦按部门裁，跨部门勾稽（采购→入库→应付→凭证）会断链，
账实/账账的恒等式先垮（`erp_invariants` 的 B 组就是查这个）；② 这类表的边界本来就是
**权限码 + 审批流 + 台账**，而不是「行级可见性」；③ 全局配置按行裁会让「有权限码的人看不到配置」
（静默少数据，比多看见更难查）。这些都在代码里写成 `SCOPE-EXEMPT: <理由>` 的显式决定，不是漏接。

---

## 3. 授权清单（V0247，每条都有理由）

| 角色 | data_scope | sys_role_dept | 理由 |
|---|---|---|---|
| `SUPER_ADMIN` | 1 全部（不变） | — | 超管在 `filter_for` 里短路恒真（S9）；显式留 1 是为了让「界面显示的范围」和「实际行为」一致 |
| `GENERAL_MANAGER` 总经理 | 1 全部 | — | 职责就是全公司经营数据，1 是授权而不是缺省 |
| `DEPT_LEADER` 部门主管 | **3 本部门及以下** | — | 管本部门这棵子树、不含平级；用 3 而非 2 是因为有下级部门形态，用 2 会让下级数据凭空消失 |
| `PURCHASE_MANAGER` 采购经理 | **3 本部门及以下** | — | 采购口的单据/任务只应看到自己部门（及下级采购组） |
| `WAREHOUSE_MANAGER` 仓管主管 | **3 本部门及以下** | — | 仓库口同理，库/仓之间互不可见 |
| `FINANCE_MANAGER` 财务经理 | **5 自定义** | `FIN`（财务部） | 只管财务部归口的预算/费用；跨部门单据走审批流与共享台账，不靠「默认可见全公司」干活 |
| `FINANCE_DIRECTOR` 财务总监 | **5 自定义** | `FIN` + `HQ` | 多一层「复核总部各部门归口费用」的职责；CUSTOM **不展开子树**，所以显式列两个部门 |
| `FLOW_CONFIG_APPROVER` | 1（不动） | — | 只管流程定义/节点，不持单据数据；流程表本身不参与行级范围 |
| `QA_LIMITED` / `QA_VIEWER` / `MUTEX_A` / `MUTEX_B` | 1（不动） | — | 验收夹具角色，QA 用例依赖其权限码集；改范围会让既有用例假红 |

迁移特性：部门按 `dept_code`（`FIN`/`HQ`）解析而不是硬编码 uuid；目标库还没有组织树时
**不切 5、保持 1 并 RAISE NOTICE**（硬写 5 而无集合行会让持有者被静默降级成「仅本人」，比不管更糟）。
自检块断言三件事：超管必须仍为 1、`data_scope=5` 必须有集合行、有组织树就必须有部门级授权。

---

## 4. 真机验证

隔离实例：`cloudpivot_p0fix2`@99 / `127.0.0.1:18092`，`profile=test`，全走真实 HTTP。

| 证据 | 内容 | 结果 |
|---|---|---|
| `03_e2e_round1.txt` / `04_e2e_round2.txt` | `p0fix2_data_scope_e2e.py` **连跑两轮** | **16/16 × 2** |
| `02_migration_drill.txt` | 干净库 `up → down → up → up` | PASS（幂等） |
| `08_invariant_negative_control.txt` | 巡检反向对照：修好后 ✅ → 回放 down 变 ⚠️（点名 5 张表）→ 再 up 又 ✅ | 有牙 |
| `09_invariants_full_isolated.txt` | 全量不变量（隔离库） | **64/64**，BLOCKER 0 / WARN 0 |

e2e 的 16 条断言里与本卡验收直接对应的四条：

```
[PASS] ③ 建单落了「负责人所在部门」快照（biz_task.dept_id）
[PASS] ④ 财务部账号看得到本部门任务、看不到销售部任务        ← A 看不到 B 的
[PASS] ④ 销售部账号看得到本部门任务、看不到财务部任务
[PASS] ⑤ 对照：FINANCE_MANAGER 改回 data_scope=1 → 立刻能看到别部门任务
[PASS] ⑤ 对照：再改回 data_scope=5 → 别部门任务重新不可见（因果成立）
[PASS] ⑥ SUPER_ADMIN 仍能看到两个部门的任务（没被限死）
```

第 ⑤ 组是这张卡最关键的证据：**同一个账号、同一份代码，只有配置变了**，
可见性随之变化 ⇒ 「看不到」是范围配置造成的，不是恰好没有数据。

---

## 5. 生产落地（144 / `cloudpivot`）

`01_prod_before.txt`（修复前）→ `05_prod_apply.txt`（应用 V0247）→ `06_prod_after.txt`（生效核算）：

```
sys_role_dept            : 0 → 3 行（FINANCE_MANAGER→财务部；FINANCE_DIRECTOR→财务部+云枢总部）
data_scope 非 1 的角色    : 0 → 5 个（DEPT_LEADER/PURCHASE/WAREHOUSE=3，FINANCE_*=5）
缓存失效                  : INCR perm:user_scope_epoch → 1（旧范围键立即不可达，不用等 300s TTL）
服务                     : active / health=200；生产二进制含数据权限接口（未鉴权 401，非 404）
巡检                     : N1 ✅（07_prod_invariant_N1.txt）
```

生产 9 个在用账号的生效范围（`06_prod_after.txt` A 段，按 `RESOLVE_SQL` 同口径展开）：

| 账号 | 角色 | 生效范围 |
|---|---|---|
| admin / henry.liao / laoliu / laoliu_test | 含 `SUPER_ADMIN` | ALL（超管短路） |
| qa.limited / qa.mutex / qa.viewer | `QA_*` / `MUTEX_*`（**均 is_locked=true**） | ALL（角色 data_scope=1，本轮刻意不动） |
| cl_43870 / verify-ai | 无角色 | SELF（仅本人） |

**所以「生产 9 个用户都能看全部」这句评审结论只有一部分成立**：

- 4 个是**超管短路**（跟范围配置无关，改 data_scope 也不会限制他们）；
- 3 个是锁定状态的功能验收账号，其角色范围沿用默认 1（本轮按「不动验收夹具」处理）；
- 2 个**本来就看不到别人的数据**（无角色 → `merge_scope_rows` 安全侧降级 SELF）。

真正成立的病根是：`sys_role_dept` 0 行 + 全部业务角色 `data_scope=1`，**能力从未被配置过**。
这条已修。

---

## 6. 诚实缺口（本卡没做、也不该在本卡做）

1. **生产组织落位**：9 个账号里 7 个没有部门（`sys_user.dept_id IS NULL`），`biz_task` 846 行的
   `dept_id` 也因此 100% 为 NULL ⇒ 部门范围在生产上「配了但暂时裁不到任何行」。
   要让部门隔离真正对业务可见，需要先把人放进部门（组织决策，不是代码问题）。
2. **超管泛滥**：4 个在用账号持 `SUPER_ADMIN`（其中 2 个同时还挂着 4 个业务角色）。
   建议拆成「1 个运维超管 + 业务岗位角色」，否则范围配置对这些人永远无效。
3. **ERP 单据表不受行级范围控制**（见 §2.2）。若产品要求「采购只能看采购部单据」，
   那是另一个量级的改动（要先解决跨部门勾稽恒等式），需单独立卡。

以上三条已作为后续卡（见卡片评论）交接。

---

## 7. 怎么复跑（评审/后续卡现场核查用）

```bash
# ① 隔离实例（99）：库 cloudpivot_p0fix2 / 端口 18092 / profile=test
#    实例当前仍在跑（/tmp/p0fix2/backend.log）；要重建：
#    su - postgres -c 'dropdb --if-exists cloudpivot_p0fix2'
#    su - postgres -c 'createdb -O alun cloudpivot_p0fix2'
#    然后按 scripts/verify/e2e_isolated_instance.sh 的 start_backend 段起（
#    NIK_DATABASE_NAME=cloudpivot_p0fix2 / NIK_SERVER_LISTEN=127.0.0.1:18092 /
#    NIK_ADMIN_PASSWORD=Admin@123456 / CLOUDPIVOT_STORAGE_MASTER_KEY=<hex32>），
#    启动即自动跑到 V0247。

# ② 真机 e2e（幂等，可连跑）
python3 scripts/verify/p0fix2_data_scope_e2e.py            # 默认打到 18092 / cloudpivot_p0fix2
P2_BASE=http://127.0.0.1:18092 P2_DB_NAME=cloudpivot_p0fix2 \
  python3 scripts/verify/p0fix2_data_scope_e2e.py

# ③ 巡检（隔离库 / 生产各一次）
NIK_DATABASE_NAME=cloudpivot_p0fix2 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py --group 权限
# 生产（144）：
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; \
  python3 /opt/cloudpivot/scripts/verify/erp_invariants.py --group 权限'

# ④ 生产生效范围核算（只读）
ssh root@172.18.10.144 'bash /tmp/prod_verify.sh'   # SQL 见 06_prod_after.txt
```

注意事项（本卡踩过）：
- e2e 会在库里造 `P2-*` 夹具（任务/部门/账号），**跑完自动清理**并把
  `FINANCE_MANAGER` 的集合还原成空，所以可以反复跑；但它会改 `admin` 的初始口令
  （`must_change_password` 流程 → `P2@Passw0rd!`），`login()` 已做多口令回退。
- 巡检 N1 是 **WARN** 级：老库（全库 `data_scope=1`、`sys_role_dept` 0 行）会告警，
  这是**预期**（就是本卡在修的那个状态），不是新引入的失败。

