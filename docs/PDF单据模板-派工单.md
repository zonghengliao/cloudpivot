# 单据 PDF 模板（五）：工作任务派工单（PDF-06）

> 卡：`PDF-06`。前四批见 [PDF单据模板-采购销售订单.md](./PDF单据模板-采购销售订单.md)（PDF-02）、
> [PDF单据模板-合同与出入库单.md](./PDF单据模板-合同与出入库单.md)（PDF-03）、
> [PDF单据模板-调拨盘点退货.md](./PDF单据模板-调拨盘点退货.md)（PDF-04）、
> [PDF单据模板-审批单.md](./PDF单据模板-审批单.md)（PDF-05）；
> 底座（Chromium / 字体 / `business/pdf` 分层）见 [PDF导出基础设施.md](./PDF导出基础设施.md)。
>
> 本文件讲**派工单**：数据在 `biz_task`（工作任务模块），版式是「一页任务信息 + 描述全文 +
> 检查项 + 签字栏」，与前四批「多行明细 + 合计」的单据不同 —— 它的正文是**一整段文本**，
> 而且必须能跨页。

## 一、接口与权限

| 方法 | 路径（`/api` 前缀由 `[router].prefix` 注入） | 权限码 | 版式 | 说明 |
|------|---------------------------------------------|--------|------|------|
| GET | `/business/task/{id}/pdf` | `business:task:read` | A4 纵向 | 派工单：任务标题 + 12 个任务信息字段 + 描述全文 + 检查项 + 签字栏 |

- **权限复用任务详情的读码**（`business:task:read`），与 `GET /business/task/{id}` 同一个码；
  handler 里再判一次（`claims.has_permission`），单测能直接证明「无码 → 403」。
- **可见性与详情页逐条同口径**：走同一条 `model::find_detail`（`数据范围 ∪ 参与关系`）。
  看不见 → **404 + 63001**（`TASK_NOT_FOUND`），**不是 403** —— 403 会告诉调用者
  「这个任务 ID 存在」，而任务 ID 不能变成可枚举资源。打印件更容易外流，PDF 不比详情页松。
- 响应是二进制（不是 `{code,msg,data}`）：

```
200 application/pdf
Content-Disposition: attachment; filename="派工单-TSK-202611-000024.pdf"; filename*=UTF-8''…
Cache-Control: no-store
```

- 失败分支：`id` 不是 UUID → **400 + 10001**；任务不存在 / 已软删 / 对本用户不可见 →
  **404 + 63001**；未登录 → 401；缺权限码 → 403（中间件 `PERMISSION_DENIED`，handler
  兜底 `TASK_FORBIDDEN` 并点名 `business:task:read`）；模板 / 渲染引擎异常 → 422 / 503 / 504。

## 二、代码落点

```
backend/src/business/pdf/
  templates/task.html.tera    派工单版式（include_str! 编译进二进制）
  template.rs                 TaskContext / TaskChecklistItem / TaskSignCell
                              + render_task / task_footer / task_pdf_opts / sample_task
backend/src/business/task/
  task_pdf.rs                 取数 + 纯口径（优先级 / 状态文案 / 协作人 / 派工人 /
                              检查项 / 签字栏）+ HTTP 入口 + 导出审计 + 13 条单测
  task_model_query.rs         新增 list_children_for_document（本单全部子任务，见 §五）
```

分层与前四批一致：`business/pdf` 不读业务数据（只出模板与渲染参数），
`business/task` 自己查库 → `build_context()`（**纯函数**）→ `render_task` →
`doc_pdf_render::render_document`。

`build_context` 是纯函数，是为了让「优先级文案 / 协作人过滤 / 检查项跳过取消 / 签字栏口径」
这四个最容易出错的点不连库就能单测（10 条纯逻辑用例）；真库只用 3 条钉
「取数口径 + 可见性 + 审计」。

## 三、版式

```
页眉：公司名 + 「工 作 任 务 派 工 单」+ 任务号
任务标题：整行大字（12pt 边框块，长标题自然折行，不与其他字段抢宽度）
任务信息：任务号 | 优先级        截止日期 | 负责人
          协作人 | 所属项目      状态     | 创建人
          分派人 | 计划开始      预估工时 | 部门        （每行 2 组）
任务描述：原文照印（pre-wrap 保留换行与缩进，可跨页）
检查项：  完成(空白框) | 序号 | 检查项 | 负责人 | 截止日   （无子任务则整节不出现）
签字栏：  派工人 | 接收人 | 完成确认 | 日期   （签名与日期一律留空手写）
页脚：派工单 <任务号> … 第 X 页 / 共 Y 页（CDP footerTemplate）
```

### 为什么任务信息是「每行 2 组」而不是 3 组

第一版按 3 组排版（标签 62pt + 值约 98pt），145 实拍发现
「PDF06 负责人张工」「PRJ-PDF06 PDF-06 派工单实测项目」这类值**每个都折成两行**，
负责人栏还和相邻栏在文本层交错（`pdftotext -layout` 把两列的行拼在一起）。
改 2 组后值区约 179pt，姓名与项目名单行可读，12 个字段仍在一页内。

> 这正是 PDF-05 记下的教训：**列宽要拿渲染图看**，`pdftotext` 抽出来的换行位置会骗人。

### 描述为什么允许跨页、且不渲染 Markdown

- **允许跨页**：描述可能是几十行。`.desc` 块**刻意不加** `break-inside: avoid` ——
  加了 Chromium 会把整块挪到下一页，首页留一大片空白（145 实测 120 行描述会变成
  1 页空白 + 正文从第 2 页起）。只把「任务描述」这个小标题粘在首段
  （`.section-title { break-after: avoid }`）。单测 `task_long_description_is_verbatim`
  直接断言 `.desc` 的 CSS 里**没有** `break-inside`。
- **不渲染 Markdown**：`biz_task.description` 存的是纯文本 / Markdown 源文。派工单是
  发到现场的纸质凭证，**必须与库里逐字一致**：把 `##` 渲染成标题、`**` 渲染成加粗，
  打印件就与系统内容出现差异，出纠纷时无法对账（审计比对的是文本层）。
  因此只做 HTML 转义 + `white-space: pre-wrap`。145 实测 S4 场景钉死
  「`## 现场要求` / `**加粗**` / 缩进行」在文本层原样保留。

## 四、检查项 = 直接子任务（且**不印状态**）

任务模块没有独立的 checklist 表（`V0100` 的 7 张表里没有），产品语义上
「拆解出来的检查项」就是子任务（任务模板的 `subtasks` jsonb 一键建任务时也落成子任务，
见 `business/task/template.rs`）。所以：

| 规则 | 理由 |
|------|------|
| 取**本单全部**直接子任务（`list_children_for_document`，**不加数据范围过滤**） | 子任务是这张单的内容；按可见性过滤会让派工单静默漏项（见 §五） |
| 跳过 `CANCELLED` | 已取消的子任务不再是接收人要干的活，印上去只能勾一个不存在的事项 |
| 序号**连续**（跳过取消项后不留空洞） | `1,3,4` 会被现场当成「漏印了一项」 |
| 每条一个 **CSS 画的 9pt 空白方框**（不印 `☐` 字符） | 字体缺字会变豆腐块；卡面验收②要求「空白供手填」 |
| **不印子任务状态** | 纸质件是接收人的确认凭证。预印「已勾」会让「系统里标完成」与「纸上已勾」混为一谈 —— `TaskChecklistItem` 结构体里干脆没有 status 字段，模板想印也印不出来（与盘点表空白三栏同一手法） |

## 五、唯一一处刻意偏离「读路径一律带可见性」

`model::list_children_for_document` **不加** `VISIBLE_OR_TAIL`。这是本模块唯一的破例，
理由写在函数文档里，摘要：

- `list_children`（详情页用）按可见性过滤，语义是**浏览任务列表**；
- 派工单是**一张单**的打印件，打印它的人已经过 `find_detail`（看不见父任务 → 404），
  子任务就是这张单的检查项。按人过滤会让纸质清单**静默少行**，接收人照残单干活；
- 因此该函数只给导出链路用，且调用方**必须先校验父任务可见**（`load_context` 里
  `find_detail` 就在它前面）。其它任何「列出任务」的路径仍必须走
  `list_children` / `page_visible`。

145 实测（S2）：子任务负责人是别人 → 仍印在派工单上；
真库用例 `context_from_real_database` 把子任务 A 的负责人设成**数据范围外**的用户，
断言 `checklist[0].owner == "PDF06 stranger"`。

## 六、签字栏

固定 4 格，`hint` 是**预印的灰色小字**（谁该签 / 验收口径），签名与日期区一律留空：

| 栏 | 预印内容 | 口径 |
|----|----------|------|
| 派工人 | `assigner_name`；**自建任务（`assigner_id IS NULL`）印创建人** | 自建任务的派工人就是创建人自己，印「—」会让签字栏看起来缺一个人 |
| 接收人 | `owner_name`（负责人） | — |
| 完成确认 | `need_review = true` → `验收人：<创建人>`；`false` → `无需验收（完成后直接归档）` | 与状态机 `REVIEW → DONE` 的验收人一致；不编一个「验收人」出来 |
| 日期 | 空 | 现场手写 |

## 七、导出审计

导出是 `GET`，`middleware::operation_log` 只记写方法，所以渲染成功后**显式**补一行
`operation_logs`（失败只记 error，不改已生成的响应）：

```
operation    = EXPORT_TASK_PDF
target_type  = biz_task
target_id    = <任务 id>（target_id 是 varchar，psql 里别写 ::uuid）
detail       = {doc_type: TASK, source: PDF-06, checklist_count, render_ms,
                doc_id, doc_no, filename, bytes, duration_ms}
```

`detail.source` 由调用方显式给 `PDF-06`（公共件 `write_export_log` 的默认值是 `PDF-03`，
PDF-05 已改成「给了就不覆盖」）。

## 八、再加一种「文本类」单据要改哪几处

1. `pdf/templates/<name>.html.tera` + `pdf/template.rs` 里 `*_TEMPLATE` / `*_NAME` 常量、
   `tera()` 里 `add_raw_template`、上下文结构体、`render_*` / `*_footer` / `*_pdf_opts` /
   `sample_*`；
2. 业务模块里新建 `<模块>_pdf.rs`：纯口径函数（**尽量多**）+ `build_context` +
   `load_context` + `export_*`（渲染收尾走 `doc_pdf_render::render_document`）+
   `#[nik::permission]` 路由 + `audit`；
3. 单测：纯口径（不连库）+ 真库（`ERP_DB_URL` / `TASK_DB_URL`，未设则 SKIP 并在汇报里说明）；
4. 文档 + 145 实测证据（`docs/evidence/<日期>-<卡号>/`）。

## 九、本卡踩过的坑（可复用）

1. **`pdftotext -layout` 会把相邻列的行交错**：签字栏四格「栏名 + 预印姓名」在
   `-layout` 下变成「四栏名一行、四姓名一行」，断言里 `派工人<姓名>` 永远匹配不上。
   用 `pdftotext -raw`（内容流顺序）对这类断言；`-layout` 只用于「这一行是否在页面上」。
2. **`biz_project` 没有 `project_code` 列**：任务卡的 `project_code` 来自
   `COALESCE(pj.project_no, '')`。写夹具 SQL 时别照字段名想当然。
3. **角色数据范围默认是 1「全部」**：e2e 里为了验「无关用户 404」，创建角色必须显式
   `data_scope: 4`（仅本人），否则所有用户都能看全公司任务，404 断言假绿。
4. **用 shell `. env` 读权限会做变量展开**：env 文件里有 `$` / `!` 时值会被改掉，
   导致「应用里 admin 密码」与「脚本里的密码」不一致。证据脚本读 env 文件请按文本解析。
5. **kill 进程别用前缀匹配**：同机存在 `./target/debug/cloudpivot-rev`（别的隔离实例），
   `pkill -f "target/debug/cloudpivot"` 会连别人的实例一起杀掉；要用
   `target/debug/cloudpivot$` 锚定 + 校验 `/proc/<pid>/cwd`。
6. **`pkill` 没杀掉旧进程 = 你测的是旧代码**：进程用相对路径 `./target/debug/cloudpivot`
   启动时，`pkill -f` 里写绝对路径匹配不上，新进程 `EADDRINUSE` 退出，端口上还是旧二进制
   —— 现象是「代码改了、PDF 一点没变」。重启后必须校验 `md5sum` 与 `/proc/<pid>/cwd`。
