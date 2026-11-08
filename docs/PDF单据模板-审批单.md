# 单据 PDF 模板（四）：审批单（PDF-05）

> 卡：`PDF-05`。前三批单据模板见 [PDF单据模板-采购销售订单.md](./PDF单据模板-采购销售订单.md)（PDF-02）、
> [PDF单据模板-合同与出入库单.md](./PDF单据模板-合同与出入库单.md)（PDF-03）、
> [PDF单据模板-调拨盘点退货.md](./PDF单据模板-调拨盘点退货.md)（PDF-04）；
> 底座（Chromium / 字体 / `business/pdf` 分层）见 [PDF导出基础设施.md](./PDF导出基础设施.md)。
>
> 本文件只讲**审批单**这一类：它和前面四批「业务单据」不同 —— 数据不在 `biz_erp_*`，
> 而在流程引擎的四张表里，且要能印出 **ARCH-8 的降级痕迹**（自审跳过 / 升级 / 兜底 / 委托）。

## 一、接口与权限

| 方法 | 路径（`/api` 前缀由 `[router].prefix` 注入） | 权限码 | 版式 | 说明 |
|------|---------------------------------------------|--------|------|------|
| GET | `/business/flow/instance/{id}/pdf` | `business:flow:instance:read` | A4 纵向 | 审批单：单据信息 + 审批轨迹时间轴 + 每级意见 + 各级签字位 |

- **权限复用审批单详情的读码**，与 `GET /business/flow/instance/{id}` 是同一个码。
- **可见性不比详情页松**（`ensure_visible`，与 `handler::instance_detail` 逐条同口径）：
  发起人 / 审批人（含已审、抄送）/ 持 `business:flow:admin` 者。打印件更容易外流，
  有读码但与本单无关 → **403** `FLOW_FORBIDDEN`（文案点名「你与这张审批单无关」）。
- 响应是二进制（不是 `{code,msg,data}`）：

```
200 application/pdf
Content-Disposition: attachment; filename="审批单-FLW-20261108-000001.pdf"; filename*=UTF-8''…
Cache-Control: no-store
```

- 失败分支：`id` 不是 UUID → **400** `66006`；不存在 / 已软删 → **404** 点名「审批单」；
  未登录 → 401；模板 / 渲染引擎异常 → 422 / 503 / 504（错误码同 PDF 底座）。

## 二、代码落点

```
backend/src/business/pdf/
  templates/approval.html.tera   审批单版式（include_str! 编译进二进制）
  template.rs                    ApprovalContext / ApprovalTrace / ApprovalSignSlot
                                 + render_approval / approval_footer / approval_pdf_opts
                                 + sample_approval（三种 ARCH-8 标记各来一条，单测与联调用）
backend/src/business/flow/
  instance_pdf.rs                取数（model::timeline / nodes_of）+ 纯口径（级次 / 标记 /
                                 金额 / 签字位）+ HTTP 入口 + 导出审计 + 24 条单测
```

分层同前三批：`business/pdf` 不读业务数据（只出模板与渲染参数），
`business/flow` 自己查库 → `build_context()`（**纯函数**）→ `render_approval` → `doc_pdf_render::render_document`。

`build_context` 之所以是纯函数，是为了让「级次编号 / 标记判定 / 签字位过滤」这三件最容易出错的逻辑
不依赖数据库就能单测（18 条纯逻辑用例）；真库只用 5 条用例钉「取数口径 + 可见性 + 审计」。

## 三、版式

```
页眉：公司名 + 「审 批 单」
单据信息：单据类型 / 单号 / 业务单号 / 金额 / 申请人 / 申请时间 / 流程(vN) / 状态 / 完成时间 / 标题
         （退回过的单据多一行「退回轮次」）
结论横幅：审批结论：已通过 / 已驳回 / 已取消 / 审批中（括号里是 result_comment）
审批轨迹：级次 | 节点 | 审批人 | 结果 | 时间 | 意见
         （每条待办一行；ARCH-8 痕迹另起一行 tag-row，不拼进「意见」列）
审批人签字：级次 | 节点 | 审批人 | 签名（留空手写） | 日期（留空）
说明：本表轨迹按「退回轮次 → 落待办时间」排序…
页脚：审批单 <单号> … 第 X 页 / 共 Y 页（CDP footerTemplate）
```

轨迹表的列宽是**实测调出来的**（A4 纵向，页面宽 174pt 可用）：

| 列 | 宽度 | 备注 |
|----|------|------|
| 级次 | 40pt | 「第 1 级」+「第 2 轮」上下排 |
| 节点 | 74pt | 节点名快照 |
| 审批人 | 80pt | 「PDF05 部门经理 / PDF05 审批小组 u_l1a」会折 2-3 行，62pt 时把「时间」挤到贴边 |
| 结果 | 40pt | 通过 / 驳回 / 跳过 / 已转办 / 已作废 / 待审批 |
| 时间 | 76pt | `2026-11-08 22:26` 正好一行；**70pt 时最后一位顶到列边框**（145 实拍） |
| 意见 | auto | 审批人原文，不截断 |

## 四、级次怎么定（`node_key` + `round`）

一条待办 = 一行轨迹（会签 3 人就 3 行，打印件要能逐人签字）。
级次**不是**节点定义顺序，而是 `(node_key, round)` 的**首次出现顺序**：

- 退回过一次的单据，同一节点有 `round=1` / `round=2` 两批待办 —— 它们是**两个级次**
  （「这一级走过两遍」正是审计要看的信息），第二轮的行另打「第 N 轮」角标；
- 并行网关的节点在 `timeline` 里可能乱序到达，用「首次出现序」而不是定义序，编号才稳定；
- 没有待办（刚发起就撤 / 节点都没激活）→ 表体打「暂无审批记录（本单尚未生成任何审批待办）」。

`model::timeline` 的排序键是 `round, created_at, id` —— 同一毫秒落库的多条待办靠 `id`（UUIDv7）定序，
不会因为批量插入顺序抖动导致同一张单两次打印行序不同。

## 五、ARCH-8 降级标记（本卡「特别要求」）

三种痕迹**全部来自 `biz_flow_task` 的留痕列**，不靠猜、不靠比对节点定义：

| 场景 | 判据（库里的列） | PDF 上打的字 | 配色类 |
|------|------------------|--------------|--------|
| 自审跳过 | `status='SKIPPED' AND is_self_approval` | `已跳过（自审）` | `is-skip` |
| 内控排除（制单人 / 发起人本人） | `status='SKIPPED' AND NOT is_self_approval` | `已跳过（内控）` | `is-skip` |
| 升级（P3-02） | `escalate_reason` 非空 | `已升级至 <目标角色名>（<原因>：<原角色码>）`，如 `已升级至 PDF05 上级总监（原角色无人：PDF05_EMPTY）` | `is-escalate` |
| 兜底（P3-03） | `assigned_by_rule='FALLBACK'` | `兜底审批人` | `is-fallback` |
| 委托（P3-04） | `assigned_by_rule='DELEGATED'` | `委托代批` | `is-delegate` |

`escalate_reason` 的码 → 文案（`escalate_reason_label`，**改码即破坏契约**，与前端
`flow-progress-tasks.ts::escalateReasonLabel` 同语义）：

| 码 | PDF 上的原因字样 |
|----|------------------|
| `NO_MEMBER` | 原角色无人 |
| `SKIP_SELF_APPROVAL` | 原角色自审剔光 |
| 其他 | 原角色无可用审批人 |

**判定顺序有语义，别调**（`downgrade_tag`）：

1. 先判派单规则 `FALLBACK` / `DELEGATED` —— 它们与升级互斥（升级成功才不会走兜底），
   先判它们才不会把「兜底人」误标成「升级」；
2. 再判 `escalate_reason`（升级）；
3. 最后判 `status='SKIPPED'` —— 被剔掉的人按设计 `escalate_reason` 恒为 `None`，不会互相遮住。

标记**独立成行**（`tr.tag-row`），不拼进「意见」列：意见是审批人写的原文，
系统字样混进去会让审计分不清「谁说的」。

另外：`is_auto=true` 且非 SKIPPED 的待办（超时自动通过 / 空审批人跳过）在意见后加
`系统代点` 角标 —— 否则打印件上「谁批的」会被误读成真人点的。

## 六、签字位只收「真要签字的人」

签字表按**级**聚合审批人，但姓名要先过 `signable(status)`：

| 状态 | 进签字位？ | 理由 |
|------|-----------|------|
| `APPROVED` / `REJECTED` | ✅ | 真审过这一级 |
| `PENDING` | ✅ | 单还在走，这一级确实要他签 |
| `SKIPPED` | ❌ | 自审 / 内控剔掉的人**没有审过**这张单 |
| `TRANSFERRED` | ❌ | 已转办出去，签的是接手人 |
| `CANCELLED` | ❌ | 或签被别人抢先后作废的票 |

某一级一个人都不剩时**仍保留该行**，姓名打 `—（本节点无人签字）` ——
「这一级走过了但没人签」本身就是审计结论，删掉行等于把问题藏了。

签名栏与日期栏一律**留空**，模板不预印任何值，页脚上方有说明句
「签名栏与日期栏一律留空，由各级审批人在纸质件上现场手写签名（打印件不作电子签名效力）」。

## 七、金额与时间口径

- 金额：`form_data.amount` → `form_data.total_amount`（与 `self_approval::amount_of` **同一键序**）。
  取不到印 `—`，**不印 `0.00`** —— 一张没填金额的单据说「金额 0.00」是假信息。
- 精度：JSON number 先 `to_string()` 再 `Decimal::from_str`。`serde_json::Number` 底层是 f64，
  直接 `from_f64` 会让 `1.005` 打成 **1.00**（少一分）；`Number::to_string()` 走最短可回环表示，
  出来的就是 `"1.005"`。
- 时间：库里的 `timestamptz` 一律按**北京时间**（`doc_pdf_model::beijing_time`）印成 `yyyy-MM-dd HH:mm`；
  轨迹行取 `completed_at`，未完的取 `created_at`（`—` 只在两者皆空时出现）。

## 八、导出审计

导出是 GET，中间件不记 → 渲染成功后显式补一行 `operation_logs`：

| 字段 | 值 |
|------|-----|
| `operation` | `EXPORT_FLOW_INSTANCE_PDF` |
| `target_type` | `biz_flow_instance` |
| `target_id` | 实例 id |
| `detail.doc_type` | `FLOW_INSTANCE` |
| `detail.source` | `PDF-05` |
| `detail.trace_count` | 轨迹条数（**不是** row_count） |
| `detail.amount` | 单据金额（保留 2 位） |
| `detail.render_ms` | 渲染耗时 |

写日志失败只记 `tracing::error`，**不改已生成的响应**（用户拿到 PDF 是第一位）。
`write_export_log` 有一条坑：`source` 的默认值是 `PDF-03`，调用方没给才会兜底 ——
审批单必须显式传 `PDF-05`（单测 `export_writes_operation_log_row` 钉死）。

## 九、验收与证据

`docs/evidence/20261108-pdf05/`：`e2e145.py`（脚本）+ `raw/e2e_run.txt`（**127 项断言 0 失败**）
+ `raw/pdftotext_*.txt` / `pdfinfo_*` / `pdffonts_*` + `sample/approval-*.pdf` 与首页 PNG。

| 卡面验收 | 场景 | 证据 |
|---------|------|------|
| ① 普通 2 级审批 → 两行轨迹 + 意见 | S1（部门经理 → 总经理，AND 直线流程） | `sample/approval-2levels.pdf`，「第1级/第2级」两行 + 两条意见原文 |
| ② 自审跳过 → 「已跳过（自审）」 | S2（发起人本人是节点角色成员，`self_approval=SKIP`） | `sample/approval-self-skip.pdf`，tag-row「⚠ 已跳过（自审）」；签字位不含发起人 |
| ③ 升级 → 「已升级至 XX」 | S3（节点角色 0 成员 → 沿 `escalation_role_id` 上溯到上级总监） | `sample/approval-escalation.pdf`，「已升级至 PDF05 上级总监（原角色无人：PDF05_EMPTY）」 |
| ④ 驳回 → 显示驳回意见 | S4（二级驳回并终止） | `sample/approval-rejected.pdf`，结论「已驳回」+ 驳回意见原文 |
| 特别要求③ 兜底 → 「兜底审批人」 | S5（角色 0 成员 + 无上级 → 节点兜底人） | `sample/approval-fallback.pdf`，tag-row「⚠ 兜底审批人」 |
| 负面 | S6 | 未登录 401 / 无权限 403 / 无关 403 / 非法 id 400 / 不存在 404 |
| 审计 | S7 | 5 条 `EXPORT_FLOW_INSTANCE_PDF`，`source=PDF-05`，每行带单号 |

**这些数据都是真引擎产生的**（建流程 → 存图 → 发布 → 发起 → 审批），E2E 脚本不往
`biz_flow_*` 写任何业务行，只用 `psql` 读事实做断言。

## 十、本卡踩过的坑

1. **时间列 70pt 顶到边框**：`2026-11-08 22:26` 在 70pt 下最后一个字符压线 → 76pt。
   列宽必须拿**真实渲染图**看，pdftotext 抽出来的换行位置骗人。
2. **审批人列 62pt 太窄**：「PDF05 部门经理 / PDF05 审批小组 u_l1a」折 3 行把整行撑高 → 80pt。
3. **签字位把「被自审跳过的人」也列上了**：第一版按「这一级出现过的人」聚合姓名，
   打印出来像是他也签过 → 改为按 `signable(status)` 过滤（见 §6）。
4. **审计 `source` 被公共件写成 PDF-03**：`doc_pdf_model::write_export_log` 的默认值先于调用方取值，
   审批单导出在 `operation_logs.detail.source` 里显示成 PDF-03 → 改为「调用方给了就不覆盖」，
   由调用方显式传 `PDF-05`（改了公共件，四批单据的调用点都复验过）。
5. **证据脚本 `EVID` 取脚本父目录**：脚本放在 `docs/evidence/<卡>/` 下时，
   `dirname(dirname(__file__))` 会把 PDF 写到上一级；改为 `EVID = 脚本所在目录`，
   并且 `export_pdf` 成功即顺手落 `pdftotext/pdfinfo/pdffonts/首页 PNG`，证据不再靠事后补 collect。
