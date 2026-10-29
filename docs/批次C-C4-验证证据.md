# 批次 C · C4 帮助文档 —— 验证证据（真机实测）

> 环境：172.18.10.144，`cloudpivot_test`，实例 `127.0.0.1:18086`（profile=test）。
> 隔离构建：`/root/c4-build`（clone 自 bare `/opt/git/cloudpivot.git`），`CARGO_TARGET_DIR=/root/c4-target`。
> 全部数字来自原始输出，未做任何"估算"。

## 0. 交付物

```
backend/migrations/business/V0082__create_help.{up,down}.sql        3 表
backend/migrations/business/V0083__help_indexes.{up,down}.sql       5 索引 + trgm 预案（注释态）
backend/migrations/business/V0088__seed_help_permissions.{up,down}.sql  11 权限码 + 菜单/授权
backend/src/business/help/{mod,model,dto,handler}.rs
backend/tests/batchC_c4_help.rs                                     11 个真库用例
backend/src/shared/error_code.rs                                    +62014..62019
backend/src/task/mod.rs                                             help_version_cleanup 接真实现
```

| 文件 | 行数 |
|------|------|
| `help/model.rs` | 1842 |
| `help/handler.rs` | 857 |
| `help/dto.rs` | 509 |
| `help/mod.rs` | 49 |
| `tests/batchC_c4_help.rs` | 780 |

## 1. 迁移应用（启动即应用，应用账号口径）

```
2026-10-29T01:55:20.283851Z  INFO alun_web::app: 数据库连接成功
2026-10-29T01:55:20.306446Z  INFO alun_web::app: 数据库迁移完成: 应用 0 个迁移
```

（首轮 09:51 的实例已应用 V0082/V0083/V0088；此后每次启动都是「应用 0 个迁移」= checksum 校验通过。）

```
 version |               name                | success
---------+-----------------------------------+---------
 88      | seed_help_permissions             | t
 87      | seed_announcement_read_permission | t
 84      | create_change_log                 | t
 83      | help_indexes                      | t
 82      | create_help                       | t
```

## 2. 迁移 down → up 往返（`cloudpivot_test`，应用账号 `cloudpivot` 执行）

| 步骤 | 三表 | 帮助索引 | 菜单节点 | 授权行 |
|------|------|----------|----------|--------|
| 回滚前 | 3 张都在 | 5 | 11 | 31 |
| `V0088.down` | 3 张都在 | 5 | **0** | **0** |
| `V0083.down` | 3 张都在 | **0** | 0 | 0 |
| `V0082.down` | **NULL/NULL/NULL** | 0 | 0 | 0 |
| `V0082.up` + `V0083.up` + `V0088.up` | 3 张都在 | **5** | **11** | **31** |

原始输出（节选）：

```
########## down 3/3：V0082（三表） ##########
||                     ← to_regclass 三列全 NULL：表已删
########## up 还原 3/3：V0088 ##########
 帮助菜单节点 | 11
 帮助授权行   | 31
```

## 3. 真库集成测试（11 个用例，`C4_DB_URL` 指向 cloudpivot_test）

```
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.81s
```

关键原始行：

```
[PASS] H-01 分类深度：第 4 层 62010 / 移动超深 62010 / 成环拒绝
[PASS] H-02 分类非空：62011 且分类仍在；文档软删后可删
[PASS] H-03 slug：重复 409 / 非法 400 / 软删后可复用
[PASS] H-04 发布必填时间：DB 拒绝裸改状态；正常发布成功
[PASS] H-05 版本快照：3 行 / 版本 1,2,3 / 内容各自正确
回滚前：version=7 历史版本=[7, 6, 5, 4, 3, 2, 1]
[PASS] H-06 回滚：内容回到 v1，version 7 → 8，历史 [7, 6, 5, 4, 3, 2, 1] → [8, 7, 6, 5, 4, 3, 2, 1]
[PASS] H-07 版本清理：25 → 20，删除 1..5，保留 6..25
[PASS] H-08 检索转义：% 命中 1 条、a_b 精确 1 条、反斜杠不报错
[PASS] H-09 检索状态过滤：普通用户 0 命中 / 管理员 status=DRAFT 命中 1
[PASS] H-10/11/12：片段纯文本、TOC 3 项（含 1 个子节点）、id 唯一且全部命中 HTML
造数：2000 行
第 1 轮：60 ms（命中 2000 条）
第 2 轮：59 ms（命中 2000 条）
第 3 轮：59 ms（命中 2000 条）
第 4 轮：58 ms（命中 2000 条）
第 5 轮：59 ms（命中 2000 条）
[PASS] 检索性能：2000 篇最坏 60 ms（阈值 100 ms）
```

## 4. 端到端 HTTP 验收（42 项断言，真实实例 18086）

```
================ 汇总 ================
PASS 42 / FAIL 0
e2e_exit=0
```

逐项原始输出：

```
[PASS] H-01d 第 4 层被拒（422 + 62010）  http=422 code=62010 msg=分类层级超过 3 层
[PASS] H-01e 移动导致超深（422 + 62010）  http=422 code=62010 msg=分类层级超过 3 层
[PASS] H-01f 移动到自己的子树（400 + 10001）  http=400 code=10001 msg=不能把分类移动到自己的子树下
[PASS] C-01 用户侧分类树只含有已发布文档的分支  http=200 树内分类数=1 c4-e2e 残留=[] 含 c4-perf=True
[PASS] D-01 新建草稿（version=0，DRAFT，自动摘要）  version=0 status=DRAFT summary=概述 这是帮助文档的概述，含关键词 anchor…
[PASS] XSS-01 新建回显 content_html 无真实危险结构  len=278 命中={}
[PASS] XSS-02 正文原始 HTML 被降级为转义文本（&lt;script）  片段=<h3 id="h-3-1">概述</h3>
[PASS] H-03a 重复 slug（409 + 62016）  http=409 code=62016 msg=文档 slug 已存在
[PASS] H-03b 非法 slug（400 + 62018）  http=400 code=62018 msg=文档标题 / 正文 / slug 格式或长度不合法
[PASS] H-05 连发 3 版 → 3 条快照、版本 3/2/1  版本=[3, 2, 1] notes=['发布 3', '发布 2', '发布 1']
[PASS] H-11 TOC 树形（3 个顶层，首个带 1 个子节点）  顶层=3 首项子节点=1
[PASS] H-12 同名标题 id 唯一（h-3-1/2/3）  ids=['h-3-1', 'h-4-1', 'h-3-2', 'h-3-3']
[PASS] H-11b 每个 TOC id 都出现在 content_html 里（锚点可用）  缺失=[]
[PASS] XSS-03 详情 content_html 无真实危险结构  len=278 命中={}
[PASS] H-06a 编辑已发布文档退回 DRAFT（架构 §3.6.2）  编辑后 version=4 status=DRAFT
[PASS] H-06b 回滚到 v1：内容 = v1  version=10 md 前 16 字=## 概述\n\n这是帮助文档的**
[PASS] H-06c 回滚生成新版本（version + 1）  回滚前 version=9 回滚后=10
[PASS] H-06d 历史版本一行不少  回滚前=[1..9] 回滚后=[1..10]
[PASS] H-02 有文档的分类删除（409 + 62011）  http=409 code=62011 msg=分类下仍有文档，不可删除
[PASS] H-02a 有子分类的分类删除（409 + 10003，不做级联删）  http=409 code=10003 msg=分类下仍有子分类，不可删除
[PASS] H-13 连点 10 次阅读量只 +1（按用户/日去重）  counted 次数=1 view 0 → 1
[PASS] H-08a 搜 『%』不全表命中（只命中含字面 % 的文档）  命中=['c4-e2e-percent']
[PASS] H-08b 搜 『a_b』只命中字面量（_ 不当通配）  命中=['c4-e2e-percent']
[PASS] H-08c 搜 『\』不报错  http=200 code=0
[PASS] H-10 snippet 纯文本（无 < / >）+ matched_terms 非空  snippet=概述 这是帮助文档的概述，含关键词 anchorword 与 alert('xss')。… matched=['anchorword']
[PASS] P-01 检索性能 2000 篇 < 100 ms（HTTP 端到端）  5 轮耗时(ms)=[62.6, 61.6, 61.3, 60.9, 60.6] 最坏=62.6ms 命中=2000
[PASS] H-14c 只读用户能看文档列表（全员可读）  total=2003
[PASS] H-14d 只读用户能看分类树  http=200 code=0
[PASS] H-14e 无 system:help:category:update → 403  msg=无权限访问，需要权限: system:help:category:update
[PASS] H-14f 无 system:help:doc:update → 403  msg=无权限访问，需要权限: system:help:doc:update
[PASS] H-14g 无 system:help:doc:create → 403  msg=无权限访问，需要权限: system:help:doc:create
[PASS] H-09 草稿检索：普通用户 0 命中 / 管理员 status=DRAFT 命中 1
[PASS] H-09b 草稿详情对普通用户 404（不泄露存在性）  http=404 code=62015
[PASS] Z-01 本轮 e2e 夹具已清理  残留=[]
```

清理核对（只删 `c4-` 前缀）：

```
 残留文档 | 残留分类 | 残留用户 | 全库文档数 | 全库版本数
----------+----------+----------+------------+------------
        0 |        0 |        0 |          0 |          0
```

## 5. 检索性能（2000 篇造数）

| 口径 | 结果 |
|------|------|
| 模型层（`model::search`，5 轮） | 58–60 ms |
| HTTP 端到端（含鉴权、JSON 序列化、分页 count） | 54.8–62.6 ms |
| 阈值（架构 §7 C4 行） | < 100 ms ✅ |
| 方案 | `ILIKE '%kw%' ESCAPE '\'` + `(status, published_at DESC)` 部分索引，**不建 trgm、不上 tsvector**（架构 §3.3 结论） |

> 说明：造数正文全部含「基座」，属**最坏情况**（命中 2000 条 → 排序 + count 全量）。
> 架构 POC-7 的口径是 2000 篇 4.2–6.1 ms（纯 SQL、无 ORM 映射、无分页 count），
> 本表是本模块端到端实测，多出的部分是渲染/映射/序列化，仍在 100 ms 预算内。

## 6. XSS 验证（同一份正文，三条路径）

正文（`content_md`）：

```markdown
## 概述
这是帮助文档的**概述**，含关键词 anchorword 与 <script>alert('xss')</script>。
### 安装步骤
第一步：安装依赖。
<img src=x onerror=alert(2)>
[x](javascript:alert(3))
```

| 断言 | 结果 |
|------|------|
| 落库/回显 `content_html` 无 `<script` / `<img` / `on*=` 属性 / `javascript:` 协议 | 命中 `{}`（空） |
| 原始 HTML 被降级为**转义文本** | `&lt;script&gt;alert('xss')&lt;/script&gt;`、`&lt;img` |
| 详情（闸 3 再净化） | 同上，`len=278` |
| 片段（H-10） | `snippet` 不含 `<` / `>` |

> 注意：字符串里仍能搜到 `onerror`（它是**文本节点**，浏览器不执行）。断言用的是
> 「不存在真实标签/属性/协议」的正则，而不是朴素子串匹配 —— 后者会把正常的转义文本误判成 XSS。

## 7. 版本回滚证据（H-06，改 3 次 → 回滚第 1 版）

```
H-06a 编辑已发布文档退回 DRAFT      编辑后 version=4 status=DRAFT
H-06b 回滚到 v1：内容 = v1          version=10 md 前 16 字=## 概述\n\n这是帮助文档的**
H-06c 回滚生成新版本（version + 1） 回滚前 version=9 回滚后=10
H-06d 历史版本一行不少             回滚前=[1,2,3,4,5,6,7,8,9] → 回滚后=[1,2,3,4,5,6,7,8,9,10]
```

| 动作 | `sys_help_doc.version` | 版本表新增 | 状态 |
|------|------------------------|------------|------|
| 新建 | 0 | — | DRAFT |
| 发布 ×3 | 1 / 2 / 3 | 1 / 2 / 3 | PUBLISHED |
| 保存+发布 ×3 | 4..9 | 4..9 | DRAFT → PUBLISHED |
| 回滚到 v1 | **10** | 10（内容 = v1） | 保持不变 |

历史行一行未删（`1..9` 全在），回滚只是**新增**一行快照 —— 这正是架构 §3.1 的口径。

## 8. 版本清理任务（H-07）

- 任务名：`help_version_cleanup`，cron `0 40 2 * * *`，保留 20 版；
- 注册点：`task::Scheduler::register_builtin` → `business::help::model::register_builtin`（**不再是 C0 占位**）；
- 启动日志：

```
2026-10-29T01:57:53.240872Z  INFO alun_plugin::scheduler: 定时任务 help_version_cleanup [0 40 2 * * *] 已排程
2026-10-29T01:57:53.241221Z  INFO alun_plugin::scheduler: 定时任务插件: 16 个任务已启动
```

- 用例：25 版 → 清理 → 只剩 20 版，删除 `1..5`、保留 `6..25`。

## 9. 检查链

```
=== cargo fmt --check ===
fmt_check_exit=0
=== cargo clippy --all-targets -D warnings ===
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.63s
=== 模块单测 business::help ===
test result: ok. 27 passed; 0 failed
```

全量回归（连跑两轮，逐二进制）：

```
running 442 tests   ... test result: ok. 442 passed   ← lib（原 415 + 帮助模块 27）
running 11 tests    ... test result: ok. 11 passed    ← tests/batchC_c4_help.rs
running 8 / 7 / 2   ... ok
running 7 tests     ... 7 ignored                     ← S3/MinIO 用例（无对象存储时忽略）
```

合计 **472 passed / 0 failed**（`442+11+8+7+2+1+1`）。

## 10. 与架构文档的两处口径差异（实测修正，已写进代码注释）

1. **heading 锚点层级**：架构 §3.2 举例 `id="h-2-1"`，但 C1 的 `shared/content` 把正文标题
   统一 **+1**（页面已有 `<h1>`，`##` → `<h3>`），因此实际锚点是 `h-3-1`。
   真正的不变量是「TOC 的 id == HTML 里的 id」，用例按此断言（`H-11b 缺失=[]`）。
2. **权限种子迁移号**：架构 §5.3 把批次 C 的 20 个权限码统一排在 V0085，但本批是**并卡交付**
   （C2 已用 V0087）。为避免两卡同时写 V0085 造成「迁移版本重复 → 启动失败」，
   C4 取预留带末尾的 **V0088**，并在迁移头注释里写清冲突关系与 `ON CONFLICT` 幂等性。

## 11. 未做（明确边界）

- 未建 `pg_trgm` 索引（架构结论：≤5000 篇用 `ILIKE` 足够；预案以注释态留在 `V0083`）；
- 未做「按角色/菜单可见」（架构 §3.4 的未决项 3）；
- 未动公告与变更日志的任何代码与迁移。
