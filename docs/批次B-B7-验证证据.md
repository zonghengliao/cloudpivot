# 批次 B · B7 验证证据 —— 角色互斥（SoD）：对称索引 + 三校验点 + 存量扫描

- 任务卡：`t_f32d9396`【批次B·B7】角色互斥SoD：对称索引+三校验点+存量扫描
- 依据：`docs/批次B-技术架构.md` §4（角色互斥）+ §5.1/§5.2（权限码 / V0068）+ §7 B7 行、PRD 互斥章节
- 代码基线：`f543abc`（B6 交付后的 main）+ 本卡提交 `f6f4aaf`
- 验证机：`172.18.10.144`
  - **真库集成测试**用独立库 `cloudpivot_b7v`（不污染 `cloudpivot_test`，避免与并行的卡抢数据）
  - **真机 HTTP 验收**用同一实例（profile=test / `cloudpivot_b7v` / `127.0.0.1:8027`）
  - **V0068 授权效果**在共享验证库 `cloudpivot_test`（`127.0.0.1:8025`）上实测

---

## 1. 交付物清单

| 文件 | 行数 | 职责 |
|---|---|---|
| `backend/src/business/mutex/mod.rs` | 81 | 模块文档：三校验点、与 delegation cap 的协同、超管不短路、权限码说明 |
| `backend/src/business/mutex/model.rs` | 715 | `sys_role_mutex` 读写；**`HELD_ROLE_SQL` = 「有效持有」的唯一权威口径** |
| `backend/src/business/mutex/guard.rs` | 486 | 三校验点共用守卫 + 文案纯函数（可无库断言） |
| `backend/src/business/mutex/checker.rs` | 147 | 单用户体检 / 单对持有者 / 全量扫描 |
| `backend/src/business/mutex/handler.rs` | 660 | 6 个 HTTP 接口 + 审计 + 错误翻译 |
| `backend/src/business/mutex/dto.rs` | 237 | 请求 / 响应 DTO（ID 一律 `String`，A/B 已规范化） |
| `backend/migrations/business/V0068__role_mutex_conflict_perm.{up,down}.sql` | 78 + 28 | 冲突查询权限补授（Q2 默认）+ 迁移期自检 |
| `backend/tests/batchB_b7_mutex.rs` | 449 | 真库集成测试（缺 `B7_DB_URL` 自动 SKIP） |

底座最小接线（`git diff --stat`：**3 文件 / +23 −1 行**，只加校验、不改任何既有语义）：

| 文件 | 改动 |
|---|---|
| `backend/src/business/mod.rs` | `pub mod mutex;` + 文档表 1 行 |
| `backend/src/system/user/handler.rs` | `create` 预生成 `user_id` 后插互斥校验；`assign_roles` 在 cap 之后插 1 行 |
| `backend/src/system/role/handler.rs` | `update` 在写库前，对「停用 → 启用」跃迁复核持有者 |

> `system/dept`、数据权限、脱敏（`business/permission`、`business/masking`、`business/shared`）**一行未动**。

---

## 2. 单测：模块内 23 个 + 真库集成 1 个

### 2.1 模块内单测（`cargo test business::mutex`）

```
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 339 filtered out
```

| 分组 | 用例 |
|---|---|
| 规范化（模型侧） | `normalize_keeps_ordered_row` / `normalize_swaps_reversed_row` / `normalize_is_stable_when_codes_equal` |
| 对称 / 反自反 | `normalize_pair_is_symmetric` / `self_pair_is_rejected`（400 + **61012** + 逐字文案）/ `distinct_pair_is_accepted` |
| 校验点 1 文案（正反 4 种形态） | `single_message_names_granted_side_and_held_side` / `single_message_flips_when_granted_side_is_a`（对称性）/ `single_message_handles_same_batch_grant`（**D3 同次授予**）/ `single_message_reports_existing_violation`（历史双持原样重提）|
| 多组命中 | `multi_message_lists_group_count_and_pairs` / `no_hits_yields_empty_message` |
| 校验点 3 文案 | `holders_message_contains_total_roles_and_sample` / `holders_message_omits_empty_sample` |
| 校验点 2 文案 | `role_enable_message_names_role_and_pairs` / `dedup_take_limits_and_dedups` |
| 扫描汇总 | `summarize_empty_scan` / `summarize_counts_holders_by_total_not_by_sample` / `summarize_ignores_negative_total` |
| 请求校验 / 错误翻译 | `reason_is_trimmed_and_blank_rejected` / `reason_length_boundary`（255/256）/ `duplicate_error_mentions_both_roles_and_directionless` / **`unique_violation_maps_to_conflict_not_500`** |

### 2.2 真库集成测试（`tests/batchB_b7_mutex.rs`）

```
$ export B7_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_b7v"
$ CARGO_TARGET_DIR=/var/tmp/b7-target cargo test --test batchB_b7_mutex -- --nocapture --test-threads=1

running 1 test
test b7_real_database_acceptance ... B7 已连接验证库

================ B7 真库验收：对称索引 + 三校验点 ================
  ① 反自反：guard=400/61012，库层=CHECK 拒绝（check constraint violated (constraint `ck_sys_role_mutex_distinct`)）
  ② 对称插入：首插成功；反向 UniqueViolation；表内 1 行；反向查询命中同一条
  ③-1 正例：仅授已持有的「采购员」→ 通过
  ③-1 反例：已持有「采购员」再授「付款审批」→ 409 角色「付款审批」与用户已持有的「采购员」互斥：不得同时持有采购与付款权限。请先解除其中一个角色。
  ③-2 对称性：先持「付款审批」再授「采购员」→ 409 同样被拒 角色「采购员」与用户已持有的「付款审批」互斥：不得同时持有采购与付款权限。请先解除其中一个角色。
  ③-3 D3：同次授予 A+B → 409 本次授权同时包含互斥角色「采购员」与「付款审批」：不得同时持有采购与付款权限。请先移除其中一个角色。
  ③-4 正例：无角色用户授单个角色、过期绑定用户授对侧角色 → 均通过
  ④ 校验点 3：存量双持 → 409 无法建立互斥关系：1 个用户同时持有「采购员」与「付款审批」，请先解除他们的其中一个角色（如 b7_u_both）。
  ⑤ 体检/扫描：双持用户命中 1 组；扫描 1 条规则 → 1 组违规 / 1 人次
  ④ 解绑后：配置校验恢复通过（先解绑再配置）
  ⑥ 启停：停用后不判定、恢复后重新生效
  ⑦ 校验点 2：启用「停用角色」→ 409 启用角色「停用角色」后将使 1 个用户同时持有互斥角色（停用角色×采购员），请先解除他们的其中一个角色（如 b7_u_off）。

================ B7 真库验收全部通过 ================
ok
```

覆盖到的关键断言（本次实测逐条为真）：

1. **对称插入**：`insert(A,B)` 成功 → `insert(B,A)` 报 `UniqueViolation`（`uk_sys_role_mutex_pair`）→ 该对在表里**只有 1 行**；`find_active_pair(B,A)` 仍命中同一条；
2. **落库规范化**：`role_a_id` / `role_b_id` 按 **UUID 字节序**（与 `LEAST/GREATEST` 索引同口径）；下发时另按 `role_code` 字典序（两条不同的顺序，各有断言，避免"以为是一回事"）；
3. **反自反**：应用层 400/61012 **且**库层 `ck_sys_role_mutex_distinct` 也拒（两层都不放过）；
4. **D3 防绕过**：无既有角色的用户，同一次请求同时授予 A+B → 命中（这是"只比本次 vs 已持有"会漏掉的用例）；
5. **口径一致性**：过期绑定不算持有；**停用角色**不算持有（因此允许为它建规则）——但**启用**该角色时被校验点 2 拦住，堵掉「先停用 → 授完 → 再启用」；
6. **启停**：停用后 `find_active_pair` = None、授予不再命中；恢复后重新生效。

---

## 3. `cargo test` 全绿（基线 349 → 373，一个不少）

```
$ cd /opt/cloudpivot/backend && cargo test
test result: ok. 362 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;   ← lib
test result: ok. 1 passed;   0 failed; … 000 filtered out;                      ← tests/batchB_b2_scope
test result: ok. 1 passed;   0 failed; …                                        ← tests/batchB_b7_mutex
test result: ok. 7 passed;   0 failed; …                                        ← tests/impexp_worker
test result: ok. 2 passed;   0 failed; …                                        ← tests/s3_minio
test result: ok. 0 passed;   0 failed; 6 ignored; …                             ← doctests
TOTAL PASSED: 373
```

| 口径 | 数量 |
|---|---|
| 开工前（`f543abc`） | **349** |
| 收尾（本卡 HEAD） | **373**（+23 模块内单测 +1 真库集成测试，0 failed / 0 ignored 新增） |

---

## 4. 门禁仍全通过（`bash scripts/check.sh`）

```
$ bash scripts/check.sh > /var/tmp/b7check.log 2>&1; echo "EXIT=$?"
EXIT=0
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
==> 后端：cargo check
==> 前端：pnpm typecheck
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
  PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> 全部检查通过
```

- 闸①：`sys_role_mutex` 是全局元数据（不在 `SCOPED_TABLES`），分页手写 `COUNT(*) + LIMIT/OFFSET`，**没有**出现 `paginate_with_params`；
- 闸②：`business/mutex/model.rs` 里出现 `FROM sys_user`，已写**带理由的** `SCOPE-EXEMPT`（诊断查询按 `user_id` / 角色对收口；反向注入数据范围会让管理员看不到自己该处理的冲突用户，"先解绑再配置"的流程会断掉）。

---

## 5. 真机 5 组实测（全部走真实 HTTP 接口）

脚本：`/root/b7src/b7_accept.py`（可重复执行，开头自动复位夹具）。实例：`127.0.0.1:8027`，库 `cloudpivot_b7v`。

```
[admin] me HTTP=200 username=admin

================ 复位夹具（可重复执行） ================
    复位后：互斥规则 0 行、夹具用户 0 个、夹具角色 4 个（角色保留，状态置回启用）

================ 准备夹具（角色 / 用户） ================
[role] 已存在 B7M_PURCHASER → 01a18373-629c-727f-877f-6b2bbddb8a22
[role] 已存在 B7M_PAY_APPROVER → 01a18373-62a3-77db-b492-fd9a2aaeaf4d
[role] 已存在 B7M_KEEPER → 01a18373-62a7-7305-be63-575cf93f13a1
[role] 已存在 B7M_AUDITOR → 01a18373-62aa-73b5-b9cd-3d5c3525b184
[user] + b7m_u1 roles=['…8a22'] → 01a18373-9fd5-70eb-be15-e60dbc42ee3a
[user] + b7m_u2 roles=['…af4d'] → 01a18373-a0e4-7110-a23c-6ad8accf9ffc
[user] + b7m_u3 roles=['…13a1'] → 01a18373-a1f1-7670-93d1-a1f0d3287f98

================ 组1：给「采购员」与「付款审批」建互斥 ================
    POST /api/business/mutex → HTTP=200 code=0 msg=ok
    [PASS] 组1 建互斥成功 — id=01a18373-a2fb-722b-bd27-7655f6596736
    规范化下发：role_a=B7M_PAY_APPROVER(付款审批) role_b=B7M_PURCHASER(采购员) enabled=True
    [PASS] 组1 规则 A/B 按 role_code 字典序规范化 — B7M_PAY_APPROVER < B7M_PURCHASER

================ 组2：已持有「采购员」的用户再加「付款审批」 ================
    PUT /api/system/user/01a18373-9fd5-…/roles → HTTP=409 code=61009
    错误文案：角色「付款审批」与用户已持有的「采购员」互斥：不得同时持有采购与付款权限。请先解除其中一个角色。
    [PASS] 组2 被拒 409/61009
    [PASS] 组2 文案含两个角色名
    [PASS] 组2 sys_user_role 未变化（仍只有采购员） — roles=['01a18373-629c-727f-877f-6b2bbddb8a22']

================ 组3：反向（先有「付款审批」再加「采购员」） ================
    PUT /api/system/user/01a18373-a0e4-…/roles → HTTP=409 code=61009
    错误文案：角色「采购员」与用户已持有的「付款审批」互斥：不得同时持有采购与付款权限。请先解除其中一个角色。
    [PASS] 组3 对称被拒 409/61009
    [PASS] 组3 文案方向随持有侧互换
    [PASS] 组3 sys_user_role 未变化（仍只有付款审批） — roles=['01a18373-62a3-77db-b492-fd9a2aaeaf4d']

================ 附加：同次授予两个互斥角色（D3 防绕过） ================
    POST /api/system/user（role_ids=[采购员,付款审批]）→ HTTP=409 code=61009
    错误文案：本次授权同时包含互斥角色「采购员」与「付款审批」：不得同时持有采购与付款权限。请先移除其中一个角色。
    [PASS] D3 建号即双持被拒 409/61009
    [PASS] D3 文案说明「同时包含互斥角色」

================ 组4：已有违规用户时建互斥规则 ================
    [db] 已直接写入 sys_user_role：01a18373-a1f1-… → 稽核（模拟历史双持）
    POST /api/business/mutex（库管,稽核）→ HTTP=409 code=61010
    错误文案：无法建立互斥关系：1 个用户同时持有「库管」与「稽核」，请先解除他们的其中一个角色（如 b7m_u3）。
    [PASS] 组4 阻止配置 409/61010
    [PASS] 组4 文案含用户数与示例用户名
    POST /api/business/mutex?force=true → HTTP=200 code=0
    [PASS] 组4 force=true 可落库（运维显式接受历史违规）
    生效规则数：1 → 2

================ 组5：scan 存量违规清单 ================
    POST /api/business/mutex/scan → HTTP=200 code=0
    summary: scanned_pairs=2 violated_pairs=1 violated_holders=1
    - 稽核×库管 total=1 sample=['b7m_u3'] reason=库管与稽核不得同一人
    [PASS] 组5 scan 返回存量违规清单
    [PASS] 组5 清单包含「库管×稽核」且列出 b7m_u3

================ 附加：单用户体检 / 分页 / 重复 / 非法参数 ================
    GET /business/mutex/conflict?user_id=u3 → HTTP=200 conflicts=[('库管', '稽核')]
    [PASS] 体检：违规用户返回 1 组互斥
    GET /business/mutex/conflict?user_id=u1 → HTTP=200 conflicts=0
    [PASS] 体检：合规用户返回 0 组
    GET /business/mutex/page → HTTP=200 total=2 list=2
    [PASS] 分页：total ≥ 2
    反向重复建互斥（付款审批,采购员）→ HTTP=409 code=61011
    [PASS] 反向重复：409/61011（不是 500）
    自反（采购员,采购员）→ HTTP=400 code=61012 msg=不能建立角色与自身的互斥关系。
    [PASS] 自反：400/61012
    空理由 → HTTP=400 code=10001 msg=请填写互斥理由（用于合规留痕）。
    [PASS] 空理由：400/10001

================ 附加：校验点 2（启用角色会让互斥成立） ================
    PUT /system/role（停用稽核）→ HTTP=200 code=0
    [PASS] 停用稽核角色成功（放松约束不拦）
    停用后体检 b7m_u3 → conflicts=0
    [PASS] 停用角色不计入有效持有（体检 0 组）
    PUT /system/role（启用稽核）→ HTTP=409 code=61009
    错误文案：启用角色「稽核」后将使 1 个用户同时持有互斥角色（稽核×库管），请先解除他们的其中一个角色（如 b7m_u3）。
    [PASS] 校验点2：启用被拦 409/61009
    [PASS] 校验点2 文案含角色名与用户名

================ 清理：删互斥规则（软删） ================
    DELETE /business/mutex/01a18373-a2fb-… → HTTP=200 code=0
    DELETE /business/mutex/01a18373-a341-… → HTTP=200 code=0
    删除后生效规则数 → 0

================ 验收汇总 ================
PASS=27  FAIL=0
```

任务卡要求的 5 组逐条对应：

| 卡上要求 | 实测 | 结果 |
|---|---|---|
| 给「采购员」与「付款审批」建互斥 → 成功 | 组1 `POST /api/business/mutex` 200 | ✅ |
| 给已持有「采购员」的用户加「付款审批」→ 被拒（贴含角色名的文案） | 组2 409/61009，`sys_user_role` 未变 | ✅ |
| 反过来（先有 B 再加 A）→ 同样被拒（对称性） | 组3 409/61009，文案方向互换 | ✅ |
| 已有违规用户时建互斥规则 → 按产品决策表现 | 组4 409/61010（阻止配置）+ `?force=true` 显式口子 | ✅ |
| `scan` 返回存量违规清单 | 组5 `scanned_pairs=2 violated_pairs=1 violated_holders=1`，含用户名 | ✅ |

---

## 6. V0068 的真实效果（Q2 默认：给持 `system:user:role` 的角色）

在共享验证库 `cloudpivot_test` 上应用 V0068 后（`应用 1 个迁移`，version=68 success=t）：

```
    role_code    | can_grant_role | has_conflict_perm
-----------------+----------------+-------------------
 B5_DEPT         | t              | t
 B5_DEPT_CHILD   | t              | t
 B5_SELF         | t              | t
 TEST_USER_ADMIN | t              | t
 A5_ROLE_1..5    | f              | f
 SUPER_ADMIN     | f              | t     ← V0065 已单独授过（超管本身短路全通）
 TEST_VIEWER     | f              | f
```

- **4 个能授角色的角色全部拿到了 `system:role-mutex:conflict`**（否则非超管管理员点保存拿到 409 却打不开冲突清单，体验断裂）；
- 不能授角色的角色**一个都没拿到**（最小授权）；
- `SUPER_ADMIN` 在 `sys_menu` 侧的 `system:user:role` 载体为空（它靠 `role_code` 短路），所以 `can_grant_role=f`，但它的 conflict 权限来自 V0065，不受影响；
- V0068 自带迁移期自检 `DO` 块：节点缺失或仍有 grantor 角色没拿到授权 → 直接 `RAISE EXCEPTION`（宁可迁移期炸，也不要在生产上发现体验断裂）。down 脚本只收回**非超管**的该按钮。

权限码三方一致（注解 / 种子 / 错误码段）：`system:role-mutex:{list,create,delete,conflict}` 逐字来自 `V0065__seed_batchb_permissions`，本卡**不新增第 13 个码**；`PUT`（改理由 / 启停）复用 `:create`。

---

## 7. 与架构文档的差异 / 取舍（逐条说明，便于评审）

1. **接口路径按票面 `/api/business/mutex/*`**（架构 §4.6 写的是 `/system/role-mutex/*`）。票面是契约来源——本批红线是"只改 `business/`，不动 `system/` 地盘"，且 B5 的票面同样把 `system:data-scope:*` 改成 `business:permission:scope:*`。权限码仍沿用 §5.1 原文的 `system:role-mutex:*`（V0065 已种，三处逐字一致）。
2. **`PUT /api/business/mutex/{id}` 只改「理由 / 启停」**（票面如此）。`sys_role_mutex` 没有 `updated_at` / `status` 列，所以"启停"用 `deleted_at` 表达：`enabled=false` = 软删（停用），`enabled=true` = 清空（恢复启用）。恢复启用等同"重新收紧约束"，因此**同样跑校验点 3**（有存量双持就 409，除非 `?force=true`）。
3. **校验点 2 落在「角色停用 → 启用」跃迁**。互斥规则按**角色 ID** 定义，改 `role_code` / `role_name` 不改变互斥拓扑（库里一行都不动），因此"改了角色标识导致新的互斥关系成立"在这套数据模型下唯一可能的形态就是**休眠绑定变有效**。
4. **校验点 1 用 UNION 形态（架构 D9），因此「一次调用内把 A 换成互斥的 B」会被拒**（请求里同时出现 A、B）。这是有意保留：SoD 要求解除动作显式留痕，管理员分两步（先移除、再添加）即可，第二步不会再命中。
5. **超管不短路**（与 delegation cap 的刻意差异）：`ensure_no_mutex` 里没有 `is_super_admin → Ok(())`，代码注释与模块文档都写明了原因（互斥是数据完整性约束，不是操作者资格约束）。超管的出路是删除规则或先解绑。
6. **「有效持有」口径含 `sys_role.status = 1`**（与 `principal` 装载权限一致）。代价是：停用角色的双持不会被判违规 → 由校验点 2 在启用时拦住；收益是管理员不会被历史停用角色卡死到无法配置。

---

## 8. 交付指纹（HEAD == origin/main）

```
$ git -C /opt/cloudpivot log --oneline -3
<证据提交> docs(b7): 角色互斥验证证据
f6f4aaf feat(mutex): B7 角色互斥 —— 对称索引 + 三校验点 + 存量扫描
f543abc feat(masking): B6 列级脱敏 —— 7 种类型 + DTO 层应用 + 导出同步脱敏

$ git rev-parse HEAD            → <见下（收尾记录）>
$ git rev-parse origin/main     → 同 HEAD
$ git --git-dir=/opt/git/cloudpivot.git rev-parse main  → 同 HEAD
$ git status --short            → 空
```

（`<见下（收尾记录）>` 在证据提交后由 `Run History` 的 summary 与本节末尾的同步核对输出补齐。）
