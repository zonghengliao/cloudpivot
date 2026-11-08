# PDF-05 审批单 PDF —— 145 实测证据

> 接口：`GET /api/business/flow/instance/{id}/pdf`（`business:flow:instance:read`）
> 文档：[docs/PDF单据模板-审批单.md](../../PDF单据模板-审批单.md)
> 采集时间：2026-11-08 23:06（CST）

## 一、被测二进制

| 项 | 值 |
|----|----|
| 提交 | `07c39d4`（`style(pdf): 审批轨迹时间列 70→76pt`） |
| 构建 | `cargo build --bin cloudpivot -j 1`（dev profile，`/root/pdf04-app/backend`） |
| 二进制 md5 | `5d06f32e4c7fc8c1cc42f681da245159` |
| 监听 | `127.0.0.1:8079`（145 隔离实例，库 `cloudpivot_pdf05`，redis db10） |
| 迁移 | 空库首启 **应用 281 个迁移**；随后每次重启 **应用 0 个**（含 23:05 这次换二进制重启） |

只动 145：`/root/pdf04-app` 代码目录 + 独立库 `cloudpivot_pdf05` + 端口 8079。
**未碰 144**，也未动 145 上 8033 的 `cloudpivot-test`。

## 二、怎么复现

```bash
# 145 上
cd /root/pdf05-evidence
BASE=http://127.0.0.1:8079 DB=cloudpivot_pdf05 ENVF=/root/pdf05.env \
  python3 e2e145.py > raw/e2e_run.txt 2>&1
```

脚本用真实引擎造数据（建流程 → 存图 → 发布 → 发起 → 审批），**不往 `biz_flow_*` 写任何业务行**，
只用 `psql` 读事实断言；PDF 事实走 `pdftotext / pdfinfo / pdffonts`，并把文本层、元信息、字体、
首页 PNG 一并落进 `raw/` 与 `sample/`。

## 三、结果

```
==== 汇总：127 项，失败 0 项（10s）
```

| 场景 | 覆盖的卡面要求 | 断言要点 |
|------|----------------|----------|
| S1 `approval-2levels` | 验收① 普通 2 级审批 | 「第1级/第2级」两行 + 两条意见原文 + 节点名 + 审批人 + 结论「已通过」+ 签字位留空 + 无降级标记行 |
| S2 `approval-self-skip` | 验收② 自审跳过 | tag-row「⚠ 已跳过（自审）」+ 审批人本人 `SKIPPED` 留痕原因 `self_approval=SKIP` + **签字位不含被跳过的人** |
| S3 `approval-escalation` | 验收③ 升级 | 正则钉死 `已升级至PDF05上级总监（原角色无人：PDF05_EMPTY）`；库侧核对 `assigned_by_rule=ESCALATED` / `escalate_reason=NO_MEMBER` / `escalated_from_role=PDF05_EMPTY` |
| S4 `approval-rejected` | 验收④ 驳回 | 结论「已驳回」+ 驳回意见原文 + 一级通过意见也在 |
| S5 `approval-fallback` | 特别要求③ 兜底 | tag-row「⚠ 兜底审批人」+ `assigned_by_rule=FALLBACK` + 兜底人意见 |
| S6 负面 | 权限 / 存在性 | 未登录 401、无权限码 403（点名 `business:flow:instance:read`）、有读码但无关 403（文案点「无关」）、非法 id 400、不存在 404（点名「审批单」）、导出不产生审批行 |
| S7 审计 | 导出留痕 | 本轮 5 条 `EXPORT_FLOW_INSTANCE_PDF`，`target_type=biz_flow_instance`，每行带单号，`detail.source=PDF-05` |

PDF 事实：单页 A4 纵向、字体已内嵌（`pdffonts` 有 `emb=yes`）、`pdftotext` 抽得出正文（不是图片 PDF）、
`Content-Type: application/pdf` + `Content-Disposition: attachment` + `Cache-Control: no-store`。

## 四、文件清单

```
raw/e2e_run.txt                     127 项断言的完整输出（含每项 [PASS]）
raw/curl_example.txt                真实 token curl 实测：200 application/pdf 头 + 401/400/404 分支 + 落盘与 pdftotext 解析
raw/operation_logs.txt              S7 抓到的审计行（operation|target_type|doc_no|trace_count|amount|source）
raw/pdftotext_approval-*.txt        五张 PDF 的文本层（核对轨迹 / 标记 / 签字位）
raw/pdfinfo_approval-*.txt          页数 / 页面尺寸（A4 纵向）
raw/pdffonts_approval-*.txt         字体内嵌情况
sample/approval-*.pdf               五张样例 PDF
sample/approval-*-p1-1.png          首页渲染图（用眼看版式，别只信 pdftotext）
raw/pdf05_build_145.sh              构建脚本
raw/pdf05_start_145.sh              起停脚本
raw/pdf05_collect_145.sh            早期的事后取证脚本（现已并入 e2e145.py，保留作历史）
raw/clean_old_audit.sql             清历史审计行用的一次性 SQL
raw/full_lib_test.txt               22:11（commit 2f17908）全量 lib 记录：3762 passed / 3 failed
raw/full_lib_test_07c39d4.txt       23:06（commit 07c39d4）全量 lib 复跑：3779 passed / 3 failed（同 3 条既有红）
e2e145.py                           本卡 E2E 脚本
```

## 五、诚实说明（不是全部绿灯）

1. **全量 `cargo test --lib`（不设 `ERP_DB_URL`）= 3779 passed / 3 failed**，3 条全是既有的
   `business::flow::linkage::admin::tests::catalog_triggers_carry_wired_flag` 家族：
   PDF-04 把 `PURCHASE_RETURN` 接进了联动，但该用例的白名单清单没同步更新 —— **属别的卡**，
   与本卡改动无关（本卡没碰 `linkage`）。
2. **本卡模块 24 条用例全绿**（19 纯逻辑 + 5 真库），真库那轮用
   `ERP_DB_URL=postgres://…@127.0.0.1:15432/cloudpivot_pdf04`（ssh 隧道到 145），
   `SKIP` 计数 0（确实连库跑的，不是静默跳过）。
3. **`raw/full_lib_test.txt`（22:11，commit `2f17908`）= 3762 passed / 3 failed**、本次复跑
   `raw/full_lib_test_07c39d4.txt`（23:06，commit `07c39d4`）= **3779 passed / 3 failed**，
   两次的 3 条红完全一致，都是 `business::flow::linkage` 的三条白名单用例，与本卡改动无关。
   用例总数从 3762 涨到 3779 是别卡（ATT-09 等）在本卡期间合入的。
4. **别拿卡片专用库跑全量 lib**：本卡试过一次 `cargo test --lib` + `ERP_DB_URL=…/cloudpivot_pdf04`
   + 4 线程 → 3023 passed / **759 failed**，失败全是「库里没有部门 / 状态机表未种」这类夹具缺失
   加并行互踩，与代码无关（该轮输出已删，不放进证据）。正确姿势是：全量 lib **不带** `ERP_DB_URL`，
   真库只跑本卡模块（5 条 db 用例，`--test-threads=1`）。
5. 前端本卡**无改动**，`pnpm typecheck` = 0、`pnpm build` = 0（全仓现状复验）。
6. 未部署：本卡只交代码 + 文档，部署由老六手工执行。
