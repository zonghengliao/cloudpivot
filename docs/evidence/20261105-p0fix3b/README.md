# 【P0-FIX-3B】QA 验收夹具角色 data_scope 1 → 4（仅本人）

> 卡片：`t_b2d339ab`｜标的库：生产 `cloudpivot`（144）+ 演练库 `cloudpivot_e2e_iso`（99，生产克隆）
> 结论：4 个验收夹具角色（`QA_LIMITED` / `QA_VIEWER` / `MUTEX_A` / `MUTEX_B`）的数据范围
> 从 **1（全部数据）→ 4（仅本人）**，角色行 / 权限码 / 绑定一行未动；生产 **0 个在线用户受影响**；
> 动作用迁移 `V0249` 收口（up/down + 幂等演练 + 引擎实跑验证 + 生产正式记账）。

---

## 1. 前提核对：4 个角色确实「0 活跃持有者」（先只读盘点，不对就不动手）

生产实测（`01_prod_before.txt`，2026-11-05 13:44 之后）：

| 角色 | data_scope | status | 活跃持有者<br>`deleted_at is null and status = 1` | 绑定行（含禁用账号） |
|---|---|---|---|---|
| `QA_LIMITED` | 1 | 1 | **0** | 1（`qa.limited`，status=0） |
| `QA_VIEWER`  | 1 | 1 | **0** | 1（`qa.viewer`，status=0） |
| `MUTEX_A`    | 1 | 1 | **0** | 1（`qa.mutex`，status=0） |
| `MUTEX_B`    | 1 | 1 | **0** | 1（`qa.mutex`，status=0） |

持有者明细（同一份证据 ② 段）：4 条绑定全部指向 P0-FIX-3 已置 `status=0` 的账号，
行保留、未软删、`expire_time` 为空 —— 「绑定还在、人已停用」正是本卡能安全动手的前提。

> 若盘点出现活跃持有者，本卡按卡面要求**停手回报**；实测为 0，故继续。

## 2. 方案选择：A（收范围到 4），不选 B（软删角色）

| | 方案 A：`data_scope = 4`（采用） | 方案 B：软删 4 个角色 |
|---|---|---|
| 生产干净度 | 角色还在，但没人持有时 **4 与软删的实际权限效果相同**（拿不到任何别人的行） | 更「干净」 |
| 验收夹具复用 | 角色 + 28~30 条权限码 + 互斥配置原样保留，复用零重建 | 需重建角色、重挂 `sys_role_menu`、重配 `sys_role_mutex`，夹具易漂移 |
| 审计/历史引用 | `sys_user_role` / `sys_role_menu` 的引用仍指向真实角色 | 引用变成「指向已删角色」 |
| 风险 | 若以后有人把账号挂上来，默认「仅本人」（安全侧） | 若以后重建，容易漏权限码（用例假红/假绿的来源） |

**为什么是 4 而不是 3 / 5**（写进迁移注释，避免下一个人再猜）：
- `3`（本部门及以下）需要「持有者所在部门」才有意义，而验收账号按设计**不进组织树**（`dept_id` 为空）；
  `merge_scope_rows` 对解析不出部门的部门类范围会**静默降级 SELF**（S10/S11）→ 配置和实际行为打架；
- `5`（自定义集合）必须配 `sys_role_dept` 行，空集合同样降级 SELF，还会踩巡检 N1-b；
- `4` 是最紧的非删除态：语义明确、不依赖组织树、不需要集合行，与「夹具只应看见自己造的数据」一致。

**未来复用口径**：要验「能看全量」的前置时，由夹具脚本**显式**把角色改回 1、收尾改回 4，
不再依赖「角色默认就是 1」这种没人记得的隐式前提。

## 3. 交付物

| 文件 | 作用 |
|---|---|
| `backend/migrations/business/V0249__qa_fixture_role_scope_self_only.up.sql` | 逐个角色判决：**有活跃持有者就跳过并 NOTICE**，无持有者才 1→4；自带三段自检（无持有者必须已收紧 / 不许软删 / 超管必须仍为 1） |
| `backend/migrations/business/V0249__qa_fixture_role_scope_self_only.down.sql` | 4→1 还原基线（只碰当前值=4 的行，不动别人后来配的 2/3/5） |
| `scripts/datafix/qa_role_scope4/10_precheck.sql` | 只读盘点（角色/持有者/全量角色/逐账号核算） |
| `scripts/datafix/qa_role_scope4/20_snapshot.sql`、`30_verify.sql` | 快照（kv，整串比对）+ 8 组断言（A~G，含 N1 两条判据与逐账号核算） |
| `scripts/datafix/qa_role_scope4/drill.sh` | up → up(幂等) → down(可逆) → up(复原)；开头先 `down` 归位，**可反复跑** |
| `scripts/datafix/qa_role_scope4/apply.sh` | 三道闸（生产写保护 / 当场备份 / 写后验收）+ `--check` / `--rollback` |
| `scripts/datafix/qa_role_scope4/40_ledger_import.sql` | 把 psql 直放过的 V0247 / V0249 正式记账（`applied_via=imported`） |
| `scripts/datafix/qa_role_scope4/50_prod_diff.sh`、`60_prod_after_state.sql` | 生产前后差异核算 / 应用后现状 |

迁移号：动手前 `ls backend/migrations/business/ | tail` 实测最大号 `V0248`（另一卡在途、未提交），
故本卡用 **V0249**；演练/引擎实测时 `rsync --exclude V0248__*` 明确排除在途迁移，避免串味。

## 4. 演练与验证

### 4.1 迁移 up/down/up 幂等（演练库 = 生产克隆）

`03_drill_isoclone_up_down_up.txt`（库 `cloudpivot_e2e_iso`，由生产 `pg_dump` 还原，12 角色 / 9 账号 / 3 条 `sys_role_dept` 与生产一致）：

```
⓪ 归位 down            → 4 个角色回到基线 1
① up                   → 4 行 1 → 4，自检通过；30_verify(exp=4) 零 FAIL
② up 再跑一次           → 4 条「已是 data_scope=4，跳过（幂等）」；快照整串相等
③ down                 → 4 行 4 → 1；快照与演练前**整串相等**（8 项断言零 FAIL）
④ up 复原              → 快照与第一次 up 后整串相等
✅ 全绿（9 个 ✅）
```

### 4.2 引擎实跑（不是「只能 psql 跑」）

`05_engine_apply_proof.txt`：用当前 main 构建的 `backend/target/debug/cloudpivot`（`profile=test`，
CWD 指向只含 `V0247 + V0249` 的迁移目录）在生产克隆库上启动：

```
数据库迁移完成: 应用 2 个迁移
schema_migrations: 247 executed (seq 194) / 249 executed (seq 195)
checksum(249) = 5962afa3…fab61 == sha256sum(V0249….up.sql)   ← 逐字节相等
```

这一步同时证明了：`DO $$ … $$` 块在引擎的 simple-query 批量执行下没问题（无 psql 元命令）、
checksum 口径 = LF 归一后的 `sha256sum`、记账行形态正确。

### 4.3 隔离库全量不变量

`04_isolated_invariants_full.txt`（克隆库，V0249 生效后）：

```
结果：65/65 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

## 5. 生产落地（144 / `cloudpivot`）

`apply.sh` 在 144 上跑（`/opt/qa4`），三道闸：生产写保护（`QA4_ALLOW_PROD=1 + QA4_REASON`，留痕
`/tmp/qa4-prod-write.log`）→ 当场备份（`/root/datafix-backup-qa4/20261105-135700-apply/`，
`tables.sql` 6 张权限表 + `full.sql` 全库 + MANIFEST）→ 写后 `30_verify` 零 FAIL。

| 项 | 结果 | 证据 |
|---|---|---|
| 4 个角色 `data_scope` | 1 → **4** | `09_prod_after_state.txt` ① |
| 角色行 / status / 权限码 | 未动（4 行仍在、`status=1`、28/28/30/29 条权限码） | `10_prod_verify_after_apply.txt` C1/C2 |
| 其它 8 个角色范围 | 未变（DEPT/PURCHASE/WAREHOUSE=3，FINANCE_*=5，GM=1，SUPER_ADMIN=1） | D1/D2 + `08_prod_before_after_diff.txt` ①（diff 只有 4 行 data_scope） |
| 行数 | `sys_role` 16→16、`sys_user` 26→26、`sys_user_role` 16→16、`sys_role_menu` 591→591、`sys_role_dept` 3→3 | `08_…`② |
| 绑定关系 | 逐 role_id 完全一致（0 处差异） | `08_…`③ |
| 第二轮 apply（幂等） | up 全打「已是 4，跳过」，记账 `INSERT 0 0` ×2，验收零 FAIL | `06_prod_apply_round2_idempotent.txt` |
| 巡检 N1 | ✅（`--group 权限` 1/1） | `11_prod_invariants_after_round2.txt` |
| 全量巡检 | **65/65，BLOCKER 0 / WARN 0**（含 O1 生产 e2e 干净） | 同上 |
| 服务 | `active`，`/health` 8023=200、nginx 80=200 | `09_…`⑥ |
| 缓存 | `INCR perm:user_scope_epoch` → 2，残留 `perm:user_scope:*` 键 0（旧范围键立即不可达，不等 300 s TTL） | `09_…`⑤ |

### 逐账号核算：没有任何在线用户受影响

生产在用账号只有 3 个（`08_…`④），**全部走超管短路**：

| 账号 | 启用角色 | 生效范围 | 受本卡影响？ |
|---|---|---|---|
| `admin` | `SUPER_ADMIN`(1) | ALL（`scope.rs::filter_for` 超管短路，与范围配置无关） | 否 |
| `henry.liao` | `DEPT_LEADER`(3) `GENERAL_MANAGER`(1) `PURCHASE_MANAGER`(3) `WAREHOUSE_MANAGER`(3) `SUPER_ADMIN`(1) | ALL（超管短路） | 否 |
| `laoliu` | `SUPER_ADMIN`(1) | ALL（超管短路） | 否 |

被收紧的是 4 个**没有活跃持有者**的夹具角色 ⇒ 不存在「某人突然看不到自己该看的数据」；
也不存在「范围被放大」的路径（方向只会更严）。`qa.limited` / `qa.viewer` / `qa.mutex` /
`cl_43870` / `verify-ai` / `laoliu_test` 仍是 `status=0`（P0-FIX-3 状态），本卡未改它们的任何一列。

## 6. 迁移记账（顺手补了 P0-FIX-2 漏的一行）

生产 `schema_migrations` 原本停在 **246**：V0247（P0-FIX-2）是 `psql` 直放的，账上没行。
不补的后果是下次启动「版本 246 → 249」出现乱序补迁 WARN（会误导排查的人以为是新迁移乱插队）。
故 `40_ledger_import.sql` 一次性把两行都记上（`applied_via=imported`，checksum 已用引擎实跑值核对）：

```
247 | data_scope_minimal_grants          | imported | dda0287e…021dc | seq 194
249 | qa_fixture_role_scope_self_only    | imported | 5962afa3…fab61 | seq 195
```

`--rollback` 只删 249 那行（247 确实被执行过，账要留）。144 的检出仍停在 `7d96692`
（迁移文件最大 V0246），下次 `deploy.sh` 把 V0249 带上去时引擎会因 checksum 命中而**跳过**，
不会重复执行。

## 7. 诚实缺口（本卡不做 / 留给后续）

1. **生产 144 的检出还是 7d96692**：本卡只在库里落了账，`/opt/cloudpivot/backend/migrations/`
   里还没有 V0249 文件（要等下一次 `deploy.sh`）。这也是「psql 直放 + 正式记账」这条路的原因 ——
   按「能走迁移引擎就不直接改库」的口径，下一轮发布后引擎会自然对齐。
2. **夹具角色长期留在生产**是否合理，仍是产品/架构决策（本卡按卡面默认方案 A 处理：留行收紧）。
   若后续决定不在生产留夹具，另有「软删 + 清 `sys_role_menu`/`sys_role_mutex`」的活要单独立卡。
3. **验收账号仍绑定在这 4 个角色上**（`status=0`）：若将来有人「启用账号」而忘了改范围，
   该账号会以「仅本人」登录（安全侧失败），排查时会看到「看不到别人的数据」——这是**刻意**的：
   显式改范围比隐式放开好。
4. 生产上此刻**没有**任何账号持有这些角色，所以本卡无法用「真人可见性差异」做端到端对照
   （没有持有者就没有对照组）；因果证据来自迁移自检 + `30_verify` 的 A/B/D/E 组 + 克隆库引擎实测。
5. **刻意没加「无持有者的全量角色」巡检**：这类判据在生产上会在「新建角色 → 还没挂人」的
   正常操作窗口里亮 WARN，变成永远响的指标（P0-FIX-3 的 O1 之所以只在生产判、且只认
   `[E2E]` 标记，就是同一个理由）。本卡的控制点是**迁移幂等 + 有活跃持有者自动跳过 + 文档化复用口径**，
   而不是一个会误报的 WARN。若后续确实要监控「夹具角色被重新放大范围」，那是权限治理卡的范围
   （需要先定义「验收夹具角色」的机读标记，例如 `sys_role.remark` 里的固定前缀）。

## 8. 怎么复跑

```bash
# ① 只读盘点（生产，144）
ssh root@172.18.10.144 'bash /opt/qa4/scripts/datafix/qa_role_scope4/run_sql.sh \
  /opt/qa4/scripts/datafix/qa_role_scope4/10_precheck.sql'

# ② 现状 / 验收（生产，只读）
ssh root@172.18.10.144 'bash /opt/qa4/scripts/datafix/qa_role_scope4/apply.sh --check'

# ③ 演练（99，克隆库；脚本开头会先 down 归位，可反复跑）
DB_NAME=cloudpivot_e2e_iso DB_USER=e2erole DB_PASSWORD=e2eiso_2026 \
  bash scripts/datafix/qa_role_scope4/drill.sh

# ④ 隔离库全量不变量
NIK_DATABASE_NAME=cloudpivot_e2e_iso NIK_DATABASE_USER=e2erole NIK_DATABASE_PASSWORD=e2eiso_2026 \
  python3 scripts/verify/erp_invariants.py

# ⑤ 回滚（生产）
ssh root@172.18.10.144 'QA4_ALLOW_PROD=1 QA4_REASON="回滚 P0-FIX-3B" \
  bash /opt/qa4/scripts/datafix/qa_role_scope4/apply.sh --rollback'
```

演练库说明：`cloudpivot_e2e_iso` 由生产 `pg_dump --no-owner --no-privileges` 还原（99 上 `alun`
没有 CREATEDB 权限、无人值守会话又不允许 `su - postgres`，故复用既有空库并 `DROP SCHEMA public CASCADE`
重建；这库同时是 `scripts/verify/e2e_isolated_instance.sh` 的默认库，下次起隔离实例会被重新灌库，无残留风险）。
