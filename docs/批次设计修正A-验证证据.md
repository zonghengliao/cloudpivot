# 批次设计修正 A —— 验证证据（门禁补 biz_task / 任务附件 storage 策略 / 命名与权限码判据）

> 卡号：`t_105a294c`（【设计修正A】）。依据：`docs/云枢设计修正补丁-20261029.md`
> 修正 2 / 修正 4 / 修正 5 / 修正 6（老大已批「有问题的都修正」）。
> 环境：`172.18.10.144`，worktree `/var/tmp/designA`（分支 `fix/design-a`，基于 `origin/main` 的 `983adc5`），
> 独立 target `/var/tmp/designA-target`，验收实例 `127.0.0.1:8026` + 独立库 `cloudpivot_fixa_v107`。
> 所有数字均为**真实执行输出**，命令与结论一一对应。

---

## 一、修正 2：门禁清单补 `biz_task`（含「故意违反必须被抓到」的前后对比）

### 1.1 改了什么

`scripts/check.sh` 三处：

| # | 改动 | 理由 |
|---|---|---|
| ① | `BATCHB_SCOPED_TABLES` 补入 7 张任务表：`biz_task` `biz_task_comment` `biz_task_attachment` `biz_task_worklog`（卡面点名）+ `biz_task_member` `biz_task_dependency` `biz_task_status_def` | `biz_task_member` 在 `business/task/model.rs` 被**直接查询** 2 处（「我参与」的 id 集合、协作人列表）；`dependency` / `status_def` T1 只建表还没有 Rust 查询，但它们按 `task_id` 挂在任务下，提前列入避免「新代码没有门禁条目」的静默缺口 |
| ② | 闸② 新增**扫描范围自检**：硬断言 `backend/src/{business/task,business,system,support,shared}` 都存在且各有 `.rs` 文件 | 扫描根本来就是整个 `backend/src`（`business/task/` 天然在范围内），但「天然在范围内」会随目录搬家静默失效 —— 写成断言，缺覆盖时门禁自己红 |
| ③ | 闸② 判定**只看真实代码行**（先剥整行注释），`FROM <表>` 与 `scope::` 两侧都剥；`SCOPE-EXEMPT` 仍整文件搜 | 修两个实测坑：注释里举例的 SQL 会把文件拖进门禁（`business/shared/mod.rs` 的 `//!` 示例）；注释里写一句 `scope::` 就能让整文件被判成已接入 = **假绿**（skill cailu-dev §9.15-6）。顺带把「同一文件命中多张表」按文件聚合，不再把 1 个文件报成 N 处 |

### 1.2 故意违反 → 必须被抓到（失败态）

在 `backend/src/business/task/` 放一个**故意不接 scope** 的临时文件（`tmp_gate_probe.rs`，未被 `mod.rs` 声明，
只含一句 `SELECT count(*) FROM biz_task_comment …`），跑 `bash scripts/check.sh`：

```
$ bash scripts/check.sh            # EXIT=1
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
    白名单：business/shared/scope.rs（+ 注释行不计）
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
    登记表：sys_user sys_dept sys_import_task sys_export_task sys_notice sys_file operation_logs audit_logs auth_logs security_logs biz_task biz_task_comment biz_task_attachment biz_task_worklog biz_task_member biz_task_dependency biz_task_status_def
  PASS：扫描范围已覆盖 backend/src/{business/task,business,system,support,shared}
    FAIL: business/task/tmp_gate_probe.rs 查询 biz_task biz_task_comment 未接入 scope:: 且无 SCOPE-EXEMPT
  FAIL：1 个文件（按文件计；B0 实测基线 21 处是 file×table 口径，仅作历史参考）
==> 批次B 门禁汇总：闸①=0 处  闸②=1 处  （B0 基线 20 / 21）
==> 批次B 门禁：1 条未通过。修法（二选一）：
    ① 分页接口改成 scope::paginate；单条/导出查询用 scope::filter_for
    ② 确实不需要数据权限的查询，在函数上方写一行 // SCOPE-EXEMPT: <理由>
```

**结论**：闸② 在 `business/task/` 里生效，且**点名到文件**（`business/task/tmp_gate_probe.rs`）。

> ⚠️ 这一轮之前还踩到一次「门禁假绿」：第一版探针在**注释里**写了 `scope::` 与 `SCOPE-EXEMPT` 两个词，
> 闸② 直接把该文件判成「已接入」→ 输出 `PASS：0 处`、`EXIT=0`。这正是上面改动 ③ 的由来：
> 判据必须只看代码行，否则任何人（包括 AI）在注释里写一句话就能让门禁永久失效。

### 1.3 删掉临时文件 → 必须全绿

```
$ rm -f backend/src/business/task/tmp_gate_probe.rs && bash scripts/check.sh   # EXIT=0
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
==> 后端：cargo check
==> 前端：pnpm typecheck
> vue-tsc --noEmit
==> 批次B 闸①：… PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②： … PASS：扫描范围已覆盖 backend/src/{business/task,business,system,support,shared}
                 PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> C7 G1：只有 business/changelog/ 能碰 sys_change_log（防止别处偷偷 INSERT）  PASS：0 处
==> C7 G2：changelog 模块内禁止自开事务（db() 非注释命中必须为 0）              PASS：0 处
==> C7 G3：record 的签名必须带 &impl SqlExecutor                              PASS
==> C7 G4：不得存在『无 executor 版本』的 record 函数                          PASS
==> C7 G5：每个 record/record_in 调用点必须在 transaction 闭包内且传 &tx
G5 PASS：19 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx（扫了 155 个文件）
==> C7 G6：按 id 查 sys_change_log 必须同时带分区键 created_at（R14）            PASS：0 处
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> C7 门禁：全部通过
==> 偏好字典与色板一致性（V0091 ui.palette oneof ↔ style/palettes.ts）
  PASS：22 套色板与 V0091 的 ui.palette oneof 逐字一致
==> 全部检查通过
```

> 红线遵守：**没有**给任何 `biz_task*` 文件加 `SCOPE-EXEMPT`；`business/task/model.rs` 现状本来就是
> 「已接 `scope::`」（`TASK_TARGET` + `scope::paginate` + `filter_for`），门禁因此真绿而不是豁免绿。

---

## 二、修正 4：V0107 任务附件存储策略

### 2.1 新增迁移

`backend/migrations/business/V0107__storage_task_policies.{up,down}.sql`

| 分类 | 用途 | max_size | 白名单 | 直传 |
|---|---|---|---|---|
| `task` | 人类上传的任务附件 | 50 MB | jpg,jpeg,png,gif,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,md,csv,json,zip,7z,rar | 开 |
| `task_ai` | **AI 产出的文件** | 100 MB | jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log | **关** |

**「硬禁止」怎么落地的（两层）**

1. **迁移层**：`sys_storage_policy` 只有白名单列、**没有黑名单列**（V0086 已解释过同一件事），
   所以 up 文件末尾带一个 `DO $$ … $$` 自检块：`task_ai`/`task` 的白名单里只要出现
   `exe,sh,bat,ps1,so,dll,jar,py,js,vbs,msi,cmd,com,scr,psm1,wasm,apk` 任何一个 → `RAISE EXCEPTION`，
   并核对 `max_size_mb`（50 / 100）与策略总数（≥8）。
2. **运行层**：`business/storage/model.rs` 新增 `FORBIDDEN_EXECUTABLE_EXTENSIONS`
   （= 设计文档两张硬禁止列的**并集**，另补若干同类），`check_upload` **先查黑名单再查白名单**：
   即便将来有人手工把 `sh` 写进某条策略的白名单，上传口子仍然是关的。`validate_policy_fields`
   在**保存策略**时也会拒绝含可执行扩展名的白名单（配置事故在写入侧就拦住）。

> 为什么 `task_ai` 关直传：`direct_upload=TRUE` 允许浏览器拿预签名 URL 绕过服务端直接 PUT。
> AI 产出物是「服务端写、人来看」的单向流，没有任何需要浏览器直传的场景 —— 关掉等于少一条可绕的边。

### 2.2 空库 up / down / up（真库，`cloudpivot_fixa_v107`）

```
$ python3 scripts/verify/fixa_storage_policy.py migrate
  PASS  策略总条数 = 8（原 6 + 本卡 2）            期望=8  实际=8
  PASS  含 task 与 task_ai                        期望=True 实际=(1, 1)
  PASS  task.max_size_mb = 50                     期望=50 实际=50
  PASS  task_ai.max_size_mb = 100                 期望=100 实际=100
  PASS  task 允许扩展名与设计逐字一致              实际=jpg,jpeg,png,gif,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,md,csv,json,zip,7z,rar
  PASS  task_ai 允许扩展名与设计逐字一致           实际=jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log
  PASS  task 开启浏览器直传                        实际=t
  PASS  task_ai 关闭浏览器直传（AI 产出必过服务端） 实际=f
  PASS  down 执行成功                              实际=rc 0
  PASS  down 后策略 8 → 6 条                       期望=6  实际=6
  PASS  down 后 task / task_ai 均不存在             实际=无
  PASS  down 后重新 up 成功                        实际=rc 0
  PASS  up 后策略 6 → 8 条                         期望=8  实际=8
  PASS  重复 up 不报错（ON CONFLICT 幂等）          实际=rc 0
  PASS  重复 up 不产生重复行（仍 8 条）              期望=8  实际=8
  PASS  迁移自检块打出通过 NOTICE                   实际=NOTICE:  V0107 自检通过：task/task_ai 就位，策略总数 8 条
  PASS  白名单被污染后重放 up → 迁移自检报错（不静默通过）
        期望=非 0 退出 + 错误含『硬禁止』
        实际=rc=3 err=ERROR:  V0107 自检失败：分类 task_ai 的白名单含可执行扩展名 sh（硬禁止）
        被污染的白名单：jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log,sh
  PASS  恢复后白名单回到设计值（8 条策略不变）
```

> 「空库」= `run_instance.sh` 起隔离实例时对空库 `cloudpivot_fixa_v107` 把**整条迁移链**跑到 V0107，
> 随后用 `psql -f` 直接跑 down/up（不走 `Migrator::rollback` —— 种子迁移会卡住回滚，见 skill §9.14-1）。

### 2.3 5 条策略实测（真机 HTTP multipart，实例 `127.0.0.1:8026`）

```
$ python3 scripts/verify/fixa_storage_policy.py upload
  PASS  .sh 上传 task_ai → 拒绝 400 / 60004
        实际=400 / code=60004 msg=禁止上传可执行/脚本文件 .sh：该类型在硬禁止黑名单内，任何分类都不放行
  PASS  train.py 上传 task_ai → 拒绝 400 / 60004        实际=400 / code=60004
  PASS  run.js 上传 task_ai → 拒绝 400 / 60004          实际=400 / code=60004
  PASS  .md 上传 task_ai → 通过 200 / code=0 且有 file_id
        实际=200 / code=0 file_id=01a18616-0c44-712c-91e5-0a3a5f108ccc
  PASS  sys_file 落库分类 = task_ai                     实际=['task_ai', '54', 'ai-report.md']
  PASS  task_ai 文件可下载且内容一致                     实际=200 + 54 字节
        download_url=http://127.0.0.1:8026/api/business/storage/download/01a18616-0c44-712c-91e5-0a3a5f108ccc
  PASS  .exe 上传 task → 拒绝 400 / 60004               实际=400 / code=60004 msg=禁止上传可执行/脚本文件 .exe：…
  PASS  51 MB 上传 task → 拒绝 422 / 60003              实际=422 / code=60003 msg=文件大小 51.00 MB 超过该分类大小上限 50 MB
  PASS  49 MB 上传 task → 通过 200 / code=0             实际=200 / code=0 file_id=01a18616-161a-7375-9f7d-e30f418d56a4
  PASS  49 MB 文件落库 category=task 且 size 一致        实际=['task', '51380224']   （51380224 = 49 MB）
  PASS  策略总数 = 8                                    期望=8  实际=8
```

策略表最终状态（8 条，原 6 条一行未改）：

```
category       max_mb  direct  sort  allowed_extensions
attachment        100  t         10  jpg,jpeg,png,gif,bmp,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,csv,md,zip,rar,7z,gz,tar,mp3,wav,mp4,mov
avatar             10  t         20  jpg,jpeg,png,gif,webp
document          200  t         30  pdf,doc,docx,xls,xlsx,ppt,pptx,txt,csv,md,json,xml
import             50  f         40  xlsx,xls,csv
template           20  f         50  xlsx,csv
announcement       50  t         60  jpg,jpeg,png,pdf,docx,xlsx,zip
task               50  t         70  jpg,jpeg,png,gif,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,md,csv,json,zip,7z,rar
task_ai           100  f         80  jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log
```

### 2.4 单测（新增 4 条，`business/storage/model.rs`）

| 测试 | 覆盖 |
|---|---|
| `forbidden_list_covers_design_document` | **锁住硬禁止清单**：exe/sh/bat/ps1/so/dll/jar/py/js 缺一个就红；并断言 pdf/md/csv/json/zip/log/png/docx 不被误伤 |
| `forbidden_extension_blocked_even_if_whitelisted` | 白名单里被写进 `sh`/`py`，`check_upload` 仍然 `ExtensionForbidden`（400/60004），同策略下 `.md` 照常通过 |
| `task_and_task_ai_policies_behave_as_designed` | `task_ai` 收 md/log、拒 sh/py/js/jar/so、拒 docx（白名单更严）、未开直传；`task` 50 MB 边界（恰好 50 MB 放行、+1 字节拒） |
| `validate_rejects_forbidden_extension_in_whitelist` | 保存侧：白名单含 `sh` / `js` 直接报错并点名「硬禁止」 |

原有 `policy_rejects_bad_extension` 同时被加强：`evil.xyz`（非黑名单、不在白名单）走 `ExtensionNotAllowed`，
`evil.exe`（黑名单）走 `ExtensionForbidden` —— 两类拒绝的语义与文案分开了，码值仍同为 60004。

---

## 三、修正 5 + 6：命名与权限码判据写进规范

`docs/编码规范.md` 新增两节（节选）：

```md
#### 1.1.1 表名前缀判据（2026-10-29 设计修正 A · 修正 5）
sys_ = 平台治理与系统能力（用户/部门/角色/菜单/配置/文件/偏好/字典/日志）
biz_ = 业务对象（由业务产生、有业务流程：任务、项目、订单…）
- 新业务对象一律 biz_
- 历史遗留（标注，不改名）：sys_announcement / sys_help_doc / sys_change_log 已上线，保持原名

#### 1.7.1 权限码前缀判据（2026-10-29 设计修正 A · 修正 6）
business: = 终端用户日常可见/使用的功能
system:   = 仅管理员配置的平台能力
support:  = 开发者/运维工具
- 历史不一致（标注，不迁移）：system:help:* 本应为 business:help:*，已授权到角色 → 保持现状
- 复核过的正例：business:announcement:* / business:task:* / system:change-log:* / system:user:*
```

`docs/后端二开指南.md`：§十 数据权限清单补一条「闸② 的登记表清单是表的集合，不是目录的集合」；
新增 §十三「上传分类与存储策略（新增上传口必看）」5 条 checklist（必须种策略行 / 白名单 fail-closed /
可执行扩展名代码级硬禁止且不许删项 / AI 产出用 task_ai / 迁移自带值自检块）。

团队内部规范 `/root/.hermes/workspace/manong/cailu-dev-standards.md` 同步更新（§5 命名规则表 + §8 权限码判据）。

---

## 四、验收总清单

| # | 项 | 结果 |
|---|---|---|
| 1 | 门禁：故意违反能被抓到（贴前后对比）+ 修好后全绿 | ✅ §1.2 失败态（EXIT=1，点名 `business/task/tmp_gate_probe.rs`）/ §1.3 全绿（EXIT=0） |
| 2 | `scripts/check.sh` 全绿（B 闸①/②=0/0、C7 G1–G6） | ✅ `闸①=0 处  闸②=0 处`、`C7 G1–G6 共 6 条，未通过 0 条`、`全部检查通过` |
| 3 | `V0107` 空库 up/down/up 通过 + 5 条策略实测 | ✅ §2.2（18 条断言）+ §2.3（11 条断言），`PASS=29 FAIL=0` |
| 4 | 规范文档已更新（贴 diff 片段） | ✅ §3（编码规范 2 节 + 二开指南新增 §十三） |
| 5 | `cargo test` 全绿（584+） | ✅ `passed=588 failed=0 ignored=7`（基线 584 → 588，新增 4 条） |
| 6 | `HEAD == origin/main` | ✅ 见 §5 |

## 五、提交与指针

- 功能提交：`fix(design-a): 门禁补 biz_task 全家族 + V0107 任务附件策略 + 命名/权限码判据`
- 提交后 `git rev-parse HEAD` = `origin/main` = bare `main`（`.githooks/post-commit` 自动推 bare，收尾再显式 fetch/merge 核对）。
- 回滚指引：`V0107__storage_task_policies.down.sql` 只删本迁移的两行（固定 id + category 双限定）；
  `FORBIDDEN_EXECUTABLE_EXTENSIONS` 是代码常量，回退需改代码（不建议 —— 它只收紧不放松）。
