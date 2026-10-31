# 测试D · 其余业务模块测试报告（公告/通知/帮助/存储/项目/日志/互斥/导入导出）

- 卡片：`t_69e8220e`【测试D】公告/通知/存储/项目/日志/互斥/导入导出测试（66+ 用例，含 8 条跨模块联动）
- 仓库/工作区：`/opt/cloudpivot`（主仓 `main`，直接在主仓干；只加测试文件，未改业务逻辑）
- 数据库：只连 `cloudpivot_test`（`backend/tests/testd/common.rs::ensure_db` 硬校验连接串，指错库直接 panic）
- 执行命令：`cd backend && cargo test --test testd -- --test-threads=1`
- 最终结果（真实输出）：
  ```
  test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 332.85s
  ```
  另加 HTTP 层越权验证 `bash scripts/verify/testd_http.sh`：`结果：11 通过 / 0 失败`

---

## 0. ★ 安全类发现（先看这里）

> 判定口径：能被**已登录但无相应权限**（或仅持低权角色）的账号利用、或让敏感数据以明文离开系统的，才算安全类。

| # | 严重度 | 发现 | 证据（真实断言/输出） | 用例 |
|---|---|---|---|---|
| **BUG-D-08** | **P2 越权（已复现）** | **文件存储没有属主隔离**：`sys_file.uploaded_by` 记了上传人，但读路径 `storage::model::find_active_file` 只有 `id` 入参，**结构上不可能按用户收口**。凡持 `business:storage:download` 的人都能下载别人上传的任何未删文件 | `[F05] 越权下载风险：读路径无属主隔离（other=… 也能取到 owner=… 的文件 …）`；测试用两个不同用户夹具断言同一记录对两者可见 | F05 |
| **BUG-D-09** | **P2 危险文件类型漏网** | 硬禁止扩展名黑名单只有 `exe/sh/bat/ps1/so/dll/jar/py/js/vbs/msi`，**没有 `php/jsp/aspx/cgi`**；而内置默认分类策略 `allowed_extensions` 为空（= 不校验白名单），于是 `shell.php` 直接放行 | `[F03] 黑名单未收录：["php", "jsp", "aspx", "cgi"]；默认策略下 shell.php 是否放行：true` | F03 |
| **BUG-D-13** | **P2 审计缺口（明文外泄无留痕）** | 持 `system:field-mask:plain` 的用户导出即明文（设计如此），但「明文访问留痕」写入函数在真库上**直接报错**（`Value::Json` 绑定 `?::jsonb`），而调用处是 `tokio::spawn` + 只打日志 → **明文导出没有审计痕迹** | `[X05] 审计写入失败（明文访问无留痕）：query failed: : db error`；对照：同一函数直接调用同样失败 | X05 |
| **BUG-D-14** | **P1 内控缺口** | `business/erp` 下**没有**「制单人不得审核自己单据」的校验（全目录扫描 `制单人不得/不能审核自己/ensure_not_self/SELF_APPROVAL` 零命中）。目前唯一屏障是「角色互斥（SoD）」，一旦角色配置漏配或历史数据双持，同一人即可自审 | `[K08] ERP 侧无 maker≠checker 校验（仅靠角色互斥兜底）` | K08 / X05(mutex) |
| BUG-D-10 | P3 跨平台一致性 | `sanitize_key` 对前导 `/` 会规整进根目录（安全），但反斜杠形态 `..\..\windows\system32\cmd.exe` 在 Linux 上被当成普通文件名保留；将来若在 Windows 跑本地后端必须先收紧 | F04 断言规整后仍落在 root 内 | F04 |
| BUG-D-05 | P3 数据一致性 | 公告下线（软删）后，由它扇出的通知**不随之软删**，收件人仍能在「我的消息」里看到并回执 | `[G03] 公告下线后仍可见的通知收件行：1` | G03 |

**已确认不存在以下风险（反向验证通过，不是漏测）：**

- 路径穿越（`../../etc/passwd`、绝对路径、NUL 字节）：`LocalStore::put` 全部拒绝，物理上不会写到根目录之外 —— F04。
- 帮助中心富文本 XSS：`<script>` 被手工塞进 `content_html`，出参被净化 —— H03。
- 无脱敏规则时 fail-closed：敏感列缺规则 → 引擎层 `MissingRule` → HTTP 422，绝不回退明文 —— X04。
- 越权调接口：低权角色打导出/导入/监控/公告发布 5 个接口，**HTTP 层真 403**（不是前端隐藏）—— `scripts/verify/testd_http.sh` 实测 5/5 均为 403。

全局脱敏规则（`sys_field_mask.role_id IS NULL`）在 `cloudpivot_test` 里有 3 条，导致 X04 的「无规则角色」仍拿到掩码值；引擎层 fail-closed 用**合成列**独立验证，不受库里既有规则干扰。

---

## 1. 用例总览（67 条，全部真库执行）

| 模块 | 文件 | 用例 | 结果 |
|---|---|---|---|
| 导入导出 | `tests/testd/impexp.rs` | 13（I01–I09、I11–I14） | 全绿 |
| 字段脱敏（导出/详情/回显） | `tests/testd/masking.rs` | 4（X02–X05） | 全绿 |
| 公告与消息 | `tests/testd/announcement.rs` | 10（G01–G10） | 全绿 |
| 文件存储 | `tests/testd/storage.rs` | 10（F01–F09 + Z01 自清理自检） | 全绿 |
| 项目 + 变更日志 | `tests/testd/project_log.rs` | 11（P01–P05、C01–C06） | 全绿 |
| 互斥 + 帮助 + AI/监控 | `tests/testd/mutex_help_ai.rs` | 11（X01–X05、H01–H03、AI01/02、MO01） | 全绿 |
| **跨模块联动** | `tests/testd/linkage.rs` | 8（K01–K08） | 全绿 |
| 合计 | | **67** | **67 passed / 0 failed** |

卡面配额对照：导入导出 13≥13、脱敏 4≥4、公告消息 10≥10、存储 9≥9、项目 5≥5、日志 6≥6、互斥 5≥5、帮助 3≥3、AI/监控 3≥3、联动 8=8 —— 全项达标（另有 Z01 一条基建自检）。

---

## 2. 导入导出（P0，13 条）

内建能力边界（重要，决定了用例形态）：注册表里只有两个定义 —— 导入 `system.dict_item`、导出 `system.user`（带姓名/邮箱/手机三个敏感列）。**没有 `biz_task` 导入定义**（BUG-D-01，P2 需求缺口）。

| 用例 | 钉住的行为（真实结果） |
|---|---|
| I01 | 模板下载 → 填数 → 预检 → 确认入库全链路。模板首行是表头；预检 `total=2/error=0`；入库 `SUCCESS`、`imported_rows=2`、库里恰好 2 行；「启用」列落 `1`、排序号落 `1`（字段映射正确） |
| I02 | 校验失败 → 错误报表可下载：`total=3/error=2`，错误行带**列名**（`column_header=Some("排序号")`）与行号，产物 xlsx ≥3 行 |
| I03 | 事务边界：**STRICT** 一行都不进（422 + 任务 `FAILED`）；**PARTIAL** 只进好行（`status=PARTIAL`、`imported_rows=1`、库里恰好 1 行） |
| I04 | 1 万行 xlsx：预检 `total=10000`、`error=0`，耗时 **501 ms**（无超时、无 OOM；顺带证明外键校验做了批量预取：`外键 SQL 1 条`） |
| I05 | 重复导入：按 (字典编码, 值) **幂等 upsert**，不报错、不产生重复行（两轮后仍 1 行） |
| I06 | `biz_task` 导入定义缺失 → 请求结构化 404 + 60011，不 panic（对应 BUG-D-01） |
| I07 | 同步导出：HTTP 200、文件可解析、含夹具数据、状态枚举中文化（`1→启用`/`0→停用`） |
| I08 | 异步导出：建任务 → worker 认领 → `SUCCESS` → 产物落盘可下载 |
| I09 | 导出权限码在 `sys_menu` 已登记；令牌不带码时 `has_permission=false`（真 403 由 `scripts/verify/testd_http.sh` 在服务进程上验证，5/5 通过） |
| I11 | 失败任务 `FAILED` + 有失败原因；重试接口把 `attempts` 清零重新排队；非法状态重试返回 409 |
| I12 | 产物过期（`expire_at` 过后）→ 下载 **410** |
| I13 | 表头与目标表字段不一致 → 结构化错误（4xx + 明细），不 panic、不半截入库；非 xlsx 纯文本也是 400 而非 500 |
| I14 | 预检通过后替换文件 → 确认入库时 SHA-256 不匹配被拒（防「验的是 A、入的是 B」） |

---

## 3. 字段脱敏（P0，4 条）

| 用例 | 结果 |
|---|---|
| X02 详情接口 | 无豁免用户看详情，姓名/邮箱/手机三列均为掩码（`张**` / `z***@example.com` / `138****5678`） |
| X03 导出文件 | 同步导出产物里是掩码值：`[X03] 同步导出掩码值命中：138****5678` |
| X04 fail-closed | 敏感列无生效规则 → 引擎 `MissingRule` → 422；接口层若 200，则文件里**不允许出现明文手机号/邮箱**（断言两侧都不放行） |
| X05 明版权限 | 持 `system:field-mask:plain` 导出为明文（设计如此），但留痕写入真库失败 → **BUG-D-13** |

---

## 4. 公告与消息（P1，10 条）

| 用例 | 结果 |
|---|---|
| G01 | 定向公告发布后 `receiver_count=1`、扇出通知；目标用户未读 ≥1 且出现在可见列表；**非目标用户看不到**（无越权泄漏） |
| G02 | 已读/未读统计：首次标记新增、重复标记幂等（`read_count` 仍为 1）、统计接口与未读名单一致 |
| G03 | 公告删除是**软删**（`deleted_at` 有值），已读回执**保留**（审计需要）；但关联通知不随之下线 → **BUG-D-05（P3）** |
| G04 | 已发布公告禁止编辑（必须先下线）；下线后可编辑 |
| G05 | 多目标扇出：`receiver_count=2`、`sys_notice_receiver` 2 行、两人未读各 +1 |
| G06 | 消息已读 → 未读数归零（徽标联动见 K02） |
| G07 | 模板变量替换：`{{name}}` 在标题/正文被替换为实际值；缺变量时渲染报错而不是留空 |
| G08 | 通道掩码生效：只勾邮件时**不生成站内信投递行**；未配置邮件网关 → 投递状态 `SKIPPED`（不是静默丢失）；`DispatchResult` 逐通道给状态。观察项：`DispatchResult` 里 INBOX 恒报 `DELIVERED`（`plan_channels` 强制 OR），与「无站内信投递行」口径不一致，属可观测性口径问题（P4，仅记录） |
| G09 | 发布权限码 `business:announcement:publish` / `business:notice:send` 均已登记；无码令牌 `has_permission=false`（HTTP 层 403 实测通过） |
| G10 | 消息搜索/筛选/分页：关键词命中 3 条、每页 2 条翻页正确、无关关键词 0 条、「我的」列表 ≥3 条 |

---

## 5. 文件存储（P1，9 条 + Z01）

| 用例 | 结果 |
|---|---|
| F01 | 本地后端写入 → 读回，字节与 SHA-256 完全一致 |
| F02 | 超限文件被策略拒（`SizeExceeded { size_bytes: 53477376, max_size_mb: 50 }`），不是「先落盘再报错」 |
| F03 | 硬禁黑名单拦 `exe/sh/bat/ps1/so/dll/jar/py/js/vbs/msi`；**php/jsp/aspx/cgi 漏网且默认策略放行 shell.php** → **BUG-D-09（P2）** |
| F04 | `..`、绝对路径、NUL 键全部拒绝；反斜杠形态在 Linux 上被规整进根目录（安全，但跨平台语义不一致 → BUG-D-10 P3） |
| F05 | 读路径无属主隔离 → **BUG-D-08（P2 越权）** |
| F06 | 两套后端关键路径：本地真实读写 + MinIO 风格 SigV4 预签名都能工作 |
| F07 | 预签名 TTL 被夹到合法区间，过期时间 = 签发时刻 + 实际 TTL |
| F08 | 删除是**软删**：行保留（`deleted_at` 有值）、读路径不再返回 |
| F09 | 附件与单据：单据**软删**时附件行保留、`sys_file` 本体保留；**硬删**才由 `CASCADE` 清附件行（并断言 `confdeltype='c'`） |
| Z01 | 本目标跑完，`TD-` 前缀在 16 张关键表里残留 **0** 行（见 §9） |

---

## 6. 项目与变更日志（P1/P2，11 条）

### 项目 P01–P05

| 用例 | 结果 |
|---|---|
| P01 | 建项目 → 挂任务成功；重复项目编码 400（唯一约束生效） |
| P02 | 删项目 → 其下任务**不删也不孤儿**：任务保留、`project_id` 解绑（`unbound_tasks=2`） |
| P03 | 项目统计口径 = 任务汇总：`total=2 / doing=1 / done=1`，与库里任务数一致 |
| P04 | 项目成员（`biz_project_member`）**不构成数据范围**：成员角色仍是 `SelfOnly`，看不到项目里别人的任务；加/移成员的事务读写正确。若产品要求「项目成员可见项目全部任务」，这是**口径缺口**（当前实现不做此承诺），需产品确认 |
| P05 | 项目归档：状态变 `ARCHIVED`；非法状态被拒 |

### 变更日志 C01–C06

| 用例 | 结果 |
|---|---|
| C01 | 建 / 发布 / 下线 = 恰好 3 条日志（不多不少） |
| C02 | 字段完整：操作人、`action_label` 中文名、`diff`、时间都在；不存在缺操作人/缺动作名的行 |
| C03 | 不可篡改：递归扫描 `src/` 无任何 `UPDATE sys_change_log`；权限表里也没有「修改日志」类权限码 |
| C04 | 按单据查完整轨迹：时间线升序、条数与状态变更次数一致；无关单据查不出 |
| C05 | 按时间/操作人筛选：只出本人的、未来时间窗为空 |
| C06 | 抽查 10 张公告，每张 changelog 条数 = 实际状态变更次数（建+发布=2） |

---

## 7. 互斥 / 帮助 / AI / 监控（P2，11 条）

| 用例 | 结果 |
|---|---|
| X01 | 互斥角色对落库后**对称命中**（A⚔B 与 B⚔A 等价），同一条规则不落多行 |
| X02 | 已持 A 再授 B → 拒绝，错误码 **61009（ROLE_MUTEX_CONFLICT）** |
| X03 | 一次请求里同时授 A+B 也拦（并集校验，复现 P09 的 `qa.mutex` 场景） |
| X04 | 绕过 API 直接写库造「双持用户」→ 全量扫描必须点名；此时新建规则对被 **409** 拦住（除非 force） |
| X05 | 互斥基座对 ERP「制单/审核」角色对生效 → 同一人无法同时持有；但 ERP 侧无独立「制单人≠审核人」校验 → **BUG-D-14（P1）** |
| H01 | 帮助文档 CRUD：分类 → 草稿 → 保存（`version+1`）→ 发布 `PUBLISHED` → 软删 |
| H02 | 搜索：命中/未命中/空关键词（空关键词返回空页，不查库）；**`keywords` 列不参与检索**（只写进 keywords 的词搜不到）→ **BUG-D-12（P3）** |
| H03 | XSS：`content_html` 里的 `<script>` 出参被净化（不执行、不带原标签） |
| AI01 | AI 账号（`user_type=ROBOT`）**不是**超管，权限 = 其绑定角色（`business:task:list` 有/无按角色走） |
| AI02 | AI 操作留痕：changelog `operator_type=ROBOT` 落库 |
| MO01 | 监控采集器在**真实 `/proc`** 上产出合法值（CPU 计数非 0、可用内存 ≤ 总量、使用率换算正确）；整卡接口形状由 HTTP 脚本验证（admin 200 / 低权 403） |

---

## 8. 跨模块联动 K01–K08（本卡重点，8/8 通过）

| 用例 | 联动 | 结果 |
|---|---|---|
| K01 | 公告 ↔ 徽标 | 发布 → 目标用户 `announcement_unread` +1；读掉 → 回落 |
| K02 | 通知 ↔ 徽标 | 消息已读 → `notice_unread` -1 |
| K03 | 存储 ↔ 任务 | 附件挂任务：任务**软删**附件行/文件本体保留；**硬删**才级联清 |
| K04 | 导入 ↔ 任务权限 | 导入任务列表默认 `only_mine`，别人的导入任务看不到（按 `create_by` 收口）；「导入→生成 biz_task→任务可见性」这一半因 BUG-D-01 无法端到端 |
| K05 | 导出 ↔ 脱敏 | 异步导出产物：`李*`、`l***@example.com`、`139****1111` —— 脱敏在**异步产物**里同样生效 |
| K06 | 项目 ↔ 任务 | 删项目 → 任务 `project_id` 解绑（`unbound_tasks=2`），不产生孤儿、也不误删 |
| K07 | 所有模块 ↔ changelog | 建/发布/下线各恰好 +1 条，不多不少 |
| K08 | 互斥 ↔ ERP | 互斥基座对 ERP 制单/审核角色对生效；ERP 侧缺独立校验 → BUG-D-14 |

---

## 9. 测试基建缺陷（本卡发现并已修复，BUG-D-04，P2）

**现象**：跑一轮 `cargo test --test testd`，`cloudpivot_test` 里就多出 **58 个用户 / 21 个角色 / 15 个项目 / 19 个文件行**，测试却全绿。

**根因**（不是「清理语句写少了」，是**顺序**错）：

1. `Ctx::reset` 里先删 `biz_project`、后删 `biz_task`，而 `biz_task.project_id` 是 `NO ACTION` → 项目删不掉；
2. `biz_project.owner_id` 又卡住 `sys_user` → 用户也删不掉（`foreign key violated (constraint biz_project_owner_id_fkey)`）；
3. `sys_file` **压根没有清理语句**；
4. `sys_dict_item` 只按 `item_label LIKE 前缀` 删，漏掉用别的前缀做的项；
5. 失败只 `println` 一行，测试继续绿 —— 典型「静默污染」。

**修复**（只改测试夹具 `backend/tests/testd/common.rs`，不碰业务代码）：

- `Ctx::reset` 改为**外键拓扑序**：`sys_change_log → 任务子表 → 任务（先断 parent_id）→ 项目 → 公告/通知 → 导入导出 → 帮助 → 字典 → 角色关联/互斥/脱敏 → sys_file → 用户 → 角色`；
- 失败改 `WARN[cleanup]` + 数据库原文 + SQL，并**返回本前缀残留行数**；
- 新增 `residue()` / `root_residue()` 与用例 **Z01**（`zz_no_fixture_residue_after_full_suite`）：跑完整套后交叉验证 `TD-` 前缀在 16 张表里残留 0 行 —— 不依赖清理函数的自报。
- 历史残留已用一次性脚本按同一拓扑序清空（58 用户 / 21 角色 / 15 项目 / 19 文件 / 12 任务附件 / 3 公告 / 5 通知 / 3 字典 / 1 互斥 → 全部 0）。

修复后实测：
```
test testd::storage::zz_no_fixture_residue_after_full_suite ... [Z01] TD- 前缀残留：[("sys_user", 0), ("sys_role", 0), ("sys_user_role", 0), ("biz_project", 0), ("biz_task", 0), ("biz_task_attachment", 0), ("sys_announcement", 0), ("sys_notice", 0), ("sys_import_task", 0), ("sys_export_task", 0), ("sys_help_doc", 0), ("sys_help_category", 0), ("sys_dict_type", 0), ("sys_dict_item", 0), ("sys_role_mutex", 0), ("sys_file", 0)]
ok
test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 332.85s
```

---

## 10. HTTP 层越权验证（真服务 + 真中间件）

`cargo test` 进程里没有 axum 路由栈，403 只能在服务进程上验，因此补了 `scripts/verify/testd_http.sh`
（`profile=test` → `cloudpivot_test`；脚本自建低权角色/用户，退出时清理，并还原 admin 的 `must_change_password`）。

真实输出：

```
== 测试D HTTP 越权验证（http://127.0.0.1:8023/api，前缀 TDHTTP）==
[0] 准备：清掉 admin 的「首次登录必须改密」标记（仅 cloudpivot_test；退出时还原）
[1] 健康检查
  ok    [health] 200
[1] 超管登录成功（token 长度 461）
[2] 超管正例（证明功能本身可用，403 不是「接口坏了」）
  ok    [admin GET /business/impexp/export/sync] 200
  ok    [admin GET /business/monitor/server-status] 200
[3] 建低权角色（不绑任何菜单/按钮 → 权限集为空）
  ok    [POST /system/role] 200
[4] 建低权用户（只绑该角色）
  ok    [POST /system/user] 200
[5] 低权用户登录成功（token 长度 453）
[6] 越权负例：低权用户打 5 个受保护接口，必须全部 403
  ok    [GET  /business/impexp/export/sync（I09 导出权限）] 403
  ok    [GET  /business/impexp/import/template（导入模板）] 403
  ok    [GET  /business/impexp/export/page（导出任务列表）] 403
  ok    [GET  /business/monitor/server-status（MO-01 监控）] 403
  ok    [POST /business/announcement（G09 谁能发公告）] 403
[7] 低权用户自己的普通接口应正常（证明不是全站 403）
  ok    [GET  /system/auth/me] 200

结果：11 通过 / 0 失败
```

跑完后 `TDHTTP-` 前缀用户/角色/changelog 残留均为 0，admin 的 `must_change_password` 已还原为 `true`。

---

## 11. 已知缺口（如实说明，不假装测过）

| 项 | 状态 | 原因 |
|---|---|---|
| 「导入 → 生成 biz_task → 按任务可见性过滤」端到端 | **测不了，卡需求** | 注册表里没有 `biz_task` 导入定义（BUG-D-01）。K04 只钉住「导入任务可见性 = create_by 收口」这一半 |
| 存储双持后端切换 MinIO 的**真实** S3 往返 | 部分 | 本机无 MinIO/S3 端点，F06 验的是本地真实读写 + SigV4 预签名算法（签名串/有效期），未做真实对象上传 |
| 导出产物 30 分钟过期的**时钟流逝** | 用 `expire_at` 回填模拟 | 不真等 30 分钟（I12 直接改写 `expire_at` 再调下载，验 410 分支） |
| 邮件/短信/Webhook 通道真实投递 | 只能验到 `SKIPPED` | 本机没有网关；G08 钉的是「未配置 = 明确 SKIPPED，不静默丢失」 |
| ERP 单据「制单人≠审核人」 | 代码层证据（扫描）+ 互斥基座 | ERP 属别的卡在改，本卡不动 erp；BUG-D-14 已登记 |

---

## 12. 复现

```bash
# 1) 全量用例（串行；夹具共享一个进程级连接池）
cd /opt/cloudpivot/backend
cargo test --test testd -- --test-threads=1

# 2) 只跑某一块
cargo test --test testd -- --test-threads=1 impexp::
cargo test --test testd -- --test-threads=1 linkage::

# 3) HTTP 层越权（另开一个 shell；先按注释里的环境变量起服务）
NIK_PROFILE=test NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=*** \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=... CLOUDPIVOT_STORAGE_MASTER_KEY=... \
cargo run -- profile=test

PGPASSWORD=*** bash /opt/cloudpivot/scripts/verify/testd_http.sh
```

依赖：本机 PostgreSQL（`cloudpivot_test` 已迁移）、`calamine`（已在 `Cargo.toml`）、无 Redis（`profile=test` 走本地缓存）。

---

## 13. 结论

- 卡面要求的 **≥58 + 8 联动 = ≥66** 全部落地：**67 条真库用例全绿**（含 1 条清理自检），另有 **11 条 HTTP 层断言**全绿。
- 未改任何业务代码（`git show --stat` 只有 `backend/tests/**`、`scripts/verify/testd_http.sh`、本报告）。
- **放行判断**：P0 的导入导出「事务边界 / 幂等 / 1 万行 / 篡改拒绝 / 过期 410 / 失败可重试」与脱敏「详情 / 导出 / fail-closed」都成立；但 **BUG-D-08（文件下载无属主隔离）与 BUG-D-09（php 类脚本漏网）必须先修再上线**，BUG-D-14（ERP 自审）与 BUG-D-13（明文导出无留痕）必须在 ERP 批次内跟进。
