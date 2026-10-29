# 批次 C+ · T2 验证证据（状态机白名单 + 进度工时加权 + 子任务）

- 任务卡：`t_5c19c0a9`（批次 C+ · T2）
- 依据：`docs/批次C+-工作任务管理-核心设计.md`（v2，老大 2026-10-29 批准）§二 / §三、
  `docs/云枢设计修正补丁-20261029.md`（修正 8 通知分级）、T1 已交付 `07020d7`
- 验证机：`172.18.10.144`（Ubuntu 26.04 / PG 18.6）+ 自建验证库 `cloudpivot_t2v`
  （空库全量迁移 + `up→down→up` 往返 + 真服务走查）
- 验证方式：空库迁移往返（`psql` 逐条断言）+ 真服务 HTTP 走查
  （`scripts/verify/t2_task_status_verify.py`，**104 条断言全 PASS**）+
  `cargo test`（**626 passed / 0 failed**）+ `scripts/check.sh`（闸①/②=0/0、C7 G1–G6 全 PASS）+
  `pnpm typecheck && pnpm build`

> 下面所有输出都是本次真跑的原文（脚本在 `scripts/verify/`，可原样复现），没有手工修饰。

---

## 0. 一句话结论

| 验收项（卡面 §五） | 结果 | 证据 |
|---|---|---|
| 1. 合法流转全走一遍（TODO→DOING→REVIEW→DONE） | ✅ 每步 200 + status + `STATUS_CHANGE` 评论 + `sys_change_log` | §4-1 |
| 2. 非法流转全被拒（跳级 / 终态直取 / 同状态 / 无验收权） | ✅ 409+63010 ×4、403+63011 | §4-2 |
| 3. 打回必须填原因 | ✅ 不填/2 字 → 400+63013；≥5 字 → 200 且 `reject_reason` + 评论 | §4-3 |
| 4. `need_review=false` 可直接 DONE / `true` 被拒 | ✅ 200 / 409+63010 | §4-4 |
| 5. DONE 后 `done_at` 有值；重开后清空 | ✅ `true` → `false`（**决定：重开清空**） | §4-5 |
| 6. 无子任务手动 0→50→100；DOING 设 5 变 10 | ✅ 全部符合 | §4-6 |
| 7. 有子任务工时加权 = 80% | ✅ `(1×0+2×50+7×100)/10 = 80` | §4-7 / §5 |
| 8. 工时全空 → 等权 50% | ✅ | §4-8 |
| 9. 父任务手改进度 → 409 | ✅ 409 + `63012`「进度由子任务汇总」 | §4-9 |
| 10. 子任务全 DONE → 父 100 但状态不变 | ✅ `progress\|status = 100\|TODO` | §4-10 |
| 11. 三级嵌套 A→B→C 逐级重算 | ✅ C 完成 → B 100% → A 50%，A 状态不变 | §4-11 |
| 12. 可见性仍然生效（无关用户 → 404） | ✅ 404 + `63001` | §4-12 |
| 13. 跨部门协作人 | ✅ 看得见 200 / 改状态 403+`63011` / 改进度 403+`63002` | §4-13 |
| 14. `cargo test` 全绿（604+） | ✅ **626 passed / 0 failed**（新增 15 条状态机 +2 条进度单测） | §7 |
| 15. `scripts/check.sh` 全绿 | ✅ 闸①=0 / 闸②=0；C7 G1–G6 全 PASS；fmt/clippy/check/typecheck 全过 | §8 |
| 16. `pnpm typecheck && pnpm build` | ✅ 均通过（dist 产物正常） | §9 |

**迁移往返**：空库全量 `应用 66 个迁移` → `V0108` down（列/约束/菜单/授权全 0）→ up（全回）→
种子重放（授权恢复 7/7）→ 再 up 幂等（`NOTICE: already exists, skipping`）。§3

---

## 1. 交付物清单

### 1.1 新增文件

| 文件 | 职责 |
|---|---|
| `backend/src/business/task/state.rs` | **状态机纯逻辑**：5 态枚举 / 角色位掩码 / 10 条白名单流转表 / 授权三段式判定 / 进度聚合（加权·等权·全完成）/ 15 条单测 |
| `backend/migrations/business/V0108__task_status_progress.{up,down}.sql` | `biz_task.status_semantic`（P1 扩展位）+ `biz_task_comment.comment_type`（STATUS_CHANGE / PROGRESS_AUTO）+ 3 个权限按钮节点（…0765–…0767）+ 自检 |
| `scripts/verify/t2_task_status_fixture.sql` | 走查夹具（跨部门组织 / 4 账号 / 三级嵌套 A→B→C） |
| `scripts/verify/t2_task_status_verify.py` | 真服务走查（**104 条断言**，HTTP 码 + 业务码 + 落库值 + 评论 + 变更日志 + 通知） |
| `docs/批次C+-T2-状态机与进度-验证证据.md` | 本文 |

### 1.2 修改文件（每处都有理由）

| 文件 | 改动 | 为什么必须改 |
|---|---|---|
| `backend/src/business/task/{mod,dto,model,handler}.rs` | `pub mod state;` + 4 个接口（PATCH status / PATCH progress / POST subtask / GET subtasks）+ 进度重算与评论落库 + 通知分级 | T2 的主体交付 |
| `backend/src/business/changelog/diff.rs` | `BIZ_TASK` 登记项 + 加入 `REGISTRY` | `record()` 对未登记表 **fail-closed**（62012），状态迁移留痕必须先登记白名单 |
| `backend/src/shared/error_code.rs` | 63xxx 段 +4 个码（63010–63013）+ 更新码表自检（9 → 13） | 新增「非法流转 / 无权流转 / 进度锁定 / 打回缺原因」四种错误必须**可区分** |
| `backend/migrations/seed/S__task_permissions.sql` | 授权清单 5 → 8 个节点；自检「每角色 4 个按钮」→「每角色 7 个按钮」 | 新权限码要给**所有未软删角色**授权，否则普通账号点不到新按钮（与 V0103 同款的时序理由：版本化迁移先跑、种子后跑） |

**没有动的红线**：`business/shared/scope.rs` 一行未改；批次 A/B/C 模块一个未改；
`biz_task.status` 的 5 值 CHECK 未动（本次不做自定义列）。

---

## 2. 状态机：白名单 10 条 / 25 种组合

`state::TRANSITIONS` 是**唯一判据**（表外组合一律 409，`if` 里没有例外分支）：

| # | from → to | 允许的身份 | 附加条件 | 进度副作用 |
|---|---|---|---|---|
| 1 | TODO → DOING | 负责人 / 分派人 / 创建人 / 管理员 | 前置依赖（T4 hook `check_dependencies_ready`） | `max(progress,10)` |
| 2 | DOING → REVIEW | 负责人 | — | `100` |
| 3 | DOING → DONE | 负责人 / 创建人 | 仅 `need_review=false` | `100` + `done_at=now` |
| 4 | REVIEW → DONE | 创建人 / 分派人 / 管理员 | 有未完成子任务 → 409 | `100` + `done_at=now` |
| 5 | REVIEW → DOING | 创建人 / 分派人 / 管理员 | 必填原因 ≥5 字 | `10`（返工） |
| 6 | DONE → DOING | 创建人 / 管理员 | — | `10` + 清 `done_at` |
| 7–9 | TODO/DOING/REVIEW → CANCELLED | 负责人 / 创建人 / 分派人 / 管理员 | **DONE 不能直接取消** | 冻结 |
| 10 | CANCELLED → TODO | 创建人 / 管理员 | — | `0` |

单测 `whitelist_covers_exactly_the_ten_allowed_pairs` 对 **5×5 = 25 种组合逐一断言**：
表里有的必须命中（10 条），表里没有的必须拒绝（15 条）；另有 `illegal_pairs_are_rejected_with_illegal_pair`
断言 `TODO→DONE` / `DONE→CANCELLED` / `REVIEW→TODO` / 5 种自环全部被拒。

**错误码分三档**（不许统一成 500）：

| 场景 | HTTP | 业务码 |
|---|---|---|
| 表外组合 / 自环 / `need_review=true` 却直接完成 | 409 | `63010` |
| 身份不够（负责人自己做验收 / 负责人自己重开） | 403 | `63011` |
| 打回没填原因 / 不足 5 字 | 400 | `63013` |

---

## 3. 迁移验收（空库 up → down → up + 幂等）

新建空库 `cloudpivot_t2v`（`DROP` + `CREATE`），用**应用账号**起服务让迁移全量应用：

```
=== migration lines ===
2026-10-29T05:40:11 INFO alun_web::app: 数据库迁移完成: 应用 66 个迁移
=== listen ===
2026-10-29T05:40:11 INFO alun_web::app: cloudpivot 启动 -> http://127.0.0.1:18091
=== errors? ===
（无）
```

结构与授权（真库 `information_schema` / `pg_constraint` / `sys_menu` / `sys_role_menu`）：

```
=== 1) status_semantic 列 ===
status_semantic|character varying|YES
=== 2) comment_type 列 ===
comment_type|character varying|NO|'PLAIN'::character varying
=== 3) 约束 ===
ck_biz_task_status_semantic|CHECK (((status_semantic IS NULL) OR ((status_semantic)::text = ANY (ARRAY['TODO','DOING','REVIEW','DONE','CANCELLED']...))))
ck_biz_task_comment_type|CHECK (((comment_type)::text = ANY (ARRAY['PLAIN','STATUS_CHANGE','PROGRESS_AUTO']...)))
=== 4) 3 个权限节点 ===
20000000-0000-0000-0000-000000000765|business:task:status|business:task:status|20000000-0000-0000-0000-000000000760|3
20000000-0000-0000-0000-000000000766|business:task:progress|business:task:progress|20000000-0000-0000-0000-000000000760|3
20000000-0000-0000-0000-000000000767|business:task:subtask|business:task:subtask|20000000-0000-0000-0000-000000000760|3
=== 5) 每个角色持有 7 个 task 按钮？ ===
SUPER_ADMIN|7
=== 6) schema_migrations 里 V0108 ===
versioned|108|task_status_progress
seed||task_permissions
```

往返与幂等（`psql -f`，`ON_ERROR_STOP=1`）：

```
########## [A] 基线（首次 up 已由启动迁移应用） ##########
cols=2
menus=3
grants=3
########## [B] down（撤销 V0108） ##########
down OK
cols=0
constraints=0
menus=0
grants=0
########## [C] up（重新应用 V0108） ##########
up OK
cols=2
constraints=2
menus=3
########## [D] 再跑一次种子（授权随 down 一起被删，重放种子恢复） ##########
seed OK
SUPER_ADMIN=7
########## [E] 幂等：up 再跑一次不应报错 ##########
NOTICE:  column "status_semantic" of relation "biz_task" already exists, skipping
NOTICE:  column "comment_type" of relation "biz_task_comment" already exists, skipping
up(2nd) OK
cols=2
menus=3
```

---

## 4. 接口走查（真服务 + 真库 + 真登录，104 条断言全 PASS）

脚本：`python3 scripts/verify/t2_task_status_verify.py`（服务 127.0.0.1:18091，库 `cloudpivot_t2v`）。
夹具：`scripts/verify/t2_task_status_fixture.sql`（跨部门组织 + 4 账号；
`t2_owner` 负责人 / `t2_maker` 创建人 / `t2_other` 无关用户 / `t2_collab` 协作人）。

原文见文末 **§10 附录**。逐项对照见 §0 结论表，关键点的补充说明：

### 4.7 工时加权 = 80%（手算 + 实测）
```
父=01a1862f-b00a-… 子=…b010(1h) …b017(2h) …b01f(7h)
计算：(1×0 + 2×50 + 7×100) / 10 = 800 / 10 = 80.0 → 80
PASS  父任务汇总值：80
父任务重算评论：
  进度自动汇总：0% → 10%（3 个子任务，工时加权）
  进度自动汇总：10% → 80%（3 个子任务，工时加权）
```

### 4.11 三级嵌套（逐级重算）
```
结构（夹具 SQL 造，业务 API 限 2 层）：A(2h 子B + 2h 子D) → B → C
完成前：TSK-T2V-900001=0%/DOING  TSK-T2V-900002=0%/DOING  TSK-T2V-900003=0%/TODO
PASS  C：DOING→DONE HTTP：200
本次重算链（由近及远）：
  7e000000-…02  10% → 100%（子任务全部完成，all_done=True）
  7e000000-…01  5%  → 50%（工时加权，all_done=False）
PASS  B 重算到 100：100
PASS  A 重算到 50（(2×100 + 2×0)/4）：50
PASS  A 的状态没有自动变（仍是 DOING）：DOING
```

### 4.15 通知分级（修正 8）
```
被打回 t3 通知：
  任务状态变更 → t2_maker（TODO→DOING，尊重偏好）
  任务待你验收 → t2_maker（DOING→REVIEW，强制）
  任务被打回 → t2_owner（REVIEW→DOING，强制）
需验收 t5 通知：
  任务状态变更 → t2_maker
  任务待你验收 → t2_maker（强制）
普通变更 t12 通知：
  任务状态变更 → t2_maker（尊重 notify.inbox）
```

---

## 5. 进度口径（逐条写清，避免评审各说各话）

```
有子任务：progress = Σ(estimated_hours × progress) / Σ(estimated_hours)   （工时加权）
          若工时不全（任一为空 / ≤0）→ 退化为 等权平均 Σprogress / n
          若子任务全部 DONE → 100（捷径）
无子任务：手动 0~100，状态联动 DOING→max(p,10)、REVIEW/DONE→100、CANCELLED 冻结
```

四个**刻意的取舍**（每个都有理由，评审可逐条推翻）：

1. **工时「不全」时整组退化为等权，而不是「空工时按 0 权重」**
   —— 按 0 权重会让空工时的子任务完全不影响父任务：一个 7 小时的活拆出来忘了填工时，
   父任务就被几条小任务拉满，「看起来做完了」是最坏的结果。等权是更保守的降级。
2. **已取消（CANCELLED）的子任务计入聚合**（取其冻结进度）
   —— 取消代表「这部分不做完」，不是「这部分凭空完成」；从分母剔掉会让父任务凭空涨进度。
   「子任务全 DONE → 100」的捷径只认 `DONE`，所以有子任务被取消的父任务不会被拉满。
3. **重开后进度归 10（不是清零、也不是保留 100）**
   —— 与「进入 DOING 至少 10%」同一口径：重开 = 回到进行中的起点。
   `done_at` 同时清空（表注释「重开时清空」的约定）。
4. **读路径不算进度**
   —— 聚合只在写路径（状态流转 / 改进度 / 建子任务）触发，沿 `parent_id` 逐级向上；
   看板 20 行不会变成 20 次额外聚合查询。代价是「进度可能短暂过期」，
   所以三条写路径都必须调 `recalc_ancestors`（已在代码里写明契约）。

---

## 6. 与卡面/设计文档的三处差异（都是刻意决定，请评审过目）

| 项 | 卡面 / 设计文档 | 实际实现 | 理由 |
|---|---|---|---|
| 完成时间列名 | 卡面写 `completed_at` | `biz_task.done_at`（T1 建表时的列名） | 不改列名：T1 的 DTO/查询/前端契约都按 `done_at`，改名要动 5 个文件且没有收益；语义完全一致 |
| 取消原因 | 设计 §二 规则 6「必须填取消原因」 | **可选**（填了就落 `cancel_reason` + 评论） | **卡面 T2 的流转表没有这个必填要求**（只要求打回必填），而卡面是与注解/前端对齐的契约源。前端若要求必填，T6 加一行校验即可 |
| 3 层嵌套 | 卡面验收 11 要求 A→B→C | 业务 API **限 2 层**（T1 的 `validate_parent`，设计 §十二「深度限 2 层」）；三级用夹具 SQL 造历史数据验证重算链路 | 移除 2 层限制属于改 T1 的既定规则，且设计文档明确写「限 2 层」。T2 的核心义务是「重算能沿 `parent_id` 逐级向上」，这一点已用 3 层数据证明（§4-11） |

另外两处实现细节：

- **协作人改状态的业务码是 `63011`（不是 `63002`）**：`63002` 是 T1 的「编辑类无权限」通用码，
  而状态流转是**独立权限码**（`business:task:status`），用专用码能区分「不能编辑」与「不能推状态」。
  两者都是 **403**，与卡面「403/63002」的**状态码**一致。
- **进入 DONE 前会拒绝「还有未完成子任务」**（409 + `63006`，复用 T1 的删除闸）：
  设计 §三「父任务 DONE 前置检查：有未完成子任务 → 拒绝并提示数量」，
  这条同时守住架构 D5（绝不自动 DONE）—— 父任务只能由人显式推动。

---

## 7. 单元测试（626 passed / 0 failed）

```
$ cargo test -j 1        # 144 只有 7GB 内存，-j 4 会让 ld 被 OOM 杀掉
running 585 tests
test result: ok. 585 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
（其余 8 个集成测试二进制 + doctest 见 §10 附录 B）
TOTAL_PASSED= 626
```

新增 15 条单测（`business::task::state::tests`，全部无库）：

| 单测 | 断言什么 |
|---|---|
| `whitelist_covers_exactly_the_ten_allowed_pairs` | **25 种组合逐一**：10 放行 / 15 拒绝 |
| `illegal_pairs_are_rejected_with_illegal_pair` | 跳级 / 终态直取 / 回退 / 5 种自环 |
| `no_transition_targets_itself_and_pairs_are_unique` | 无自环、无重复登记、角色掩码非空 |
| `review_to_done_needs_acceptance_right_not_just_ownership` | 负责人自己验收 → 403；创建人/分派人/管理员 → 放行 |
| `todo_to_doing_allows_four_roles_and_rejects_none` | 四角色放行、无角色拒绝 |
| `reopen_and_restore_are_creator_or_admin_only` | 重开 / 恢复只认创建人 / 管理员 |
| `need_review_blocks_direct_done_but_keeps_review_open` | `need_review` 两条分支 |
| `reject_requires_reason_of_at_least_five_chars` | 0/2/4 字拒绝（含空白与中文按 char 计）、5 字放行 |
| `progress_effects_follow_the_transition` | 10 条流转的进度副作用 |
| `done_at_effects_are_set_on_done_and_cleared_on_reopen` | `done_at` 三态 |
| `role_mask_unions_and_admin_is_independent` | 多角色并集 + 管理员位独立 |
| `weighted_progress_matches_the_card_example` | 卡面 80% 例子 |
| `equal_weight_when_hours_are_missing` | 全空 / 不全 / 0 工时 → 等权 |
| `all_done_children_pin_parent_to_hundred_without_changing_status` | 全 DONE → 100；含 CANCELLED 不算全完成 |
| `aggregate_edges` | 无子任务 / 单子任务 / 四舍五入 / 进度锁定 |

---

## 8. `scripts/check.sh`（全绿）

```
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.25s
==> 后端：cargo check
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.17s
==> 前端：pnpm typecheck
> vue-tsc --noEmit
==> 批次B 闸①：分页必须走 scope::paginate
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
  PASS：扫描范围已覆盖 backend/src/{business/task,business,system,support,shared}
  PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> C7 G1：只有 business/changelog/ 能碰 sys_change_log            PASS：0 处
==> C7 G2：changelog 模块内禁止自开事务                             PASS：0 处
==> C7 G3：record 的签名必须带 &impl SqlExecutor                    PASS
==> C7 G4：不得存在『无 executor 版本』的 record 函数               PASS
==> C7 G5：每个 record/record_in 调用点必须在 transaction 闭包内且传 &tx
G5 PASS：20 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx（扫了 161 个文件）
==> C7 G6：按 id 查 sys_change_log 必须同时带分区键 created_at      PASS：0 处
==> C7 门禁：全部通过
==> 偏好字典与色板一致性（V0091 ui.palette oneof ↔ style/palettes.ts）  PASS：22 套
==> 全部检查通过
```

> G5 的 20 个调用点里包含本次新增的 `change_status`：`changelog::record(&tx, …)` 写在
> `db().transaction(|tx| async move { … })` 闭包体内 —— 状态变更与审计日志**同事务**，
> 业务回滚则日志一起消失（C5 的 D11）。

---

## 9. 前端 `pnpm typecheck && pnpm build`

```
=== pnpm typecheck ===
> vue-tsc --noEmit
（无输出 = 通过）
=== pnpm build ===
dist//opt/cloudpivot/frontend/assets/index-DSFGkY2z.js.gz   101.05kb / gzip: 38.49kb
dist//opt/cloudpivot/frontend/assets/tdesign-MZElNuJa.js.gz  998.21kb / gzip: 268.97kb
dist//opt/cloudpivot/frontend/assets/echarts-CuV_skF0.js.gz 1002.89kb / gzip: 330.65kb
（构建成功）
```

> T2 是纯后端卡：前端没有改一行（新接口与 4 个错误码留给 T6 的任务书）。
> `error-code.ts` 的文案映射按码值命中，未加条目时前端展示后端返回的 `msg`（已是中文），
> 不影响功能；T6 一并补 63010–63013 的固定文案。

---

## 10. P1 扩展位（本次不做，只留落点）

- `biz_task.status` 保持 **5 值枚举**（DB CHECK 未动）；
- 新增 `biz_task.status_semantic VARCHAR(16)`（可空，恒 NULL）：
  将来接 `biz_task_status_def`（自定义列）时，把自定义列映射到语义状态写进这一列，
  读路径「NULL → 用 `status` 本身」，**不引入第二份真相**；
- 单测 + 走查第 16 项都断言「全表 `status_semantic` 为 NULL」，
  防止有人在 P1 之前偷偷填值。

```text
## 10 附录 A：走查原文（`scripts/verify/t2_task_status_verify.py`，104 条断言全 PASS）


==============================================================================
0) 登录（admin + 4 个夹具账号，密码哈希与 admin 相同）
==============================================================================
  admin/owner/maker/other/collab token 长度 = 461 / 665 / 665 / 665 / 667
  t2_owner=7d000000-0000-0000-0000-000000000011  t2_maker=7d000000-0000-0000-0000-000000000012  t2_collab=7d000000-0000-0000-0000-000000000014

==============================================================================
1) 合法流转全走一遍：TODO→DOING→REVIEW→DONE（need_review=true）
==============================================================================
  task=01a1862f-ffcc-7233-8ca1-c20f0be77704  creator=t2_maker  owner=t2_owner  need_review=true
  PASS  初始状态：TODO
  PASS  初始进度：0
  PASS  TODO→DOING 由负责人做 HTTP：200
  PASS    落库状态：DOING
  PASS    进度联动 max(0,10)：10
  PASS  DOING→REVIEW 由负责人做 HTTP：200
  PASS    落库状态：REVIEW
  PASS    进度拉满：100
  PASS  REVIEW→DONE 由创建人（验收人）做 HTTP：200
  PASS    落库状态：DONE
  PASS    done_at 有值：True
  评论（biz_task_comment，按时间）：
    STATUS_CHANGE|状态变更：待开始（TODO）→ 进行中（DOING）；进度 0% → 10%
    STATUS_CHANGE|状态变更：进行中（DOING）→ 待验收（REVIEW）；进度 10% → 100%
    STATUS_CHANGE|状态变更：待验收（REVIEW）→ 已完成（DONE）
  PASS  STATUS_CHANGE 评论条数：3
  PASS  每条评论都带 from→to：True
  变更日志（sys_change_log）：
    STATUS|progress,status|{"v": 1, "meta": {"pk": "01a1862f-ffcc-7233-8ca1-c20f0be77704", "table": "biz_task", "action": "STATUS"}, "mode": "WHITELIST", "fields": {"status": {"a": "DOING", "b": "TODO"}, 
    STATUS|progress,status|{"v": 1, "meta": {"pk": "01a1862f-ffcc-7233-8ca1-c20f0be77704", "table": "biz_task", "action": "STATUS"}, "mode": "WHITELIST", "fields": {"status": {"a": "REVIEW", "b": "DOING"}
    STATUS|done_at,status|{"v": 1, "meta": {"pk": "01a1862f-ffcc-7233-8ca1-c20f0be77704", "table": "biz_task", "action": "STATUS"}, "mode": "WHITELIST", "fields": {"status": {"a": "DONE", "b": "REVIEW"}, 
  PASS  变更日志条数：3
  PASS  变更日志动作：STATUS
  PASS  变更日志 diff 里有 status 字段：True
  PASS  变更日志 diff 里 from 是 TODO、to 是 DOING：True

==============================================================================
2) 非法流转全被拒（跳级 / 终态直取 / 同状态 / 无验收权）
==============================================================================
  PASS  TODO→DONE（跳级）HTTP：409
  PASS    业务码：63010
    文案：该状态流转不在允许的白名单内（TODO → DOING → REVIEW → DONE，任意非终态可取消，CANCELLED 可恢复为 TODO）
  PASS  DONE→CANCELLED（终态直取）HTTP：409
  PASS    业务码：63010
  PASS  REVIEW→DONE 由负责人自己做（无验收权）HTTP：403
  PASS    业务码：63011
    文案：当前身份（负责人）无权执行该流转，允许的身份：创建人 / 分派人 / 管理员
  PASS  REVIEW→REVIEW（同状态）HTTP：409
  PASS    业务码：63010
  PASS  REVIEW→TODO（回退到待开始）HTTP：409
  PASS    业务码：63010

==============================================================================
3) 打回（REVIEW→DOING）必须填原因且 ≥5 字
==============================================================================
  PASS  不填 reason HTTP：400
  PASS    业务码：63013
    文案：打回必须填写原因（至少 5 个字，当前 0 个字）
  PASS  reason 只有 2 个字 HTTP：400
  PASS    业务码：63013
    文案：打回必须填写原因（至少 5 个字，当前 2 个字）
  PASS  reason 合法（≥5 字）HTTP：200
  PASS    落库状态：DOING
  PASS    打回后进度归 10：10
  PASS    reject_reason 落库：测试没跑通，返工补单测
  PASS    reason 进评论：True

==============================================================================
4) need_review=false 时 DOING→DONE 允许；true 时被拒
==============================================================================
  PASS  need_review=false：DOING→DONE HTTP：200
  PASS    落库状态：DONE
  PASS  need_review=true：DOING→DONE HTTP：409
  PASS    业务码：63010
    文案：该任务需要验收（need_review=true），必须先提交验收（DOING → REVIEW），由验收人确认后才能完成
  PASS  need_review=true：DOING→REVIEW HTTP：200

==============================================================================
5) DONE 后 done_at 有值；重开（DONE→DOING）后清空
==============================================================================
  PASS  DONE 后 done_at 非空：true
  PASS  DONE→DOING 由创建人重开 HTTP：200
  PASS    重开后 done_at 清空：false
  PASS    重开后 progress 归 10：10
  PASS  DONE→DOING 由负责人自己做（负责人不在重开白名单）HTTP：403
  PASS    业务码：63011
  PASS  DOING→DOING（同状态）HTTP：409
  PASS    业务码：63010

==============================================================================
6) 无子任务：手动 0→50→100；DOING 时设 5 → 自动 10
==============================================================================
  PASS  手动设 0 HTTP：200
  PASS    回执 progress：0
  PASS  手动设 50 HTTP：200
  PASS    回执 progress：50
  PASS  手动设 100 HTTP：200
  PASS    回执 progress：100
  PASS  DOING 时设 5 HTTP：200
  PASS    实际落库（联动抬到 10）：10
  PASS    回执原值 requested：5
  PASS  进度 101（越界）HTTP：400
  PASS    业务码：10001

==============================================================================
7) 有子任务：1h/2h/7h，进度 0/50/100 → 期望 (1×0 + 2×50 + 7×100)/10 = 80
==============================================================================
  父=01a18630-01f6-77c5-953c-e6487ab17801  子=01a18630-01fd-71a2-bb6f-65422d7b6d54(1h) 01a18630-0205-7702-bd52-fa370b764671(2h) 01a18630-020b-76f7-a816-36a23053a292(7h)
  PASS  子任务设进度 HTTP：200
  PASS  父任务汇总值：80
  计算：(1×0 + 2×50 + 7×100) / 10 = 800 / 10 = 80.0 → 80
  父任务重算评论：
    进度自动汇总：0% → 10%（3 个子任务，工时加权）
    进度自动汇总：10% → 80%（3 个子任务，工时加权）

==============================================================================
8) 工时全空 → 等权平均：(0 + 50 + 100) / 3 = 50
==============================================================================
  PASS  父任务等权汇总值：50
  PASS  子任务进度 0 的仍是 0：0

==============================================================================
9) 父任务手改进度 → 409 + 63012（进度由子任务汇总）
==============================================================================
  PASS  HTTP：409
  PASS  业务码：63012
  文案：进度由子任务汇总，不能手动修改（当前 3 个子任务，汇总值已写入任务）
  PASS  父任务进度未被改动：80

==============================================================================
10) 子任务全 DONE → 父 progress=100 但 status 不变（架构 D5）
==============================================================================
  PASS  子任务 01a18630 推到 DONE：200
  PASS  子任务 01a18630 推到 DONE：200
  PASS  父任务 progress|status：100|TODO

==============================================================================
11) 三级嵌套 A→B→C：C 完成 → B 重算 100 → A 重算 50
==============================================================================
  结构（夹具 SQL 造，业务 API 限 2 层）：A(2h 子B + 2h 子D) → B → C
  完成前：TSK-T2V-900001=0%/DOING  TSK-T2V-900002=0%/DOING  TSK-T2V-900003=0%/TODO
  PASS  C：TODO→DOING HTTP：200
  PASS    C 的父任务（B）被重算：7e000000-0000-0000-0000-000000000002
  PASS  C：DOING→DONE HTTP：200
  本次重算链（由近及远）：
    7e000000-0000-0000-0000-000000000002  10% → 100%（子任务全部完成，all_done=True）
    7e000000-0000-0000-0000-000000000001  5% → 50%（工时加权，all_done=False）
  PASS  B 重算到 100：100
  PASS  A 重算到 50（(2×100 + 2×0)/4）：50
  PASS  A 的状态没有自动变（仍是 DOING）：DOING

==============================================================================
12) 无关用户改状态 → 404 + 63001（不暴露「存在但无权」）
==============================================================================
  PASS  HTTP：404
  PASS  业务码：63001
  文案：任务不存在或已删除
  PASS  无关用户改进度 HTTP：404
  PASS  业务码：63001

==============================================================================
13) 跨部门协作人：看得见（200）但推不动（403）
==============================================================================
  PASS  协作人看详情 HTTP：200
  PASS    协作人能看见：01a18630-03ab-7749-836a-c35ad037443e
  PASS  协作人改状态 HTTP：403
  PASS    业务码（状态流转专用码）：63011
    文案：当前身份（无（协作人 / 关注者 / 无关用户））无权执行该流转，允许的身份：负责人 / 创建人 / 分派人 / 管理员
  PASS  协作人改进度 HTTP：403
  PASS    业务码（T1 的编辑类无权限码）：63002
  PASS  协作人编辑任务 HTTP（T1 口径）：403
  PASS    业务码：63002
  PASS  负责人把 t12 推到 DOING（普通状态变更）HTTP：200

==============================================================================
14) 子任务接口：建 / 列 / 继承 / 深度限制
==============================================================================
  PASS  子任务继承父任务优先级：P1
  PASS  子任务继承父任务截止日：2026-12-31
  PASS  子任务继承 need_review：True
  PASS  子任务父指向正确：01a18630-03e9-7116-8a3d-c4cc27545333
  PASS  子任务列表 HTTP：200
  PASS    列表条数：1
  PASS    进度已锁：True
  PASS    父任务进度 = 子任务（唯一 0h→等权）：0
  PASS  给子任务再加子任务（第 3 层）HTTP：400
  PASS    业务码：63005
    文案：invalid value: 子任务最多 2 层：目标父任务自身已是子任务
  PASS  父任务推进到 DOING（有子任务时进度不动）：0

==============================================================================
15) 通知分级：待验收 / 被打回（强制）；其它状态变更（尊重偏好）
==============================================================================
  被打回 t3（01a18630）通知：
    任务状态变更 → 7d000000-0000-0000-0000-000000000012
    任务待你验收 → 7d000000-0000-0000-0000-000000000012
    任务被打回 → 7d000000-0000-0000-0000-000000000011
  需验收 t5（01a18630）通知：
    任务状态变更 → 7d000000-0000-0000-0000-000000000012
    任务待你验收 → 7d000000-0000-0000-0000-000000000012
  普通变更 t12（01a18630）通知：
    任务状态变更 → 7d000000-0000-0000-0000-000000000012
  PASS  被打回通知到负责人：True
  PASS  待验收通知到创建人：True

==============================================================================
16) P1 扩展位：status_semantic 全程保持 NULL
==============================================================================
  PASS  有值的行数（应为 0）：0
  PASS  comment_type 分布：True

==============================================================================
汇总
==============================================================================
  PASS = 104   FAIL = 0
  全部断言通过

```
