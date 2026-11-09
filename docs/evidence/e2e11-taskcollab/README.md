# E2E-11 · 任务 + 协作域演练证据（145 / cloudpivot_dev）

> 卡：`t_60b8feb8`（E2E-11 任务+协作域：任务/看板/甘特/模板 + 项目 + 公告/通知/消息）
> 环境：145 测试机 · 后端 `127.0.0.1:8033`（systemd `cloudpivot-test`，debug 构建 2026-11-09 12:48）
> 库：`cloudpivot_dev`（PG 18，允许写演练数据）· 账号 `admin`（`/etc/cloudpivot/cloudpivot-dev.env`）
> 时间：2026-11-09 13:08 CST
> **未连 144 生产；未 DROP / TRUNCATE；未改已应用迁移；只操作 `E2E11%` 前缀数据**

## 演练范围

| 页面 | 覆盖功能点 |
|---|---|
| ① 任务列表 | 建任务（默认值/编号 TSK-YYYYMM-NNNNNN）→ 分配（creator 派活不写 assigner / 非 creator 转派写 assigner+TRANSFERRED）→ 子任务 **8 层链** → 第 9 层 `400+63005` → 折叠展开（`root_only=true` 只出根 + 逐层 `subtasks` 懒加载）→ 可见性 / 写权限边界 |
| ② 任务看板 | 5 态列分组（`status=` 过滤）→ 拖拽改状态（`PATCH /status`）→ 批量改状态 / 批量指派（逐条结果，非整体回滚） |
| ③ 甘特图 | 只读排期：窗口回显、`bar.offset_days/length_days`、依赖边、`>180 天` 400、`board_id`（P1 能力）400、改期走 `PATCH /schedule` 而非拖拽 |
| ④ 任务模板 | 建模板（含 3 个子任务）→ 从模板一键建任务（子任务 3 条 / estimate_hours / owner 覆盖）→ 改 → 软删 → 已删模板再建 404 |
| ⑤ 项目 | 建项目（编码唯一）→ 成员 MEMBER/VIEWER（幂等 changed=false、非法角色 400）→ 任务归项目 → 项目任务列表 / 完成率 → 移除成员 → 非负责人维护成员被拒 |
| ⑥ 公告 | 草稿 → 发布（扇出站内信 + 幂等）→ 可见范围 ALL / USER / ROLE（各角色视角严格互斥）→ 未读计数 → 已读（`first_read` 幂等）→ 全部已读 → 阅读统计 → 附件上传/绑定/下载（两条路径）→ 置顶（带到期）→ 下线（原因必填） |
| ⑦ 消息 | 站内信发送（USER 定向）→ 我的通知 → 未读数 → 单条已读 → 全部已读 → 模板（建 / 试渲染 sample 兜底 / 缺变量发送 422 / 被引用删除 409）→ **SSE 实时推送**（事件含 `receiver_id`/`notice_id`）→ **频道隔离**（投给甲、乙收不到） |
| ⑧ 任务附件（AUD-01 回归） | 上传 → `uploader_name` 快照 → **用户改名后仍显示上传当时姓名**（负责人姓名同时实时 join 显示新名）→ 下载（负责人 / 管理员）→ 软删留痕 |

## 结果总览

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| S0 | 夹具：2 角色（`data_scope=4 仅本人`）+ 3 用户 + 菜单授权 | 200 | `sys_role.data_scope=4` | 通畅 |
| S1-1 | `POST /business/task` 根任务 | 200 | `TODO` / `TSK-202611-000001` | 通畅 |
| S1-2 | `PATCH /task/{id}/assign` 派给甲 | 200 | `owner_id=甲`，`assigner_id=NULL`（creator 派活口径） | 通畅 |
| S1-3 | `PATCH /assign` 甲转派乙 | 200 | `assigner_id=甲`，`source=TRANSFERRED` | 通畅 |
| S1-4 | 连挂 7 层子任务（共 8 层） | 200 ×7 | 递归 CTE `depth=8 nodes=8` | 通畅 |
| S1-5 | 第 9 层子任务 | **400 + 63005** | 未落库 | 通畅（护栏生效） |
| S1-6 | `page?root_only=true`（折叠口径） | 200 | 仅根任务 1 条 | 通畅 |
| S1-7 | 协作人（`user_ids`）→ 乙改任务 | 200 → **403** | 协作行存在、任务未变 | 通畅（写权限≠可见性） |
| S2-1 | 叶子任务 `TODO→DOING→DONE` | 200 / 200 | `DONE`、`done_at` 有值、`progress=100` | 通畅 |
| S2-2 | 状态日志 | 200（`/changelog/timeline`） | `sys_change_log action=STATUS` `{a,b}` 前后值 + 操作人 | 通畅 |
| S2-3 | `TODO→DONE` 跳级 / `DONE→CANCELLED` | **409 + 63010** ×2 | 状态未变 | 通畅 |
| S2-4 | 有子任务父任务 `DOING→DONE` | **409 + 63006** | 进度锁定为 0（子任务汇总） | 通畅 |
| S2-5 | 需验收任务 `DOING→DONE` | **409 + 63010** | 仍在 `DOING` | 通畅 |
| S2-6 | `DOING→REVIEW` → 自动挂 `TASK_ACCEPT` 审批单 | 200 | `approval_instance_id` 有值、实例 `PENDING` | 通畅 |
| S2-7 | 审批中 `REVIEW→DONE`（负责人 / 创建人各一次） | **409 + 65030** ×2 | 状态未变（门闸优先于身份） | 通畅 |
| S2-8 | 审批中心 `approve` | 200 | 实例 `APPROVED` → 回调任务 `DONE` + `done_at` + 日志（`审批流程（系统回调）`） | 通畅 |
| S2-9 | 驳回默认（PREV） | 200 | 实例仍 `PENDING`、round=2、`START` 重开、RESUBMIT 待办 1 条、任务**不**被误推 | 通畅 |
| S2-10 | 终止驳回（`return_to=NONE`） | 200 | 实例 `REJECTED` → 任务回 `DOING`、`reject_reason`=审批意见、进度归 10 | 通畅 |
| S3-1 | B 加前置 A | 200 | `biz_task_dependency` 1 行、`blocking_count=1` | 通畅 |
| S3-2 | 前置未完成 B→DOING | **409 + 63102** | 状态未变，文案点名「E2E11 依赖-前置A」 | 通畅 |
| S3-3 | 自依赖 / 成环 | **400 + 63103** / **400 + 63101** | 未落库，环上任务被点名 | 通畅 |
| S3-4 | A 完成后 B→DOING | 200 | 后继负责人收到强制站内信 1 条 | 通畅 |
| S3-5 | 前置 `CANCELLED` 不阻塞 | 200 | E 进 `DOING` | 通畅 |
| S3-6 | 解除依赖 / 后继视角 | 200 | 前置空、`blocking_count=0`、successors 含 E | 通畅 |
| S4-1 | 看板 5 列分组 / 拖拽 | 200 | `TODO`→`DOING` 列归属正确 | 通畅 |
| S4-2 | `batch-status` 2 条 | 200 | `success_count=2`，均 `DOING` | 通畅 |
| S4-3 | `batch-assign` 2 条 | 200 | `owner_id=乙` 2 行 | 通畅 |
| S4-4 | `batch-status` 混入非法 uuid | 200 | `fail_count=1`、合法项照常推进 | 通畅（逐条语义） |
| S5-1 | `GET /task/gantt?start&end` | 200 | `range.days=30`、`offset_days=9`、`length_days=5`、依赖边 1 | 通畅 |
| S5-2 | 窗口 365 天 / `board_id` / 起止倒置 | **400+63105 / 400+63103 / 400** | — | 通畅 |
| S5-3 | `PATCH /task/{id}/schedule shift_days=2` | 200 | `start 11-17 / due 11-27` | 通畅 |
| S6-1 | 建模板（3 子任务）+ 列表 | 200 | `biz_task_template` 1 行 | 通畅 |
| S6-2 | `from-template` 一键建任务 | 200 | 父任务 + **3 子任务**、`estimate_hours=4`、owner 覆盖生效 | 通畅 |
| S6-3 | 改 / 软删 / 已删模板再建 | 200 / 200 / 404 | `deleted_at` 有值 | 通畅 |
| S7-1 | 建项目 `E2E11P` | 200 | `project_no=E2E11P`、`owner=甲` | 通畅 |
| S7-2 | 重复编码 | 400/409 | — | 通畅 |
| S7-3 | 成员 MEMBER / VIEWER / 幂等 / 非法角色 | 200 / 200 / `changed=false` / 400 | 成员 2 行、`real_name` 实时取名 | 通畅 |
| S7-4 | 任务归项目 + 项目任务列表 + 完成率 | 200 | `task_total=1 task_done=1 completion_rate=100.0` | 通畅 |
| S7-5 | 非负责人维护成员 / 移除成员 | 403/404 / 200 | 成员行删除 | 通畅 |
| S8-1 | 上传公告附件（storage，png） | 200 | `sys_file` 落库 | 通畅 |
| S8-2 | 建公告（草稿，ALL，带附件） | 200 | `DRAFT`、`AN2026110986C19A`、附件绑定 1 | 通畅 |
| S8-3 | 发布 | 200 | `PUBLISHED`、`publish_at`、扇出通知 39 人（= 实际接收行 39） | 通畅 |
| S8-4 | 用户可见 / 未读 / 已读（幂等）/ 全部已读 | 200 | 未读 1→0、`sys_announcement_read` 1 行、`first_read` true→false | 通畅 |
| S8-5 | 阅读统计 / 附件下载（两条路径） | 200 | `read_count=1` | 通畅 |
| S8-6 | 范围隔离：USER 只给乙 / ROLE 只给执行人角色 | 200 | 甲看不到定向、乙看得到；同角色乙看得到、外角色看不到 | 通畅 |
| S8-7 | 范围非 ALL 但 `scope_ids` 空 | **400 + 62006** | — | 通畅 |
| S8-8 | 置顶（`is_top=true` 必带 `top_expire_at`）/ 下线（原因空 400） | 200 / 200 | `is_top=t`、`OFFLINE`、下线后用户不可见 | 通畅 |
| S9-1 | SSE 订阅（甲、乙两条连接） | 200 `text/event-stream` | `stream/stats`：`total_users=2 receivers=2` | 通畅 |
| S9-2 | 发站内信（USER→甲） | 200 | `receiver_count=1`、接收行 1 | 通畅 |
| S9-3 | SSE 实时推送 | — | 甲收到 1 条（title/notice_id/receiver_id 一致）、**乙 0 条** | 通畅 |
| S9-4 | 我的通知 / 未读 / 单条已读 / 全部已读 | 200 | 未读 -1 → 全部已读后 0 | 通畅 |
| S9-5 | 模板：建 → 试渲染 → 缺变量发送 → 被引用删除 | 200 / 200 / **422** / **409** | 模板渲染标题 `【E2E11】完成通知` 落库 | 通畅 |
| S9-6 | 管理端分页 / 详情 / 软删 | 200 | 删除后接收侧不可见 | 通畅 |
| S10-1 | 任务附件上传 | 200 | `attachment_id`、`uploader_name=E2E11 执行人甲` | 通畅 |
| S10-2 | 上传者改名后再看附件 | 200 | `uploader_name` **仍为旧名**（快照）；`owner_name` 显示新名（实时 join） | 通畅（AUD-01 回归通过） |
| S10-3 | 下载（负责人 / 管理员）/ 软删 | 200 / 200 / 200 | `deleted_at` 有值、列表为空 | 通畅 |

**断言总数：324 PASS / 0 FAIL**（A 段 191：S0 14 + S1 49 + S2 54 + S3 27 + S4 13 + S5 18 + S6 16；B 段 133：S7 30 + S8 51 + S9 38 + S10 14）

## 勾稽验证（DB 直查，9 组断言全 0 差异）

| # | 勾稽项 | 结果 |
|---|---|---|
| 1 | 八层父子链深度（向下递归） | `depth=8 nodes=8`（与卡片要求一致） |
| 2 | 悬空父任务（parent_id 指向不存在/已删） | 0 |
| 3 | `DONE` 任务缺 `done_at` | 0 |
| 4 | `DONE` 任务进度非 100 | 0 |
| 5 | 公告 `read_count` 冗余 vs `sys_announcement_read` 回执行数 | 0 差异 |
| 6 | 通知 `read_count` 冗余 vs `sys_notice_receiver.read_at` | 0 差异 |
| 7 | 任务附件 `uploader_name` 为空 | 0（1/1 有快照） |
| 8 | 审批单 `APPROVED` 但任务未 `DONE`（回调漏推） | 0 |
| 9 | 公告 `receiver_count` vs 实际通知接收行 | 0 差异（39/1/2 三档全部对齐） |

补充口径证据：

- 公告扇出：`receiver_count=39`（ALL）/ `1`（USER 定向）/ `2`（ROLE 两成员），与 `sys_notice_receiver` 实际行数逐条相等。
- 通知通道：本卡 4 条通知 `channel_mask=1`（仅站内信）→ 不进 `sys_notice_delivery`（该表只记外发通道），`dispatch_status=SENT`。
- 状态日志：`sys_change_log`（`resource=biz_task`, `action=STATUS`）记录 `{a: 新态, b: 旧态}` + `operator_name`；**审批回调行操作人显示「审批流程（系统回调）」**，人工行显示 `admin` / `e2e11_w1`。
- 父任务自动状态：子任务首次进入 `DOING` 时，父任务自动 `TODO→DOING`（日志文案「父任务因首个活动子任务自动进入进行中」），且父任务 `progress` 保持 0（进度由子任务汇总，不手改）。

## 问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| 1 | 公告 / 任务附件的存储策略拒绝 `.txt`（返回 `400 + 60004`「不支持的文件类型 .txt，允许 jpg,jpeg,png,pdf,docx,xlsx,zip」） | P3 | `POST /api/business/storage/upload?category=announcement` 上传 `.txt` → 400 | 公告正文说明、巡检记录等纯文本只能靠正文粘贴，无法作为附件 | 若为有意白名单，请在公告页上传处提示允许类型；若要放开，给 `announcement`/`task` 策略加 `txt,md,csv` |
| 2 | 公告中心无「超管短路」：`SUPER_ADMIN` 在 `/announcement/visible` 看不到 ROLE/USER 范围公告（`total=0`），ALL 范围可见 | P3 | admin 调 `GET /api/business/announcement/visible?keyword=E2E11 角色公告` → `total=0` | 管理员在公告中心里「自己发的定向公告消失」，只能去公告管理页；易被误报为丢数据 | 产品拍板：若超管应全见，在 `VISIBLE_PREDICATE` 前加 `is_super` 分支；否则前端在公告中心提示「定向公告请到管理页查看」 |
| 3 | 公告附件下载路径参数名为 `{file_id}`，语义实为「附件绑定行 id」 | P4 | `GET /announcement/{id}/attachments/{file_id}/download` 传 `sys_file.id` → 404；传 `sys_announcement_attachment.id` → 200 | 集成方按参数名传 `file_id` 必 404（本卡踩到） | 后端注释已说明；建议前端 api 注释同步，或新增 `{att_id}` 路径并保留旧路径 |
| 4 | 直接改库 `sys_role.data_scope` 不会失效用户数据范围缓存（Redis `perm:user_scope:{epoch}:{uid}`），最长 300s 仍按旧范围过滤 | P4 | 1) `psql update sys_role set data_scope=4` 2) 用户请求任务列表 | 演练/运维若用 SQL 改范围，会出现「范围已收紧但仍越权可见」（本卡首次运行实测：乙能看到全量 34 条） | 一律走 `PUT /api/system/role/{id}`（会 `invalidate_role_scope_cache` + 推进 token_version）；脚本已内置 `e2e11_flush_scope.py` 兜底 |
| 5 | `task/template.rs` 模块文档写「`biz_task` 的深度上限是 2（父任务自身不能再有 parent_id）」 | P4 | 读代码注释 | 与实现 `MAX_TASK_DEPTH=8` 矛盾，二开时容易按 2 层设计 | **已修**（见下） |
| 6 | `TASK_ACCEPT` 默认流程发布后，任务一进 `REVIEW` 就挂审批单，创建人手工 `REVIEW→DONE` / `REVIEW→DOING` 一律被 `409 + 65030` 拦住 | P4（待产品确认） | 需验收任务 `DOING→REVIEW` → 创建人点「验收通过」→ 65030 | 验收人必须去审批中心处理；任务详情页的验收按钮会「点了报错」 | 属门闸设计（门闸优先于身份）；建议前端在 `approval_instance_id` 非空时把「验收通过/打回」置灰并给「去审批中心」入口 |
| 7 | 项目成员角色 `VIEWER` 与 `MEMBER` 权限完全等价（写权限只看项目负责人/创建人/权限码） | P4（已知预留） | VIEWER 成员仍可被 `ensure_writable` 之外的操作影响（角色行仅展示） | 与「成员权限」字面预期不符 | 注释已标「本卡两者权限等价，预留」；若要做只读成员，需在项目写路径加成员角色判定 |

P0 = 0，P1 = 0，P2 = 0。

## 修复记录

- `backend/src/business/task/template.rs`（模块文档，仅注释）：把「`biz_task` 深度上限是 2」的过期说法改为「模板子任务只挂一层；表本身的父子深度上限是 `MAX_TASK_DEPTH = 8`（E2E-11 实测第 9 层 `400 + 63005`）」。`rustfmt +1.94.1 --edition 2024 --check` 无 diff。
- 演练脚本（新增，`scripts/verify/`）：`e2e11_lib.py`、`e2e11_a.py`（S0–S6）、`e2e11_b.py`（S7–S10）、`e2e11_db_evidence.py`、`e2e11_cleanup.py`、`e2e11_flush_scope.py`、`e2e11_q1.sql`。
- commit：见卡内汇报（`test(e2e11): …`）。

## 结论

**全部通畅。** 任务域（8 层父子 / 状态机 + 状态日志 / FS 依赖三护栏 / 看板拖拽 + 批量 / 甘特只读 + 窗口上限 / 模板一键建任务）、协作域（项目成员与任务归属、公告发布→范围→已读→附件→置顶→下线、站内信 + 模板 + SSE 实时推送与频道隔离、任务附件 `uploader_name` 快照回归）在 145 真实 HTTP + 落库层面全部按设计工作。

- 324 条断言全绿，9 组 DB 勾稽 0 差异。
- 历史问题回归：**任务管理 v2（父子 8 层 / 折叠展开 / 依赖）通过**；**AUD-01 任务附件 uploader_name 快照通过**（改名后仍显示旧名，同时负责人姓名实时 join 显示新名）。
- 3 个 P3/P4 待拍板项已列清单（存储白名单、超管公告可见性、通道/参数口径），本卡未发现 P0/P1/P2 缺陷。

## 复现方式

```bash
# 145 上
scp scripts/verify/e2e11_*.py scripts/verify/e2e11_q1.sql root@172.18.10.145:/tmp/e2e11/
ssh root@172.18.10.145
cd /tmp/e2e11
python3 e2e11_cleanup.py      # 清本卡前缀数据（幂等）
python3 e2e11_flush_scope.py  # 清范围缓存（走 SQL 改过 data_scope 时必需）
python3 e2e11_a.py            # S0–S6（191 断言）
python3 e2e11_b.py            # S7–S10（133 断言）
python3 e2e11_db_evidence.py  # DB 证据 + 9 组勾稽（退出码 0 = 全 0 差异）
python3 e2e11_cleanup.py      # 收尾自清（残留应为 0）
```

## 文件说明

| 文件 | 内容 |
|---|---|
| `asserts.log` | 324 条断言原始日志（`PASS/FAIL\t场景\t说明`） |
| `run_a.out` / `run_b.out` | 两段脚本完整 stdout（含每步 HTTP 状态与 DB 值） |
| `db_evidence.out` | 20 组 DB 证据 + 9 组断言式勾稽输出 |
| `raw/*.json` | 每步 API 原始响应（193 个，按步骤名索引，页面契约以此为准） |
