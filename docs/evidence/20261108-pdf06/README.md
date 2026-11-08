# PDF-06 工作任务派工单 PDF —— 145 实测证据

> 接口：`GET /api/business/task/{id}/pdf`（权限码 `business:task:read`）
> 文档：[docs/PDF单据模板-派工单.md](../../PDF单据模板-派工单.md)
> 采集时间：2026-11-09 00:22（CST）

## 一、被测二进制

| 项 | 值 |
|----|----|
| 提交 | `af65eb3`（`test(pdf): 派工单真库夹具修正`；运行时代码与 `983b197` 相同） |
| 构建 | `cargo build --bin cloudpivot -j 4`（dev profile，`/root/pdf06-app/backend`） |
| 二进制 md5 | `0b26a689f6a7053abea22a09141883a9` |
| 监听 | `127.0.0.1:8080`（145 隔离实例，库 `cloudpivot_pdf06`，redis db11） |
| 迁移 | 该库从空库起共应用 **283** 条（`schema_migrations` 283 行）；本卡**未新增/未修改**任何迁移，最后一次换二进制重启日志为「数据库迁移完成：应用 0 个迁移」 |

只动 145：`/root/pdf06-app`（worktree）+ 独立库 `cloudpivot_pdf06` + 端口 8080。
**未碰 144**，也未动 145 上 `cloudpivot-test`（8033）。

## 二、怎么复现

```bash
# 145 上
cd /root/pdf06-evidence
BASE=http://127.0.0.1:8080 DB=cloudpivot_pdf06 ENVF=/root/pdf06.env \
  python3 e2e145.py > raw_e2e.txt 2>&1
```

脚本用**真实 HTTP + 真实 Chromium** 造数据（建角色 / 用户 → 建项目 → 建任务 → 建子任务 →
取消一条 → 加协作人 → 导 PDF），PDF 事实走 `pdftotext / pdfinfo / pdffonts` 断言，
并把文本层 / 元信息 / 字体 / 首页 PNG 一并落进 `raw/`、`sample/`。

## 三、结果

```
==== 汇总：96 项，失败 0 项（7s）
```

| 场景 | 覆盖的卡面要求 | 断言要点 |
|------|----------------|----------|
| S1 普通任务（无检查项） | 验收① 完整描述 | 标题「工 作 任 务 派 工 单」、任务号、任务标题、`P1（高）`、截止日、负责人、协作人、`PRJ-PDF06` + 项目名、状态「待开始」、创建人 / 分派人（admin）、计划开始、`16.0 小时`、描述逐行原文；签字栏 4 格 + 「验收人：超级管理员」；**无检查项时整节不出现** |
| S2 带检查项（3 个子任务，1 个已取消） | 验收② 空白勾选框 | 「检查项（共 2 项，勾选框留空，由接收人逐项手填）」；甲 / 丙在、**乙（已取消）不印**；文本层**没有** `☑ √ ☐` 任何字形（方框是 CSS 画的）；不印子任务状态；无需验收口径 |
| S3 长描述（120 行） | 验收③ 跨页正常 | `Pages = 4`；120 行**一行不少**；第 1 页到第 028 行、第 2 页从第 029 行接着排（描述块没有被整体挤到下一页） |
| S4 原文照印 | 描述「完整正文」 | `## 现场要求` / `- 列表` / `**加粗**` / `*斜体*` / 四空格缩进行**原样保留**，文本层没有 `<h2` 之类的渲染结果 |
| S5 签字栏口径 | 派工人 / 接收人 | 自建任务（`assigner_id IS NULL`）派工人印**创建人**、不印「—」；协作人加入后**也能导出**，且出现在「协作人」栏 |
| S6 负面 | 权限 / 存在性 | 未登录 401；无 `business:task:read` → 403 且文案点名权限码；无关用户 → **404 + 63001**（不暴露存在）；非法 id → 400 + 10001；不存在 → 404 + 63001；JSON 路径取不到 PDF |
| S7 审计 | 导出留痕 | `operation = EXPORT_TASK_PDF`、`target_type = biz_task`、`detail.source = PDF-06`、`doc_no` 对得上；带检查项那张单 `checklist_count = 2` |
| S8 真 token curl | 实测留证 | 200 头（`application/pdf` + `content-length` + RFC5987 中文名 + `no-store`）+ 401 / 400 / 404 分支 |

PDF 事实：A4 纵向（595.28 × 841.89 pt）、字体全部内嵌（`pdffonts` 无 `emb=no`）、
`pdftotext` 抽得出正文（不是图片 PDF）。

## 四、文件清单

```
raw_e2e.txt                          96 项断言的完整输出（含每项 [PASS]）
raw/curl_example.txt                 真实 token curl：200 头 + 401/400/404 分支
raw/curl_200.pdf                     curl 落盘的 PDF（可独立打开）
raw/operation_logs.txt               S7 抓到的审计行（operation|target_type|source|doc_no|checklist_count）
raw/pdftotext_*.txt                  六张 PDF 的文本层（layout 模式）
raw/pdftotext_raw_task-self-built.txt  签字栏「栏名 + 预印姓名」用 -raw 取（-layout 会列交错）
raw/pdfinfo_*.txt                    页数 / 页面尺寸
raw/pdffonts_*.txt                   字体内嵌情况
sample/task-normal.pdf               普通任务（无检查项）
sample/task-checklist.pdf            带检查项（2 项，含空白方框）
sample/task-long.pdf                 长描述 4 页
sample/task-verbatim.pdf             Markdown 记号原文照印
sample/task-self-built.pdf           自建任务（派工人 = 创建人）
sample/task-collab-view.pdf          协作人视角导出（协作人栏有名字）
sample/*-p1-1.png / task-long-p2-2.png  首页 / 第 2 页渲染图（用眼看版式，别只信 pdftotext）
e2e145.py                            本卡 E2E 脚本
```

## 五、诚实说明（不是全部绿灯）

1. **全量 `cargo test --lib`（不设 `ERP_DB_URL`）= 3797 passed / 4 failed**。
   4 条红**全部在基线复现**（把本卡 4 个文件临时移出 worktree 后重跑这两组用例，
   同样 4 条红），与本卡改动无关：
   - `business::flow::linkage::admin::tests::catalog_triggers_carry_wired_flag`
   - `business::flow::linkage::tests_wiring::trigger_specs_cover_whitelist_in_order`
   - `business::flow::linkage::tests_wiring::wired_triggers_match_call_sites`
     （以上 3 条属 LNK-12：`PURCHASE_RETURN` 接线后白名单 / 调用点清单没同步）
   - `shared::startup_guard::tests::merge_semantics_or_base_and_profile`
     （启动守卫缓存合并语义，与 PDF 无关）
2. **本卡模块 13 条用例全绿**（10 纯逻辑 + 3 真库）。真库那轮用
   `ERP_DB_URL=postgres://…@127.0.0.1:15432/cloudpivot_pdf06`（ssh 隧道到 145），
   3 条 `db_tests::*` 均为 `ok` —— **没有 SKIP**（连不上时它们只会 `println SKIP` 并返回）。
3. **本卡文件 clippy 0 告警**；`cargo clippy --lib --all-targets -- -D warnings` 全仓仍红
   2 处，均在别人代码（`erp/receivable.rs:800`、`erp/sales_order_model.rs:699`），
   与 PDF-05 交接里记的是同两处。
4. **一处操作事故（已记入 skill）**：换二进制重启时第一版脚本用
   `pkill -f "target/debug/cloudpivot"` 前缀匹配，误杀了同机其他卡的两个
   `./target/debug/cloudpivot-rev` 实例。脚本已改为 `target/debug/cloudpivot$` 锚定 +
   校验 `/proc/<pid>/cwd == /root/pdf06-app/backend`；本卡后续重启均只动自己的进程。
