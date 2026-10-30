# 云枢（CloudPivot）批次 C —— 内容管理 产品需求文档（PRD）

## 公告管理 · 帮助文档 · 变更日志

| 项 | 内容 |
|---|---|
| 文档版本 | v1.0（待评审） |
| 所属批次 | **批次 C（内容管理）** |
| 编制 | 产品经理（product-manager） |
| 适用系统 | 云枢后台管理平台（Rust / Nik 0.4.2 + Vue3 + TDesign 1.19 + PostgreSQL 16） |
| 上游依赖 | 批次 A（对象存储 / 导入导出 / 消息通知）**已上线**；批次 B（数据权限 / 列级脱敏 / 角色互斥）**已上线** |
| 下游批次 | 批次 D（工作流审批）—— 本批只在第 10 章提一句，不做设计 |
| 交付形态 | 纯需求文档，不含代码 |

---

## 阅读指引

- **开发先读第 0 章**：38 条代码事实（F1–F38）全部带**文件路径与行号**，15 条已定产品口径（D1–D15）直接照做，不需要再问产品。
- **测试直接读第 9 章**：7 组验收场景，每组都有"步骤 → 预期 → 必须记录的数据 → 判定语句"，其中 9.6「回滚不留假日志」是本批**最关键**的一条，也是唯一一条要求"日志表里必须查不到某行"的验收。
- **前端读第 8 章**：7 个页面逐页给"入口 → 列表 → 表单/详情 → 关键反馈 → 空态 → 异常态"，错误文案在第 8.8 节给原文，不要自己发挥。
- **安全读第 3.9、4.7、5.9 三节**：XSS 净化与日志脱敏是本批的两条硬红线。
- 本文所有"事实"都可用 `grep` 复核；所有"实测"都是在本机 PostgreSQL 18.6 上真跑过的 SQL（目标库 PG16 相关特性一致），实测脚本见附录 C.7。

### 本批**不做**的事（先看这个，防止范围蔓延）

工作流审批（批次 D）、内容审核（编辑↔审核互斥，依赖 D 的审批流）、用户评论/点赞、公告定时推送的"企业微信/邮件"通道（批次 A 只做了站内信）、帮助文档的多语言、变更日志的回滚（只做"看"，不做"撤销"）。完整清单见附录 D。

---

## 目录

| 章 | 标题 | 关键内容 |
|---|---|---|
| 0 | 核查事实与已定口径 | F1–F38 事实；D1–D15 决策 |
| 1 | 角色与场景 | 5 类角色、8 个数字故事、1 个反场景 |
| 2 | 功能清单与优先级 | 公告 18 项 / 帮助 14 项 / 变更日志 15 项，P0/P1 分布 |
| 3 | 公告管理详设 | 数据模型、发布范围、有效期、已读、站内信联动、XSS |
| 4 | 帮助文档详设 | 分类树、Markdown 渲染、TOC、搜索、按角色可见 |
| 5 | 变更日志详设 | 数据模型、事务一致性（回滚不留假日志）、diff、脱敏、归档、与审计的区别 |
| 6 | 权限模型 | 21 个权限码、敏感操作、数据权限结论 |
| 7 | 菜单结构 | 3 个可见菜单 + 2 个隐藏菜单，sort_order 与 module_code |
| 8 | 页面与交互 | 7 个页面逐页 + 三处错误文案原文 |
| 9 | 验收场景 | 7 组，含 2 组反例 |
| 10 | 后续规划 | 一段话 |
| 附录 A | 文案总表 | 用户可见文案索引 |
| 附录 B | 待拍板决策 | 6 条，已按推荐值落地 |
| 附录 C | 非功能与交付前提 | 表变更、依赖、性能指标、上线顺序、实测脚本 |
| 附录 D | 明确不做 | 12 条 |
| 附录 E | 接口清单与内部契约 | 30 个接口 + 内部调用契约 |

---

# 第 0 章 核查事实与已定口径

> 本章是本 PRD 的**落地依据**。凡是"框架能不能做"的判断，这里都给了文件路径 + 行号，评审时可直接 `grep` 复核。
> 仓库根：`/root/.hermes/workspace/information-security/cailu-src`（同栈底座，下文路径均相对该根）。

## 0.1 平台与权限事实（F1–F8）

| # | 核查结论 | 证据（路径:行号） | 对本文的影响 |
|---|---|---|---|
| **F1** | 权限码格式 `{模块}:{资源}:{动作}`，存 `sys_menu.auth_code`；`menu_type` 1 目录 / 2 菜单 / 3 按钮；按钮行 `is_visible=FALSE` 不进导航 | `backend/migrations/system/V0004__create_sys_menu.up.sql:1-24` | 本批 21 个权限码全部落成 `sys_menu` 种子行（第 6 章） |
| **F2** | 前端 `hasPermission()` 是**全等匹配**，只特判 `*:*:*`，**不支持前缀通配** | `frontend/src/stores/user.ts:67-73` | 每个动作必须有独立 `auth_code`。写 `business:announcement:*` 在前端恒为 false，按钮永远不显示 |
| **F3** | `filterNavMenus` 递归过滤：`is_visible=FALSE` 不进导航，**目录下无可见子菜单则整目录隐藏** | `frontend/src/stores/user.ts:21-38` | 用户端"公告中心/帮助中心"必须做成隐藏菜单，否则会出现在侧边栏 |
| **F4** | 顶级目录 sort_order 现状：`system`=10、`support`=20；30 预留给协同办公，40/50/60 已被 ERP/BI/MDM 批次占用，70 是批次 A 的 `/platform` | `backend/migrations/seed/S__menu_data.sql:24,80-81` + 批次 A PRD 7.1 | 本批**不新建一级目录**，三处管理端菜单挂到批次 A 的 `/platform` 下（sort 60/70/80），零排序冲突（第 7 章） |
| **F5** | `sys_menu.module_code` 是 `VARCHAR(20)` | 同上 F1 迁移 | 新模块段只能取 ≤20 字符；本批统一用 `business`（与批次 A 一致），不新增值 |
| **F6** | 菜单管理页的"模块编码"是**自由输入框**，不是下拉 | `frontend/src/views/system/menu/index.vue:139-140` | 无需改前端下拉选项（批次 B 担心的"筛不到菜单"问题在本仓库不成立，但种子数据仍要正确填） |
| **F7** | `sys_role` **没有 `data_scope` 列**，业务代码中也无该字段 | `backend/migrations/system/V0002__create_sys_role.up.sql:2-15`（列清单无 data_scope）；`grep -rn data_scope backend/src` 返回空 | 数据权限只能服务层硬实现。本批公告的"部门主管只能发本部门"用祖先链校验硬编码（3.4），不依赖配置 |
| **F8** | 部门子树递归**已有**：`find_dept_subtree_ids()` 用 `WITH RECURSIVE` **向下**取子树，自带 `deleted_at IS NULL` | `backend/src/system/dept/model.rs:261-279` | 「发布给技术部（含下级）」的**受众展开**复用它；但「判断某用户是否可见」需要**向上**的祖先链（3.4.3，本文新增一次递归，实测更省） |

## 0.2 日志与审计事实（F9–F16）—— 第 5 章的地基

| # | 核查结论 | 证据（路径:行号） | 对本文的影响 |
|---|---|---|---|
| **F9** | `audit_logs` 现有列：`request_id / user_id / username / operation / target_type / target_id / before_data(JSONB) / after_data(JSONB) / detail(JSONB) / ip / user_agent / created_at`。**没有** `module_code`、**没有** `result`、**没有** `action`（新增/修改/删除）；索引只有 user_id / operation / created_at 三个 | `backend/migrations/support/V0014__create_audit_logs.up.sql:1-35` | 「列表要按模块筛选、要显示结果列、要按对象 ID 查历史」在现有表上**做不了**——缺列缺索引。这是本批要新建 `sys_change_log` 而不是"照抄 audit_logs"的直接原因（5.1） |
| **F10** | **审计日志是异步写**：业务侧只做一次无阻塞 `channel send`，后台 worker 每 500ms 或满 256 行批量落库；写库失败只记 `tracing::error!` 并**丢弃该批次** | `backend/src/support/log/bus.rs:45,96-101,204-232,292-309` | 🔴 **本批最关键的架构事实**：日志落库与业务事务**完全解耦**。业务事务回滚后，已经入队的日志照样会落库 → **"回滚留下假的成功日志"是现状架构的必然结果**，不是偶发 bug（5.3 给解法，9.6 给判定） |
| **F11** | `USER_UPDATE` 的审计快照只写了 **3 个字段**：`real_name`、`dept_id`、`status`。**`mobile` / `email` / `remark` 根本没有进快照** | `backend/src/system/user/handler.rs:316-335`（before/after 各三个 key） | 验收场景 5「改手机号 → 日志显示手机号前后值」**按现状 100% 做不到**。本批必须把"受管对象的变更字段白名单"显式定义出来（5.4.2），否则这条验收无法通过 |
| **F12** | `update()` 用户是**单条 UPDATE、没有事务**（`to_save.update(db())`）；只有 `delete` / 分配角色 / 批量调部门 / 创建用户包了 `db().transaction(...)` | `backend/src/system/user/handler.rs:297-300`（无事务）；`:209,385,699,781`（有事务） | 要让"业务与日志同生共死"，这些写操作**必须补事务**。这是本批对存量代码唯一的侵入式改造，清单见附录 C.5 |
| **F13** | `record_audit()` **不做任何脱敏**：`before_data/after_data` 原样进模型，直接投递 | `backend/src/support/log/mod.rs:204-220` | 一旦按 F11 补齐 mobile/email 快照，**日志表里就会多一份明文 PII 副本**。脱敏必须在写入路径上做（5.6） |
| **F14** | `security_logs` 列：`request_id / user_id / username / event_type / detail(JSONB) / ip / user_agent / created_at`，无 result、无 target | `backend/migrations/support/V0016__create_security_logs.up.sql:1-27` | 失败尝试留痕放这里（`event_type='OPERATION_FAIL'`），**不放**变更日志，避免"失败记录"污染变更事实（5.5.3） |
| **F15** | 五张日志表清理任务是**五段 cron**、03:00–03:40 错峰（`clean_operation_log`=0 3 / `clean_audit_log`=10 3 / `clean_auth_log`=20 3 / `clean_security_log`=30 3 / `clean_password_log`=40 3）；保留期常量统一 **90 天** | `backend/src/task/mod.rs:45-55`；`backend/src/shared/constants.rs:210-217` | 本批新增的归档任务排 **04:00**、公告已读清理排 **04:20**，五段 cron，避开现有 5 个任务（3.12、5.8） |
| **F16** | 现有日志页 `/support/log` 用 5 个 tab（操作/登录/审计/安全/改密）覆盖全部日志；**审计 tab 的列只有**：时间 / 用户名 / 操作 / 目标类型 / 目标 ID / IP / request_id，**没有模块、没有动作、没有结果、没有详情 diff、没有导出** | `frontend/src/views/support/log/index.vue:216-222,270-278` | 「变更日志」不是"再造一个审计 tab"，而是**面向业务与合规的可读视图**：按模块/动作/结果筛、按对象追历史、字段级 diff、脱敏导出（5.2 讲清分工） |

## 0.3 XSS 与渲染事实（F17–F23）—— 第 3.9、4.7 节的依据

| # | 核查结论 | 证据（路径:行号） | 对本文的影响 |
|---|---|---|---|
| **F17** | backend 的 `nik` 依赖**已开启 `xss` feature**，Cargo.toml 里的注释写的就是"HTML 净化：社区公告等富文本入库前过滤"；但 `backend/src` 全仓 `grep xss::` **一次都没调用过** | `backend/Cargo.toml:32`；`grep -rn "xss::\|sanitize_html" backend/src` 返回空 | 能力已就位、从未使用。本批是它第一次真正上身——**不需要新增 feature，不需要新增 crate 下载**（F21） |
| **F18** | `nik_utils::xss` 只暴露 3 个函数：`sanitize_html` / `sanitize_html_strict` / `has_potential_xss`，**没有暴露 `Builder`，无法自定义标签/属性白名单** | `~/.cargo/registry/src/*/nik-utils-0.4.2/src/xss.rs:29-48` | 若产品要求"保留 target=_blank"或"允许 span 样式"，仅靠这 3 个函数做不到，必须**在 backend 直接依赖 ammonia** 并自建 Builder（F21 已证零下载） |
| **F19** | **ammonia 4.2 默认白名单实测结果**（本机真跑，输入→输出见附录 C.7）：`<script>`/`<iframe>`/`<svg>`/`<style>` **整块删除**；`onerror` 等事件属性删除；`style` 属性**整条删除**（含 `position:fixed` 全屏覆盖层）；`class` 属性**整条删除**；`href="javascript:"` → 只留 `<a rel="noopener noreferrer">`；`<img src="data:text/html...">` → `src` 被剥只剩 `<img>`；`target="_blank"` **被剥掉**；`table/thead/tr/th/td/pre/code/ul/ol/li/img(https)/h1-h6` 正常保留 | `~/.cargo/registry/src/*/ammonia-4.2.4/src/lib.rs:380-500`（默认 tags / generic_attributes={lang,title} / tag_attributes / style_properties=None / allowed_classes={} / link_rel=noopener noreferrer）+ 本地 `cargo run` 实测输出 | ①XSS 默认策略够严，**可以放心用于公告富文本**；②但 `class`/`style` 被剥意味着**富文本不能保留字体颜色、字号、自定义样式**，这是必须写进界面的产品限制（3.9.3）；③`<code class="language-xx">` 的语言标记会丢 → 帮助文档**不能用富文本存代码块**（4.1 的选型依据之一） |
| **F20** | `nik` 的 `xss.rs` 文档注释称 `<a>` 支持 `href, title, target`，**实际 ammonia 默认 `a` 只有 `href, hreflang`**（`title` 来自 generic_attributes 所以有，`target` 没有） | `nik-utils-0.4.2/src/xss.rs:24` 对比 `ammonia-4.2.4/src/lib.rs:394-398` 与实测 | 文档与实现不一致，**以实测为准**。公告里的外链会在当前页打开（不是新窗口）——要么接受，要么自建 Builder 加 `target`（附录 B3 待拍板） |
| **F21** | `ammonia 4.2.0` **已在 `backend/Cargo.lock`**（由 nik-utils 的 xss feature 传递引入） | `backend/Cargo.lock:336-338` | 若需自定义白名单，在 `backend/Cargo.toml` 声明 `ammonia = "4.2"` 是**零下载**（本地 registry 已有 4.2.0/4.2.4）。这是本批唯一"可自选"的技术杠杆 |
| **F22** | 前端 `package.json` 依赖只有 axios / dayjs / echarts / pinia / tdesign-icons-vue-next / tdesign-vue-next / vue / vue-i18n / vue-router。**没有任何 Markdown 渲染器，也没有 DOMPurify 之类的净化库** | `frontend/package.json`（dependencies 全量） | 帮助文档的 Markdown 渲染 + 二次净化**必须新增前端依赖**（`markdown-it` + `dompurify`），属交付前提（附录 C.2），需要 pnpm 私服可用 |
| **F23** | 工程内**没有任何富文本编辑器依赖**；TDesign 1.19 官方组件清单中也没有可直接用的富文本编辑器（本工程只引了 `tdesign-vue-next` 本体） | `frontend/package.json`；`ls frontend/node_modules` 不存在（构建期安装） | 公告富文本编辑需要引入第三方编辑器（建议 `@wangeditor/editor-for-vue@5`，附录 B2）。**编辑器选型不进 P0 的前置阻塞**，可用 `t-textarea` 富文本降级方案先跑通接口 |

## 0.4 前端与图标事实（F24–F29）

| # | 核查结论 | 证据（路径:行号） | 对本文的影响 |
|---|---|---|---|
| **F24** | axios 全局 `timeout: 15000`；分页返回体 `{list, total, page, page_size}` | `frontend/src/api/request.ts:68,138,15-22` | 变更日志导出、帮助文档大文档保存都必须单独放宽 timeout 或走异步导出（3.10、5.7） |
| **F25** | 分页上限 `MAX_PAGE_SIZE=1000`，默认 `DEFAULT_PAGE_SIZE=10` | `~/.cargo/registry/src/*/nik-web-0.4.2/src/api.rs:104,107` | `page_size > 1000` 会被静默截到 1000；变更日志默认 20 条/页，导出走独立通道不受此限 |
| **F26** | 图标存在性实测（`ICON_NAMES` 共 2356 个）：✅ `pin` `notification` `help-circle` `search` `history` `article` `file-pdf` `file-copy` `rollback` `time` `refresh` `edit-1` `add` `delete` `view-list` `browse` `cloud-download` `check-circle` `error-circle` `attach`；❌ **不存在** `announcement` `bulletin` `log` `clipboard` `sort` `ascending` | `frontend/src/components/IconSelect/icons.ts`（去 CRLF 后逐个 `grep -x`） | 菜单与按钮图标只能从 ✅ 里选。公告= `article`、公告中心= `pin`、帮助= `help-circle`、变更日志= `history`（第 7 章） |
| **F27** | `v-privilege` 指令存在，无权限时 `display:none`（保留 DOM） | `frontend/src/directives/permission/privilege.ts:26-27`；用法见 `frontend/src/views/support/config/index.vue:20,43,102` | 页面内按钮一律用 `v-privilege="['code']"`，不要自己写 `v-if="hasPermission(...)"`（口径不统一会导致有人绕过） |
| **F28** | 路由 `meta.permission` 做页面级权限 | `frontend/src/router/index.ts:5-13,39-43` | 5 个新路由都要带 `meta.permission`；用户端两个页面带 `business:announcement:view` / `business:help:view` |
| **F29** | 顶栏结构（从左到右）：折叠按钮 → 面包屑 → 主题切换（`basic-layout__theme-toggle`）→ 用户下拉（`BasicLayout.vue:54-60`） | `frontend/src/layouts/BasicLayout.vue:23-61` | 公告入口插在**主题切换与用户下拉之间**（批次 A 的消息铃铛也在这里，两者并排：铃铛在右、公告在左，见 8.1.1） |

## 0.5 数据库实测事实（F30–F34）—— 本机 PostgreSQL 18.6 真跑

> 实测脚本见附录 C.7，可复现。PG16 与本测涉及特性（声明式分区、`DETACH/ATTACH PARTITION`、`pg_trgm`、`jsonb_ops/path_ops`、`IS DISTINCT FROM`）行为一致。

| # | 核查结论 | 证据 | 对本文的影响 |
|---|---|---|---|
| **F30** | **回滚行为实测**：①同一事务内 `INSERT` 日志后 `ROLLBACK` → 日志表 **0 行**；②事务外（或先于事务、或异步总线）写日志后业务 `ROLLBACK` → 日志表 **仍在，1 行** | 实测输出：`t2_log_rows_after_rollback = 0`；`t3_log_rows_after_rollback = 1` | 第 5 章「事务内同步写」不是洁癖，是**唯一能让"回滚不留假日志"成立**的做法；F10 的异步总线恰好是第 ② 种写法的现成例子 |
| **F31** | 分区可行性实测：`sys_change_log ... PARTITION BY RANGE (created_at)` + `PRIMARY KEY (id, created_at)`（分区键必须进主键）建表成功；按月分区路由正确（10 月/11 月行各进各分区）；`DETACH PARTITION` 后 `ATTACH` 到**同构分区归档表**成功，归档表立即可查；**不能再手工 `ADD PRIMARY KEY`**（分区已继承主键，会报 `multiple primary keys`） | 实测输出：`partition=sys_change_log_2026_10/11`、`archive_rows=3` | 归档方案定为「DETACH → ATTACH 同构归档表」（5.8），**秒级、不扫行**；附录 C.1 的建表脚本必须按实测写法给，避免开发踩 `multiple primary keys` |
| **F32** | JSONB 索引操作符实测：默认 `jsonb_ops` 的 GIN **支持 `?` / `?|`**（键存在），强制关 seqscan 后走 `Bitmap Index Scan`；`jsonb_path_ops` **不支持 `?`**（只有 `@>`），写 `after_data ? 'mobile'` 会退化为 `Seq Scan` | 实测输出：`Bitmap Index Scan on ..._after_data_idx1`；`jsonb_path_ops` 那条三处全是 `Seq Scan` | 「按字段名筛变更日志」（如"谁改过手机号"）必须用 **`jsonb_ops`（默认）**，不能用 `jsonb_path_ops`（5.1.1 索引清单） |
| **F33** | 中文检索实测（5000 行文档）：**2 字关键词**（`'%请假%'`）**不生成三元组 → 退化为 Seq Scan**，耗时 **9.75ms**；**3 字**（`'%请假流%'`）与 **4 字**（`'%请假流程%'`）走 `Bitmap Index Scan`，耗时 **0.25ms / 0.21ms** | 实测 `EXPLAIN (ANALYZE)` 三连 | 「搜『请假』」是验收场景 4 的原文，正好是**最慢的 2 字场景**。结论：P0 必须接受"2 字词走全表扫描"，并在文档量 >5000 篇时给出性能红线（4.5.4）；trgm 索引仍要建（3 字以上直接受益） |
| **F34** | 底座**没有任何** `CREATE EXTENSION` 迁移，`grep pg_trgm` 为空；数据库容器是官方 `postgres:16-alpine` | `grep -rn "pg_trgm\|CREATE EXTENSION" backend/migrations` 返回空；`docker/init/Dockerfile:8` | `pg_trgm` 需在迁移里 `CREATE EXTENSION IF NOT EXISTS pg_trgm;`（要求 DB 属主有权限）→ 列入交付前提 C.3，并给"扩展不可用"的降级方案（4.5.4） |

## 0.6 依赖与边界事实（F35–F38）

| # | 核查结论 | 证据 | 对本文的影响 |
|---|---|---|---|
| **F35** | 底座仓库**完全没有**消息/公告能力：`grep -i notice` 在 backend/frontend/migrations 全仓 0 命中 | `grep -rn "notice" -i backend/src backend/migrations frontend/src` 返回空 | 本批的「公告发布 → 推站内消息」**建立在批次 A 交付物之上**（`sys_notice_message` / `sys_notice_receiver` / `business:notice:*`）。若批次 A 消息中心尚未合并到主干，本批**先做公告本体，站内信联动按 D4 的降级方案排队**（3.8.4） |
| **F36** | `sys_file` 列：`file_name/original_name/file_key/file_size/file_hash/mime_type/extension/storage_path/category/status/uploaded_by/…/deleted_at`，**没有** `storage_id` | `backend/migrations/support/V0010__create_sys_file.up.sql:2-24` | 公告与帮助文档的**附件只存 `file_id` 关联**，不复制文件元数据；附件下载走批次 A 的文件接口（契约零改动） |
| **F37** | 错误码分段：`1000x` 通用 / `2000x` 认证 / `3000x` 授权 / `4000x` 支撑域；日志查询接口权限码统一 `support:log:list` | `backend/src/shared/error_code.rs:26-31`（码段表）、`:45`（`RESOURCE_NOT_FOUND=10002`）；`backend/src/support/log/handler.rs:135-137` | 本批新增业务错误码在 `error_code.rs` 追加 **`5000x` 内容域**段（不复用 4000x，避免与支撑域语义混淆）；**用户看到的文案见 8.8，界面上不出现任何错误码** |
| **F38** | `Mask::mobile("13812345678") == "138****5678"`（前 3 + `****` + 后 4）；而 `Mask::mask_by_type` 的 `_ =>` 兜底分支按**字节**切片，中文值会 panic | `~/.cargo/registry/src/*/nik-utils-0.4.2/src/mask.rs:209-210`（测试断言）；`mask.rs:196-201`（兜底分支） | 变更日志的字段脱敏必须**逐字段显式指定类型**调 `Mask::mobile/email/name/id_card`，**禁止**走 `mask_by_type` 兜底（会导致中文备注字段 5xx）。列入代码评审强制项（附录 E.4） |

---

## 0.7 十五条必须先定的产品口径（本文已定，开发照做）

| # | 问题 | 本文的定法 | 为什么这么定 |
|---|---|---|---|
| **D1** | 公告正文用富文本还是 Markdown | **富文本（HTML）**，但样式受限：只保留结构（标题/段落/列表/表格/代码块/引用/图片/链接/粗斜体下划线） | 发公告的人是行政/HR，不是工程师——让他们写 Markdown 等于把发布门槛抬到技术岗。样式受限不是为了省事，而是 F19 实测：ammonia 会剥掉 `style`/`class`，若产品承诺"能改字体颜色"，上线即翻车（3.9.3 把限制写进界面） |
| **D2** | 帮助文档用什么格式 | **Markdown 原文存储**，前端渲染（`markdown-it` 禁 raw HTML + `DOMPurify` 二次净化）；**后端不存 HTML** | ①代码块要语言标记做高亮，而 ammonia 会剥 `class="language-xx"`（F19），富文本路线天然做不好；②文档要能被人 diff/被后续"开发者写文档"场景复用；③TOC 从渲染后的 h1–h4 生成，Markdown 路线最自然 |
| **D3** | HTML 净化放在哪一层、用什么白名单 | **入库前在服务端净化**（写接口里做，不是读接口做），用 `nik::xss::sanitize_html`（ammonia 默认白名单）；前端渲染 `content_html` 时**不再二次解析**（直接 `v-html` 输出已净化串），编辑器侧粘贴时另做一次前端净化（双保险） | ①入库即净化 → 库里没有脏数据，导出/搜索/消息摘要都不会二次踩坑；②若只在读接口净化，任何新出口（导出、站内信、开放接口）都会漏；③默认白名单已实测够严（F19），**不自定义**是最省事且最安全的选择 |
| **D4** | 公告发布是否推站内消息 | **推**（P0）。发布成功即写一条 `sys_notice_message`（`business_key='content.announcement'`，`biz_ref_id=公告 id`，`action_url=/notice/detail/{id}`，`action_text='查看公告'`），正文取**公告摘要的纯文本**（≤200 字），不推 HTML | 公告的价值在于"被看到"。只靠顶栏角标，用户不进系统就不知道；站内信是批次 A 已建好的主干道，复用成本最低。正文推纯文本是因为批次 A 的消息正文是纯文本（`white-space: pre-wrap`），推 HTML 会渲不出来 |
| **D5** | 已读口径 | 一人一行 `sys_announcement_read`；**顶栏角标 = 当前用户"可见但未读"的公告数**（上限显示 99+）；阅读人数 = `count(*)`（不去重读次数）；"阅读明细"（谁读了）P1 | 与批次 A 消息中心"一人一行、各自已读"同构，用户心智一致；阅读人数用聚合而非冗余计数列，避免并发写热点 |
| **D6** | 公告列表排序 | `置顶(且置顶未过期) DESC → publish_at DESC`；管理端同序但多一列状态 | 置顶是"告知"的最强手段，必须压过时间序；置顶带过期时间（`top_expire_at`）是为了防止"去年的置顶还在第一位" |
| **D7** | 有效期的"到点自动上下线"怎么实现 | **双保险**：①**查询谓词惰性判定**（`publish_at <= now() AND (expire_at IS NULL OR expire_at > now())`）—— 所有读取路径都必须带这个谓词，**这是 P0 唯一必须做的**；②定时任务每 5 分钟把已过期的 `PUBLISHED` 翻成 `EXPIRED`（P0，仅为让管理端状态列不骗人） | 只靠定时任务会有"最长 5 分钟窗口"的状态不一致；只靠谓词则管理端状态列永远显示"已发布"。两者都要，且以谓词为准（谓词错了定时任务救不回来） |
| **D8** | 帮助文档是否"按角色可见" | **P0 不做**（所有登录用户可读，与任务书"所有登录用户可读"一致）；**P1 预留** `visible_scope` 列（`ALL` / `ROLE`）并把判定写进同一个查询谓词，默认为 `ALL` | P0 加角色可见会把"帮助中心"变成第二套权限体系（还要做"看不见的分类树如何显示"的交互），收益远小于成本。预留列 + 统一谓词，P1 打开时零迁移 |
| **D9** | 变更日志的数据源：改 `audit_logs` 还是新建表 | **新建 `sys_change_log`**（按月分区），只承载**业务变更事实**；`audit_logs` 保持现状不动 | ①`audit_logs` 缺模块/动作/结果三列且索引不含 target_type+target_id（F9），改动要动全局写入路径 + 5 张日志表的清理逻辑；②新表能用"事务内同步写"从第一行代码就正确（F30），而 `audit_logs` 的异步通道是既成事实（F10），改它等于把 10 处 handler 全部重写。**两表分工写清（5.2），比强行合并更安全** |
| **D10** | "回滚不留假日志"怎么保证 | **三道闸门**：①业务写与日志写**在同一个事务**里（`record(&mut tx, …)`）；②失败尝试只能在**事务返回 Err 之后**用**独立连接**写，且强制 `result='FAILED'` + `after_data IS NULL`；③数据库 `CHECK (result <> 'FAILED' OR after_data IS NULL)` 兜底 | 任何"事后补写成功日志"的做法都会在回滚时造出假记录（F30 实测 ②）；只有"日志跟业务同生共死"能保证"日志里有成功记录 ⟺ 数据真的变了"。第 ③ 条让"开发写错代码"变成**插入即报错**，而不是靠评审看 |
| **D11** | 变更日志里的敏感值怎么存 | **写入即脱敏**：`before_data/after_data` 存的是 `Mask::mobile/email/name/…` 的输出；同时写一个 `changed_fields` 数组，记录**按明文比较**得出的"真正变了哪些字段" | ①日志表不该成为第二份 PII 明文副本（F13）；②但"脱敏后旧值新值相同"是真实陷阱（改中间 4 位手机号），所以变更判定必须在**脱敏前**用明文做，结果记进 `changed_fields`，详情页据此显示"已变更（脱敏后显示相同）"（5.4.3） |
| **D12** | 保留多久、要不要归档 | 在线 `sys_change_log` **12 个月**（按月分区）；归档表 `sys_change_log_archive` **5 年**（同构分区，只读）；每日 04:00 把过期分区 `DETACH`→`ATTACH` 进归档（**秒级，不扫行**，F31）；超过 5 年的分区导出 xlsx 到对象存储后 `DROP PARTITION` | 合规场景"查一年前的变更"是常态，12 个月在线 + 5 年归档既满足查得到，又避免在线表无限膨胀（日志表是全库增长最快的表）。分区 + DETACH 是唯一能做到"归档不锁表"的方式 |
| **D13** | 公告管理端的可见/可操作范围 | 有 `business:announcement:update` 的人可编辑**全部**公告；**但"发给谁"受部门约束**：若操作者不持有 `business:announcement:publish-all`，则选中的目标部门必须全部落在**自己部门的子树内**，否则拒绝（3.4.3） | 公告是"广播权"，不是"数据归属"。按 `created_by` 隔离编辑权会导致"HR 离职后公告改不了"；而"发给谁"必须有边界，否则任何有发布权的人都能给全公司发（这才是真实风险） |
| **D14** | 变更日志的数据权限 | **不做行级数据范围过滤**：有 `business:changelog:list` 就能看上所有模块的变更记录（含超管自己的） | 日志的价值就是横向监督。若按部门过滤，"市场部管理员改了自己部门的数据"反而查不到——审计就废了。风险用"权限码收紧 + 导出单独授权 + 明文导出双闸门"控制，不用行过滤 |
| **D15** | 三个模块的菜单挂哪 | 挂在批次 A 的 **`/platform`（通用底座，sort 70）** 下，新增 3 个可见菜单（sort 60/70/80）+ 2 个隐藏菜单（用户端公告中心/帮助中心） | 三者都是平台级能力、权限码同属 `business` 段；新建一级目录只有一个空位（30，已预留给协同办公），为三个菜单占掉它是浪费。挂 `/platform` 后侧边栏顺序天然合理（存储/文件/导入导出/通知/消息/**公告/帮助/变更日志**） |

---

# 第 1 章 角色与场景

## 1.1 角色总览

| 角色 | 在哪个系统里活 | 本批的核心诉求 | 对应权限码（详见第 6 章） |
|---|---|---|---|
| **行政 / HR** | 管理端 | 发全员公告、带附件、置顶、看阅读人数 | `business:announcement:list/create/update/publish/publish-all/top/delete/read-stat` |
| **部门主管** | 管理端 | 只给本部门（含下级）发通知，不能惊动全公司 | `business:announcement:list/create/update/publish`（**无** `publish-all`） |
| **普通员工** | 用户端（顶栏 + 公告中心 + 帮助中心） | 不漏公告、看得懂、能搜到答案 | `business:announcement:view`、`business:help:view`（登录即默认给） |
| **系统管理员** | 管理端 | 维护帮助文档；查"某条数据被谁改成什么样" | `business:help:doc:*`、`business:changelog:list/detail` |
| **审计 / 合规** | 管理端 | 按时间/人/模块筛变更，导出脱敏台账 | `business:changelog:list/detail/export` |

> **一个账号可以同时是多个角色**（`sys_user_role` 多对多，F 已有的 RBAC）。上面的划分是"典型岗位"，不是硬边界。例如 HR 通常同时拿到 `publish-all`，而部门主管永远只有普通 `publish`。

## 1.2 八个数字化故事（含数字，可直接当验收基线）

### 故事 1（行政 / HR · 发国庆放假通知）

10 月 29 日 16:20，HR 王芳要给 **312 人**发《2026 年国庆放假通知》。她填标题、摘要"10 月 1 日至 7 日放假，10 月 8 日正常上班"、正文（一张 3 行表格 + 两段说明），上传 `国庆放假通知.pdf`（**2.4MB**，来自 `sys_file` 的 `document` 分类），发布范围选「全员」，有效期设为 `10-01 00:00` 到 `10-08 00:00`，勾「置顶」并把置顶期设为 **7 天**（到 11-05 自动取消置顶）。

点「发布」后：
- 系统在 1 秒内生成 **312 行** `sys_notice_receiver`（批次 A 的扇出算法），每人收到一条站内信，正文是**摘要的纯文本**；
- 顶栏公告角标从 **0 → 312 人各 +1**（每人各自的未读数）；
- 管理端列表该行状态变「已发布」，阅读人数初始 `0/312`。

三天后她回来看：**阅读人数 287/312**，点「阅读明细」看到 25 个未读人的姓名与部门——她给这 25 人各发了一条催办（P1 能力）。

### 故事 2（行政 / HR · 发错公告后撤回）

王芳发布 3 分钟后发现**附件传成了去年的版本**。她点「撤回」：
- 弹窗提示：「撤回后**所有人都看不到了**，已有 **8** 人读过。撤回不会删除已发送的站内信，站内信点开会提示『该公告已撤回』。是否撤回？」
- 确认后：公告状态变「已撤回」，用户端列表立即消失，**8 条已读记录保留**（合规要能查"谁在撤回前看过"）；
- 她修正附件后重新发布 → 生成**新的公告 ID**，未读数再 +1（这是刻意的：不能让"撤回-重发"变成偷改，读者必须知道内容变了）。

### 故事 3（部门主管 · 只给技术部发值班安排）

技术部主管李强有 `business:announcement:publish` 但**没有** `publish-all`。他发《春节值班安排》，范围选「指定部门 → 技术部」并勾「含下级」：
- 选部门时，下拉里**只有他自己部门及其下级**（14 个部门节点），别的部门根本不让选（前端过滤 + 后端校验双保险，3.4.3）；
- 扇出 **28 人**（技术部 9 人 + 3 个下级组 19 人）；
- **市场部 47 人**：公告列表看不到、按 ID 直查返回 **404**（不是 403——403 等于告诉对方"这条存在"，4.4 讲清原因）、站内信也没收到。

### 故事 4（普通员工 · 登录看未读、点开变已读）

市场部张伟早上 9:02 登录：
- 顶栏 `article` 图标右上角红点显示 **3**；tooltip「公告中心（3 条未读）」；
- 点开进入公告中心，列表**置顶在前**：《国庆放假通知》带 `置顶` 标签、《考勤制度调整》、《食堂菜单更新》；
- 点开第一条，正文渲染出来（表格正常、附件 `国庆放假通知.pdf · 2.4MB` 可下载）；
- **0.5 秒内**：列表该行未读圆点消失、顶栏角标 3 → 2（本地先减，接口幂等）；
- 他 K 掉页面重新登录：角标仍是 **2**（已读不会回退）。

### 故事 5（普通员工 · 在帮助中心搜"请假"）

同一时间，张伟要请假，在帮助中心搜索框输入 **「请假」**（2 个字，F33 实测这是最慢的场景）：
- 后端返回 **63 条**命中（标题命中 51 条 + 正文命中 12 条），**按相关度排序**（标题命中 > 摘要命中 > 正文命中），首屏 20 条；
- 每条结果给「标题（关键词 `<mark>` 高亮）+ 60 字片段（片段里也高亮）+ 所属分类」；
- 他点进《请假流程与审批时限》，正文渲染：**左侧 TOC** 自动列出「一、适用范围 / 二、申请步骤 / 三、审批时限 / 四、常见问题」；正文里的 `请假` **全部高亮**（黄色底）；
- 页脚显示「最近更新：2026-09-12 · 版本 4」——他知道自己看的是新流程。

### 故事 6（系统管理员 · 查某用户上周被谁改过）

安全事件复盘：某账号 `zhangwei` 上周手机号被改成陌生号码。管理员在「变更日志」里输入对象 ID（或从用户详情页点「变更历史」）：
- 列表 7 条记录，按时间倒序，最新一条：`2026-10-22 14:31 · 王芳(admin) · 系统管理 · 修改 · 用户 zhangwei · 10.20.3.88 · 成功`；
- 点开详情，**diff 表格**：

| 字段名 | 旧值 | 新值 |
|---|---|---|
| 手机号 | 138****1234 | 139****5678 |
| 所属部门 | 市场部 | 市场部 |

- 第三行判定「未变」灰字显示，一眼看出**只有手机号变了**；
- 他点「查看该对象的全部变更」→ 30 条历史，包含 10-22 之前三个月的每一次改动；
- 他想看**明文**：没有 `business:changelog:export-plain` 与 `system:field-mask:view-plain` 双权限，界面直接不显示"查看明文"（P1 才有此入口）。

### 故事 7（审计 / 合规 · 导出某段时间所有敏感操作）

季度内审，合规要 2026 Q3（7-1 至 9-30）所有"用户/角色/权限"模块的**删除**与**授权变更**：
- 筛选：时间 `2026-07-01 ~ 2026-09-30`、模块 `系统管理`、动作 `删除` + 修改；
- 命中 **1,284 条**（> 1000，同步导出被拒）→ 系统自动转**异步导出**（复用批次 A 的 `sys_impexp_task`），提示「预计 1,284 行，已转后台导出，完成后通知你」；
- **4 分 12 秒**后收到站内信，下载 `变更日志_20260831.xlsx`（11 列，首行冻结）；
- 文件里手机号列是 `139****5678`、身份证列 `1101**********1234`、邮箱 `z***@ex.com`——**导出与屏幕同一份脱敏结果**，不存在"导出明文接口"（5.7）。

### 故事 8（系统管理员 · 维护帮助文档）

管理员新建《如何申请 VPN 权限》：
- 左侧 Markdown 编辑器、右侧实时预览（预览里代码块带语言高亮、表格有边框）；
- 插入图片：走批次 A 的文件上传，插入 `![拓扑](https://…/vpn.png)`；
- 保存草稿 → 点「发布」→ 用户端 3 秒内可搜到（列表缓存失效）；
- 下架后：**用户端搜不到、直接访问 URL 显示「该文档已下架」**，管理端仍可见（标灰）。

---

### 反场景（明确不做什么，防止后面扯皮）

**管理员改 `zhangwei` 的手机号，同时把 `username` 改成一个已存在的账号名 —— 数据库唯一索引冲突，整个事务回滚。**

这时：
- ✅ 系统提示「用户名已存在」；
- ✅ `sys_user.mobile` **仍是旧值**（数据没变）；
- ✅ `sys_change_log` 里 `result='SUCCESS'` 的记录 **0 条**（这是本批最关键的技术红线，9.6 有判定语句）；
- ✅ 允许有 **1 条** `result='FAILED'`（`after_data` 必须为 NULL），或者**一条都没有**——两种都算通过；
- ❌ **绝对不允许**出现"成功：手机号 138→139"这种记录。日志说改了、数据其实没改，这在合规复盘时是**灾难**：审计会被引向不存在的变更，真正的问题被掩盖。

---

# 第 2 章 功能清单与优先级

> 编号规则：公告 `A`、帮助 `H`、变更日志 `C`。**P0 = 本批必须交付且验收**；**P1 = 本批排期做，若资源不足可顺延到 C.1 迭代，但表结构与权限码必须一次到位**（避免二次迁移）。

## 2.1 公告管理（18 项）

| # | 功能 | 优先级 | 说明 |
|---|---|---|---|
| A1-01 | 公告列表（管理端） | **P0** | 列：标题 / 发布范围 / 状态 / 置顶 / 发布人 / 有效期 / 阅读人数 / 操作；筛选：标题、状态、范围类型、时间区间 |
| A1-02 | 新增公告（草稿） | **P0** | 标题、摘要、正文、附件、范围、有效期、置顶 |
| A1-03 | 编辑公告 | **P0** | **已发布也能改**，但每次保存写一条变更日志（对象=公告） |
| A1-04 | 删除公告 | **P0** | 仅「草稿/已撤回/已过期」可删；已发布必须先撤回（软删） |
| A1-05 | 发布 / 撤回 | **P0** | 发布=写站内信 + 扇出受众；撤回=立即对用户不可见，保留已读记录 |
| A1-06 | 置顶（带置顶期） | **P0** | `top_expire_at` 到期自动取消置顶（懒判定 + 定时任务） |
| A1-07 | 发布范围：全员 / 角色 / 部门（多选） | **P0** | 部门勾选项带「含下级」开关；角色与部门可多选 |
| A1-08 | 有效期：起止时间 | **P0** | 支持"立即生效/定时生效"；结束时间可空（长期有效） |
| A1-09 | 正文富文本 | **P0** | 受限样式（D1/F19）；服务端净化（D3） |
| A1-10 | 附件（多附件） | **P0** | 关联 `sys_file.id`；单公告上限 **10 个 / 合计 50MB**；白名单走批次 A 的策略 |
| A1-11 | 阅读人数与未读人数 | **P0** | 列表列 `已读 X / 应读 Y`；Y 为发布时扇出人数快照 |
| A1-12 | 用户端公告中心（列表 + 详情） | **P0** | 顶栏入口 + 未读角标；排序 `置顶 → 时间` |
| A1-13 | 详情自动标已读 | **P0** | 幂等；本地先减角标 |
| A1-14 | 发布即推站内信 | **P0** | D4；`business_key='content.announcement'`；点消息跳详情 |
| A1-15 | 阅读明细（谁读了/谁没读） | P1 | ≤1000 行可直接展示，超过导出（脱敏：只给姓名+部门） |
| A1-16 | 公告草稿预览（用户视角） | P1 | 管理端「预览」按用户端样式渲染 |
| A1-17 | 到期自动翻转状态 | **P0** | 每 5 分钟任务 `announcement_expire`（D7） |
| A1-18 | 站内信补推（对未读者一键催办） | P1 | 仅对未读 25 人再发一次；48h 内最多 1 次 |

## 2.2 帮助文档（14 项）

| # | 功能 | 优先级 | 说明 |
|---|---|---|---|
| H1-01 | 分类树管理 | **P0** | 增删改、排序、启用停用；树深 ≤3 层 |
| H1-02 | 文档列表（管理端） | **P0** | 列：标题 / 分类 / 状态 / 排序 / 更新人 / 更新时间 / 版本 |
| H1-03 | 文档新建 / 编辑（Markdown + 预览） | **P0** | 左右分栏，实时预览；自动保存草稿（本地 localStorage，30s） |
| H1-04 | 发布 / 下架 | **P0** | 下架后用户端不可见（含直达 URL） |
| H1-05 | 排序（分类内拖拽 + 数字） | **P0** | `sort_order`，同级生效 |
| H1-06 | 用户端帮助中心（分类树 + 正文） | **P0** | 左侧分类树、中间正文、右侧 TOC |
| H1-07 | Markdown 渲染（代码块/表格/图片/引用） | **P0** | `markdown-it` + `DOMPurify`（D2） |
| H1-08 | 目录 TOC | **P0** | 从 h2–h4 生成，滚动高亮当前节 |
| H1-09 | 关键词搜索（标题 + 正文） | **P0** | 高亮命中；片段 60 字；按相关度排序 |
| H1-10 | 搜索高亮 | **P0** | 标题 + 片段 + 正文内 `<mark>`（在净化后做文本节点替换） |
| H1-11 | 文档删除 | **P0** | 软删；已被收藏/引用不影响（本批无收藏） |
| H1-12 | 文档版本号 | **P0** | 每次发布 `version+1`，用户端显示「版本 N · 最近更新」 |
| H1-13 | 按角色可见（`visible_scope`） | P1 | D8；列与谓词先埋好 |
| H1-14 | 历史版本对比与回滚 | P1 | 保留近 10 个版本（`sys_help_doc_version`） |

## 2.3 变更日志（15 项）

| # | 功能 | 优先级 | 说明 |
|---|---|---|---|
| C1-01 | 变更日志列表 | **P0** | 列：时间 / 操作人 / 模块 / 动作 / 对象 / IP / 结果 |
| C1-02 | 筛选：时间范围 | **P0** | 默认最近 7 天；上限单次查询 366 天 |
| C1-03 | 筛选：操作人 / 模块 / 动作 / 对象 ID | **P0** | 操作人支持模糊（用户名或姓名） |
| C1-04 | 详情：字段级 diff 表 | **P0** | 字段名 / 旧值 / 新值 / 判定；仅列变更字段可切换「只看变更」 |
| C1-05 | 按对象查历史 | **P0** | 用户/角色详情页「变更历史」入口 → 带 `target_type + target_id` 跳转 |
| C1-06 | Excel 导出（脱敏） | **P0** | 同步 ≤1000 行；超过转异步（复用批次 A） |
| C1-07 | 事务内同步写（回滚不留假日志） | **P0** | D10；§5.3 是唯一实现路径 |
| C1-08 | 失败尝试留痕 | **P0** | `result='FAILED'`，独立连接写，`after_data` 必须 NULL |
| C1-09 | 写入即脱敏 + 变更判定 | **P0** | D11；`changed_fields` 用**明文比较**得出，值用 `Mask` 打码后落库 |
| C1-10 | 受管对象注册表（表 → 模块/字段白名单/脱敏类型） | **P0** | 5.4.1；本批接入 6 个对象类型（用户/角色/用户角色授权/部门/公告/帮助文档） |
| C1-11 | 按月分区 + 每日归档 | **P0** | D12；`DETACH → ATTACH`（F31） |
| C1-12 | 归档查询入口 | P1 | 默认只查在线表，勾「含归档」才查归档表 |
| C1-13 | 明文查看（双闸门） | P1 | `business:changelog:export-plain` + `system:field-mask:view-plain` |
| C1-14 | 变更趋势看板（按模块/按人统计） | P1 | 复用批次 BI 的 echarts |
| C1-15 | 可疑变更告警（如 1 小时内同一人对同一对象改 >10 次） | P1 | 写入 `security_logs`，接批次 A 的站内信 |

## 2.4 优先级分布与"不做的事"

| 模块 | P0 | P1 | 合计 |
|---|---|---|---|
| 公告管理 | 14 | 4 | 18 |
| 帮助文档 | 12 | 2 | 14 |
| 变更日志 | 11 | 4 | 15 |
| **合计** | **37** | **10** | **47** |

> **本批不做（与本章功能无关但评审常被问到）**：内容审核流、评论、点赞、收藏、公告多语言、帮助文档评论、变更日志的"一键回滚"、富文本内嵌视频（`<iframe>` 被 ammonia 删除，F19）。完整清单见附录 D。

---

# 第 3 章 公告管理详设

## 3.0 一句话：本模块在做什么

> **现状**：平台没有任何"对多人说话"的能力。发通知只能靠群发微信/邮件，谁看了、谁没看、什么时候过期，全靠人盯。
> **本批**：建一条**有受众边界、有生命周期、有已读回执**的公告通道：管理端能发（含附件、置顶、定时生效、到期自动下线），用户端在顶栏看得到未读数、点开即已读，发布同时走批次 A 的站内信主干道推一条消息。

---

## 3.1 数据模型（4 张新表）

> 命名沿用底座习惯：本批新表统一 `sys_` 前缀（与 `sys_file`/`sys_notice_*` 一致）。**不写 SQL**，字段清单供架构师出迁移。

### 3.1.1 `sys_announcement`（公告主表）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | PK | 主键 |
| `title` | VARCHAR(200) | NOT NULL | 标题 |
| `summary` | VARCHAR(500) | — | 摘要；**站内信正文就用它**（纯文本，≤200 字展示） |
| `content_html` | TEXT | NOT NULL | **净化后**的富文本 HTML（D3：入库前净化） |
| `content_text` | TEXT | NOT NULL | 富文本去标签后的纯文本（搜索与消息摘要用，写入时由服务端生成） |
| `scope_type` | VARCHAR(20) | NOT NULL, CHECK ∈ {ALL, ROLE, DEPT} | 发布范围类型 |
| `status` | VARCHAR(20) | NOT NULL, CHECK ∈ {DRAFT, SCHEDULED, PUBLISHED, REVOKED, EXPIRED} | 状态机见 3.2 |
| `is_top` | BOOLEAN | NOT NULL DEFAULT FALSE | 是否置顶 |
| `top_expire_at` | TIMESTAMPTZ | — | 置顶到期时间；NULL=永久置顶（**界面默认填 7 天**，3.6） |
| `publish_at` | TIMESTAMPTZ | — | 生效时间（定时发布用；发布时若为空则取 now()） |
| `expire_at` | TIMESTAMPTZ | CHECK (expire_at IS NULL OR publish_at IS NULL OR expire_at > publish_at) | 失效时间；NULL=长期有效 |
| `author_id` | UUID | — | 发布人 `sys_user.id` |
| `author_name` | VARCHAR(64) | — | 发布人姓名**快照**（人离职/改名后历史公告不跟着变） |
| `target_count` | INT | NOT NULL DEFAULT 0 | **发布瞬间的受众数快照**（Y 值；扇出后回写） |
| `read_count` | INT | NOT NULL DEFAULT 0 | 已读数（**不维护冗余计数**，见 3.7.4 的说明，此列仅作缓存展示） |
| `attachment_count` | SMALLINT | NOT NULL DEFAULT 0 | 附件数（列表页徽标用，避免 JOIN） |
| `remark` | VARCHAR(255) | — | 内部备注（不对用户展示，供管理员写"为什么要发这条"） |
| 审计字段 | — | — | `created_at / updated_at / create_by / update_by / deleted_at`（与平台一致） |

**索引（4 个，名字必须照抄，便于性能验收对照）**

| 索引名 | 定义 | 用途 |
|---|---|---|
| `idx_ann_list` | `(status, is_top DESC, publish_at DESC) WHERE deleted_at IS NULL` | 管理端 + 用户端列表 |
| `idx_ann_expire` | `(expire_at) WHERE status='PUBLISHED' AND expire_at IS NOT NULL` | 到期翻转任务（3.12） |
| `idx_ann_top_expire` | `(top_expire_at) WHERE is_top AND top_expire_at IS NOT NULL` | 取消置顶任务 |
| `idx_ann_author` | `(author_id, created_at DESC)` | "我发的公告"快速筛选 |

### 3.1.2 `sys_announcement_target`（发布范围明细）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | PK | — |
| `announcement_id` | UUID | NOT NULL, FK→`sys_announcement(id)` | — |
| `target_type` | VARCHAR(10) | NOT NULL, CHECK ∈ {ROLE, DEPT} | **`ALL` 不写明细行**（`scope_type='ALL'` 即全部）；这样"全员公告"不会产生 312 行明细 |
| `target_id` | UUID | NOT NULL | 角色 ID / 部门 ID |
| `include_children` | BOOLEAN | NOT NULL DEFAULT TRUE | **仅部门有意义**：是否含下级。角色一律 TRUE（角色本身无层级） |
| `target_name` | VARCHAR(100) | — | 名称快照（角色/部门被删后仍能显示"发给了市场部"） |

- 唯一约束：`UNIQUE (announcement_id, target_type, target_id)`
- 索引：`idx_target_lookup (target_type, target_id)` —— 用户端可见性判定按此查
- **受众快照**：发布时按上表展开成具体用户列表，落 `sys_notice_receiver`（批次 A 表），`target_count` 写展开后的去重人数。**不另建受众表**——站内信收件人就是受众快照，一份数据两个用途。

### 3.1.3 `sys_announcement_read`（已读，一人一行）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | PK | — |
| `announcement_id` | UUID | NOT NULL | — |
| `user_id` | UUID | NOT NULL | 读者 |
| `read_at` | TIMESTAMPTZ | NOT NULL DEFAULT now() | 首次读时间（重复读不更新） |

- 唯一约束：`UNIQUE (announcement_id, user_id)` —— **幂等的物理保证**（3.7.3）
- 索引：`idx_ann_read_user (user_id, announcement_id)` —— 算"我的未读数"走它
- 保留期：**24 个月**（到期由 04:20 任务清理，见 3.12）

### 3.1.4 表清单变化

| 表 | 动作 | 说明 |
|---|---|---|
| `sys_announcement` | 新建 | 主表 |
| `sys_announcement_target` | 新建 | 范围明细（ROLE/DEPT） |
| `sys_announcement_read` | 新建 | 已读 |
| `sys_announcement_file` | 新建 | 附件关联（`announcement_id / file_id / sort_order`，唯一 `(announcement_id, file_id)`）；文件本体仍在 `sys_file`（详见 3.10） |
| `sys_file` | **零改动** | 附件只存 `file_id`（F36） |
| `sys_notice_message` / `sys_notice_receiver` | **零改动** | 复用批次 A 的站内信（F35） |

---

## 3.2 状态机（5 个状态、8 条转移）

```
        ┌──────────┐   发布(立即)    ┌────────────┐
        │  DRAFT   │───────────────▶│ PUBLISHED  │
        │  草稿    │                │  已发布    │
        └────┬─────┘                └──┬──────┬──┘
             │ 定时发布                 │      │ 撤回
             ▼                          │      ▼
        ┌──────────┐  到点自动发布      │  ┌──────────┐
        │SCHEDULED │───────────────────┘  │ REVOKED  │
        │ 待发布   │                      │ 已撤回   │
        └──────────┘                      └──────────┘
                                            │ 重新发布（生成新 ID）
                                            ▼
                                       （新 PUBLISHED）

   PUBLISHED ──过期(定时任务/懒判定)──▶ EXPIRED（已过期，只读，可删除）
```

| 当前状态 | 动作 | 目标状态 | 前置条件 | 副作用 |
|---|---|---|---|---|
| — | 保存 | DRAFT | 有 `create` 码 | 写变更日志（CREATE） |
| DRAFT | 发布 | PUBLISHED | 有 `publish` 码；标题/正文非空；`publish_at` ≤ now 或为空 | ①受众展开 ②写站内信 ③`target_count` 回写 ④写变更日志 |
| DRAFT | 定时发布 | SCHEDULED | `publish_at` > now | 无（到点由任务发布） |
| SCHEDULED | 到点发布 | PUBLISHED | 任务触发，且未过期 | 同"发布" |
| SCHEDULED | 取消定时 | DRAFT | 有 `publish` 码 | 无 |
| PUBLISHED | 撤回 | REVOKED | 有 `revoke` 码 | 用户端立即不可见；**已读记录保留**；站内信不删（点开提示已撤回） |
| PUBLISHED | 到期 | EXPIRED | `expire_at` ≤ now（任务或懒判定） | 用户端不可见 |
| REVOKED / EXPIRED / DRAFT | 删除 | （软删） | 有 `delete` 码 | 写变更日志（DELETE） |

**硬规则**
1. **`PUBLISHED` 不能直接删除**，必须先撤回（防止"公告消失了但没人知道发生过"）。
2. **编辑 `PUBLISHED` 的公告要求二次确认**：提示「该公告已有 **287** 人读过，修改后他们看到的将是新内容。是否继续？」。
3. **"重新发布"永远生成新 ID**，不复用被撤回的那条（故事 2）。

---

## 3.3 发布范围与受众展开

### 3.3.1 四种组合（三种 `scope_type`，其中 `ALL` 无明细行）

| 场景 | `scope_type` | 明细行 | 受众 |
|---|---|---|---|
| 全员 | `ALL` | 0 行 | 所有 `sys_user.deleted_at IS NULL AND status=1` |
| 指定角色（可多选） | `ROLE` | N 行（N 个角色） | 命中任一角色的**生效授权**用户 |
| 指定部门（可多选，各带"含下级"） | `DEPT` | M 行 | 各自部门的子树并集 |
| 角色 + 部门（同一公告混选） | **不允许** | — | 见下 |

> **为什么不允许"角色 + 部门混选"**：混选会让"受众到底是谁"在事后无法用一句话说清（"发给所有主管 + 财务部所有人"是并集还是交集？）。产品上强行约束：**一次只能选一种范围类型**；要同时覆盖两类人，就发两条公告。这是为了让人在**列表页一眼看懂**这条公告发给了谁。

### 3.3.2 扇出算法（发布时执行，与批次 A 的 `sys_notice_receiver` 落地一致）

```
输入：announcement.id, scope_type, target 明细
1) 求受众集合 U（去重）：
   - ALL  → SELECT id FROM sys_user WHERE deleted_at IS NULL AND status = 1
   - ROLE → SELECT DISTINCT ur.user_id FROM sys_user_role ur
            JOIN sys_role r ON r.id = ur.role_id
            WHERE ur.role_id = ANY(目标角色)
              AND ur.deleted_at IS NULL
              AND r.status = 1
              AND (ur.expire_time IS NULL OR ur.expire_time > now())   ← 口径同批次 B 的"生效角色"（D13）
   - DEPT → 对每个目标部门 d：
              include_children ? find_dept_subtree_ids(d)   ← 复用 F8 的递归
                               : [d]
            再 SELECT id FROM sys_user WHERE dept_id = ANY(展开结果) AND deleted_at IS NULL AND status = 1
2) 排除自己？**不排除**（发布人也要收到，否则"我发的公告我没看到"）
3) 分页批量写 sys_notice_receiver（每批 1000 行，避免 10 万人时单条 SQL 过大）
4) 回写 target_count = |U|
5) 失败处理：扇出失败 → 公告仍为 PUBLISHED，但 target_count = 0 且列表标红「受众推送失败，请点此重试」，
   重试接口为幂等（按 (message_id, user_id) 唯一键去重，3.8.3）
```

### 3.3.3 受众为空时的行为（必须给确定答案，不留"看情况"）

| 情况 | 行为 |
|---|---|
| ALL 但系统只有 1 个超管 | 正常发出，`target_count=1` |
| 选了 3 个角色，其中 2 个角色无人持有 | 只为有人的那个扇出；发布前**预校验**提示「角色『审计员』当前无人持有，将跳过」 |
| 选了部门，但该部门下无人 | 发布前预校验拦截：「部门『已撤销的市场二部』下无在职用户，请重新选择」 |
| 全部目标都无人 | **拒绝发布**（400）：`本次发布的目标范围内没有任何用户，请检查发布范围` |

> 预校验接口 `POST /business/announcement/preview-target` 返回 `{total, by_target:[{name, count}]}`，界面在"发布范围"下方实时显示「预计发送 **28** 人」，发布前必须拿到这个数字（避免"我以为发给了全公司"）。

---

## 3.4 用户端可见性判定（**本模块的安全底线**）

### 3.4.1 判定谓词（唯一一份，四处复用）

> 这个谓词是公告模块的"数据权限"实现。**必须抽成一个函数/方法**（建议 `announcement::visible_scope(user) -> ScopePredicate`），不允许各接口自己写。

```
可见 ⟺ deleted_at IS NULL
      AND status = 'PUBLISHED'
      AND (publish_at IS NULL OR publish_at <= now())
      AND (expire_at  IS NULL OR expire_at  >  now())
      AND (
            scope_type = 'ALL'
         OR (scope_type = 'ROLE' AND EXISTS (命中我的生效角色))
         OR (scope_type = 'DEPT' AND EXISTS (
                target.target_id = ANY(dept_ancestors(我的 dept_id))
                AND (target.include_children = TRUE OR target.target_id = 我的 dept_id)))
      )
```

### 3.4.2 四条必须覆盖的读取路径（少一条就是漏洞）

| # | 路径 | 漏了会怎样 |
|---|---|---|
| 1 | **列表分页**（`/business/announcement/page`） | 直接漏数据 |
| 2 | **详情（按 ID 直查）** —— **最容易被绕过的一处** | 验收场景 2 的"不能靠 ID 直查绕过"就死在这里 |
| 3 | **未读数接口**（`/business/announcement/unread-count`） | 角标数字虚高（把看不见的公告算进未读），用户点进去发现"没有这条" |
| 4 | **标已读接口**（`POST /business/announcement/{id}/read`） | 攻击者可拿别人的公告 ID 刷已读，污染阅读人数 → 阅读人数统计不可信 |

### 3.4.3 「含下级」的判定方向：必须用**祖先链**，不能用子树（实测踩过的坑）

**第一版设计（错的）**：把用户部门的**子树**算出来，判断"公告目标部门是否在我的子树里"。
**实测结果（F/附录 C.7 T5）**：技术部**子部门**（前端组）的用户**看不到**发给「技术部（含下级）」的公告 —— 方向反了。

**正确语义**：用户属于部门 D，公告目标是部门 T（含下级）。可见 ⟺ **D 在 T 的子树内** ⟺ **T 在 D 的祖先链上**。

两种等价写法，选前者：

| 写法 | 复杂度 | 结论 |
|---|---|---|
| ①算 **D 的祖先链**（≤6 次递归），再 `target_id = ANY(祖先数组)` | 每请求 **1 次**递归 | ✅ **采用**。实测可用（附录 C.7 `dept_ancestors`） |
| ②对每个目标部门 T 算 T 的子树，判断 D 是否在内 | 每公告每个目标 **1 次**递归 → N×M 次 | ❌ 列表页会放大成几十次递归 |

实测结果（附录 C.7 T5）：

| 用户所在部门 | 可见公告 |
|---|---|
| 技术部（根） | 国庆放假通知、技术部值班安排 |
| 技术部/前端组 | 国庆放假通知、技术部值班安排、仅本科室(不含下级) |
| 技术部/前端组/一组 | 国庆放假通知、技术部值班安排（**看不到**"仅本科室"那条） |
| 市场部 | 只有国庆放假通知（全员） |

### 3.4.4 按 ID 直查的返回码：**404，不是 403**

| 情况 | 返回 | 文案 |
|---|---|---|
| 公告存在但我不可见 | **404** + `10002 RESOURCE_NOT_FOUND` | 「公告不存在或已下线」 |
| 公告存在、我不可见、但我有管理端 `list` 码 | **403** | 「该公告不在你的发布范围内」 |
| 公告不存在 | 404 | 同上 |

> 沿用批次 B 的 D4 口径：**业务数据越权一律 404**（403 等于告诉探测者"这个 ID 是真的"，可用于枚举"公司有多少条部门公告"）。管理端是例外——管理员本来就知道它存在，403 更有指示性。
> 实测：市场部用户按 ID 直查技术部公告 → **0 行**（附录 C.7 T5 的 `id_direct_probe=0`），同一条 ID 换技术部用户 → **1 行**。说明判定只看部门不看 ID。

### 3.4.5 无部门的用户（`dept_id IS NULL`）

| 范围类型 | 无部门用户可见？ |
|---|---|
| `ALL` | ✅ 可见 |
| `ROLE` | ✅ 按角色判定（无部门不影响） |
| `DEPT` | ❌ **不可见**（祖先链为空集，与批次 B 的 D6"无部门视为空集"口径一致） |

界面上：无部门用户在公告中心顶部提示「你尚未归属部门，只能看到全员公告」（与批次 B 的空部门提示风格一致）。

---

## 3.5 有效期：到点自动上下线（双保险，见 D7）

| 环节 | 做法 |
|---|---|
| **生效判定** | 谓词 `publish_at <= now() AND (expire_at IS NULL OR expire_at > now())`。**所有读取路径必须带**（3.4.2 的四条） |
| **状态翻转** | 任务 `announcement_expire`（每 5 分钟）把 `PUBLISHED AND expire_at <= now()` 改成 `EXPIRED`；把 `SCHEDULED AND publish_at <= now()` 按"发布"流程发布 |
| **边界** | ①`expire_at` 与 `publish_at` 相等 → 保存时拒绝（400，「失效时间必须晚于生效时间」）；②`publish_at` 在过去 → 保存时提示「该时间已过，发布后将立即生效」，允许（常用于补录）；③`expire_at` 已过但仍是 DRAFT → 保存时提示但允许 |
| **时区** | 全部按服务端 `Asia/Shanghai` 解释；前端 `t-date-picker` 传 ISO8601 带时区（`2026-10-01T00:00:00+08:00`），**禁止传裸 `YYYY-MM-DD HH:mm:ss`**（历史上有批次因裸串被当 UTC 存，差 8 小时） |
| **到期后用户端** | 列表消失；若用户已经收藏了 URL（本批无收藏功能）→ 直达详情返回 404「公告已下线」 |
| **到期后管理端** | 状态列 `已过期`（灰），可「删除」；不可「撤回」（已经下线了，撤回无意义） |

**验收要求**：验收场景 3 的"18:00 下线"，**允许最长 5 分钟窗口**（任务间隔），但**谓词必须立即生效**——即 18:00:00 之后用户端列表立刻查不到（不等任务）。这两条分开验：①谓词立即生效（秒级）；②状态列在 5 分钟内变灰。

---

## 3.6 置顶

| 规则 | 定法 |
|---|---|
| 置顶生效条件 | `is_top = TRUE AND (top_expire_at IS NULL OR top_expire_at > now())` |
| 排序 | `置顶生效 DESC, publish_at DESC`（置顶内部仍按时间倒序） |
| 置顶数量 | **不限制条数**，但列表页超过 3 条时在 UI 上折叠：显示前 3 条 + 「还有 N 条置顶」（避免首页被置顶占满） |
| 默认置顶期 | 新建公告勾「置顶」时，`top_expire_at` 默认 **now()+7 天**（故事 1 的"置顶 7 天"就是默认值，HR 不用算日期） |
| 取消置顶 | 手动取消（清 `is_top`）或到期自动（任务 `announcement_top_expire` 每 10 分钟） |
| 权限 | 置顶/取消置顶需 `business:announcement:top`，**单独拆码**（能编辑不等于能霸占首页第一位） |

---

## 3.7 已读 / 未读

### 3.7.1 三个数字的口径（必须分清，界面上三个说法不能混用）

| 指标 | 定义 | 出现在哪 |
|---|---|---|
| **我的未读数** | `可见公告数 − 我已读的可见公告数` | 顶栏角标、公告中心 tab「未读(N)」 |
| **已读人数 / 应读人数** | `count(sys_announcement_read) / target_count` | 管理端列表列「已读 287/312」 |
| **阅读明细** | 逐人已读状态（P1） | 管理端「阅读明细」弹窗 |

### 3.7.2 顶栏角标

| 项 | 定法 |
|---|---|
| 位置 | 顶栏，`article` 图标，在**主题切换按钮右边、消息铃铛左边**（F29 的插入点；批次 A 的铃铛在用户下拉前，两者并排） |
| 样式 | `t-badge :count="unread" :max-count="99"`；0 不显示角标 |
| tooltip | 「公告中心（3 条未读）」 |
| 刷新 | 三个触发点（与批次 A 的消息角标同构）：①登录后立即拉一次；②**60 秒轮询**（仅页面可见时）；③读完之后本地先减 + 后台静默刷新 |
| 失败降级 | 轮询失败**不弹错误**；连续 3 次失败 → 角标变灰点，tooltip「公告状态暂不可用」 |

### 3.7.3 标已读：幂等 + 本地先减

```
用户点开详情
  → 前端：本地 unreadCount -= 1（立即，不等接口）
  → 前端：POST /business/announcement/{id}/read
  → 后端：①先跑 3.4.1 的可见性谓词（不可见直接 404）
          ②INSERT ... ON CONFLICT (announcement_id, user_id) DO NOTHING
          ③返回 {read_at: <首次读时间>}
  → 前端失败处理：接口报错则**回滚本地 -1**（重新拉一次未读数），避免"看起来读了其实没读"
```

- **幂等**：重复调用不报错、不改 `read_at`（唯一约束保证，3.1.3）。
- **不允许"标记全部已读"**：公告是"要看完"的内容，与站内信不同（站内信可以一键已读，公告不行——否则阅读人数统计失去意义）。这条要写进界面提示。

### 3.7.4 阅读人数为什么不维护冗余计数列

`sys_announcement.read_count` 只作**缓存展示**，真实值来自 `count(sys_announcement_read WHERE announcement_id=?)`。
- 理由：全公司公告 312 人同时点开会产生 312 次 `UPDATE sys_announcement SET read_count=read_count+1`，**同一行热点更新**会锁行；而 `count(*)` 走 `UNIQUE (announcement_id, user_id)` 索引的前缀扫描，312 行是微秒级。
- 缓存刷新：`read_count` 由**列表查询时**用一次 `LEFT JOIN LATERAL` 或批量聚合得到，不依赖异步回写（避免"读完人数还是 0"的错觉）。**若性能实测显示批量聚合 >50ms，再降级为异步刷新（P1）**，但 P0 按实时聚合实现。

---

## 3.8 与「消息中心」的关系（D4）

### 3.8.1 结论：**推**，一条公告推一条站内信

| 项 | 定法 |
|---|---|
| 触发时机 | 发布成功后（与扇出同一个流程，站内信收件人 = 受众） |
| `business_key` | `content.announcement` （供"按业务跳转/按业务统计"） |
| `biz_ref_id` | 公告 ID |
| `title` | 公告标题 |
| `content` | **摘要纯文本**（≤500 字；摘要为空时取 `content_text` 前 200 字 + 「…」）。**不推 HTML**——批次 A 的消息正文是纯文本渲染（`white-space: pre-wrap`），推 HTML 会原样显示标签 |
| `level` | 公告有"紧急"标记时 `urgent`，否则 `info`（本批公告表**不加**紧急级别字段，P1 再加；P0 一律 `info`） |
| `action_url` | `/notice/detail/{id}` |
| `action_text` | 「查看公告」 |
| `notice_type` | `business`（不是 `system`，便于用户按类型过滤） |
| 撤回后 | 站内信**不删除**；点「查看公告」→ 详情接口返回 404 → 前端落到「该公告已撤回或已下线」页（8.3.6），并提示「可在公告中心查看其他公告」 |

### 3.8.2 用户在两个地方看到同一件事，会不会重复打扰？

会，但这是**刻意的**：
- 站内信 = **一次性提醒**（可删除、可已读）；
- 公告 = **有生命周期的正式内容**（有有效期、有阅读人数、有附件）。
- 两者各自维护已读状态：**读站内信不自动把公告标已读**（用户可能只看了标题），但点「查看公告」按钮进详情后会触发标已读。界面上的表述必须分开：「消息 3 条未读」 vs 「公告 2 条未读」，**不能合并成一个数字**。

### 3.8.3 幂等与重试

- 站内信写入以 `(message_id, receiver_id)` 唯一键去重（批次 A 的表已有该唯一约束）。
- 发布流程改为**可重入**：公告状态为 `PUBLISHED` 但 `target_count=0` 时，点「重试推送」重新扇出，不会重复发信。

### 3.8.4 降级方案（若批次 A 的消息中心尚未合入主干）

**F35**：底座仓库当前**没有**任何 notice 实现，本批依赖批次 A 的交付物。若时间上错开：

| 方案 | 做法 | 影响 |
|---|---|---|
| 主方案 | 直接写 `sys_notice_message`/`sys_notice_receiver` | 无 |
| 降级 A（推荐） | 公告模块只做**自己的角标**（3.7.2），站内信延后到批次 A 合入后补一条数据迁移脚本 | 用户在公告中心仍能看到公告，只是不弹站内信 |
| 降级 B（不推荐） | 自建一张 `sys_announcement_push` 表 | 会出现两套消息表，后期必须合并，**明确不做** |

> 开发须知：**不要在公告模块里自己实现消息发送**。所有站内信写入必须调用批次 A 的 `notice` 模块公开函数（附录 E.3 的契约），否则批次 D 的审批通知会出现第三套写法。

---

## 3.9 XSS 与正文规范（**本模块的安全红线**）

### 3.9.1 净化发生在**写接口**，不是读接口

| 环节 | 动作 |
|---|---|
| 前端编辑器 | 粘贴时前端净化（编辑器自带的 `pasteFilter`），**只是体验优化，不承担安全职责** |
| **后端保存**（`POST/PUT /business/announcement`） | ①取 `content_html` → ②`nik::xss::sanitize_html()` → ③把净化后的串存 `content_html` → ④同时生成 `content_text`（去标签 + 实体反转义 + 折叠空白） |
| 读取 | 原样返回库里的 `content_html`；**前端 `v-html` 直接渲染，不再做任何转换**（不做前端 markdown 渲染、不做 innerHTML 拼接） |

**为什么必须在写接口净化**：只要有一个出口忘了净化，就等于没做。写接口净化后，**库里的数据本身就是安全的**，导出、搜索摘要、站内信正文、未来的开放接口都自动安全。

### 3.9.2 净化白名单（用默认策略，不自定义 —— 附实测结果）

采用 `nik_utils::xss::sanitize_html`（= ammonia `Builder::default()`）。**实测输入输出**（附录 C.7 有完整输出）：

| 输入 | 输出 | 结论 |
|---|---|---|
| `<script>alert(1)</script><p>正文</p>` | `<p>正文</p>` | ✅ 标签与内容整块删除 |
| `<img src="x.png" onerror="alert(1)">` | `<img src="x.png">` | ✅ 事件属性删除 |
| `<p style="position:fixed;…;background:red">钓鱼</p>` | `<p>钓鱼</p>` | ✅ **style 整条删除** → "全屏遮罩钓鱼"无效 |
| `<p class="t-red">红字</p>` | `<p>红字</p>` | ⚠️ class 删除（见 3.9.3 的样式限制） |
| `<a href="javascript:alert(1)">点我</a>` | `<a rel="noopener noreferrer">点我</a>` | ✅ 危险协议删除 |
| `<a href="https://…" target="_blank">外链</a>` | `<a href="https://…" rel="noopener noreferrer">外链</a>` | ⚠️ **target 被删**（F20：文档说支持，实际不支持） |
| `<iframe src="//evil.com"></iframe>` | （空） | ✅ 整块删除 → **不支持内嵌视频** |
| `<svg onload="alert(1)"></svg>` | （空） | ✅ |
| `<img src="data:text/html;base64,…">` | `<img>` | ✅ data: 协议被删 |
| `<style>@import url(//evil.com/a.css);</style><p>x</p>` | `<p>x</p>` | ✅ |
| `<table>…<th>字段</th>…</table>` | 原样 | ✅ 表格保留 |
| `<pre><code class="language-rust">let a=1;</code></pre>` | `<pre><code>let a=1;</code></pre>` | ⚠️ 语言 class 丢失（这就是 D2 选 Markdown 的原因） |
| `<img src="https://cdn…/a.png" alt="图" width="300">` | 原样 | ✅ |

### 3.9.3 产品必须向编辑者披露的样式限制（写进界面的说明，不是内部约定）

> 编辑器工具栏**只提供**：标题级别（H1–H4）、粗体、斜体、下划线、删除线、有序/无序列表、引用、代码块、表格、链接、图片、对齐、撤销/重做。
> **不提供**：字体、字号、字体颜色、背景色、行高、任意 CSS。
> 原因：服务端净化会删除 `style`/`class` 属性（上表实测），**即使编辑器给了也会保存后消失**——不如不给。若业务方坚持要"红头文件样式"，正确做法是做**公告模板**（预置整篇样式，见附录 B4），而不是放开任意样式。

### 3.9.4 附件与图片的另一个坑

- 富文本里的图片**只接受 https URL**（`data:` 被净化器删除，F19 实测）；上传走批次 A 的文件接口，插入的是该文件的下载 URL。
- 若插入的是私有文件 URL，用户端可能 403（批次 A 的私有文件需 `file:download-private`）→ **公告里的图片必须使用非私有文件**，编辑器插入图片时强制把 `category` 设为 `announcement` 的**公有**分类；后端保存时校验：`content_html` 里引用的 `file_id` 若为私有，直接拒绝并提示「公告图片不能使用私有文件，请重新上传」。

---

## 3.10 附件（多附件）

| 项 | 定法 |
|---|---|
| 存储 | 只存关联关系：新表 `sys_announcement_file (announcement_id, file_id, sort_order)`；文件本体与元数据全在 `sys_file`（F36，**零改动**） |
| 上限 | 单公告 **≤10 个**、合计 **≤50MB**（单文件上限由批次 A 的 `sys_storage_policy` 决定，本模块不重复定义） |
| 允许类型 | 走批次 A 的 `document` 分类白名单（pdf/doc/docx/xls/xlsx/ppt/pptx/txt/zip）；**禁止**在公告里挂可执行的 `.exe/.js/.html`（即使白名单里有 `.html` 也不许——富文本 + HTML 附件是钓鱼组合） |
| 用户端下载 | 走批次 A 的下载接口；非私有文件直接下；文件名用 `original_name` |
| 附件变更 | 编辑公告增删附件 → 写变更日志（对象=公告，字段=`attachments`，值记录"3 个附件（+1/-1）"，不记录文件内容） |
| 删除公告 | 附件关联软删；`sys_file` 里的文件**不删**（可能被别的公告/业务引用，批次 A 的文件是共享资产） |

---

## 3.11 性能与容量（给测试的量化基线）

| 场景 | 数据规模 | 目标 |
|---|---|---|
| 用户端公告列表（第 1 页） | 全库 2,000 条公告，用户可见 800 条 | P95 **< 150ms** |
| 未读数接口 | 同上 | P95 **< 30ms**（走 `idx_ann_read_user`） |
| 发布全员公告（扇出） | 312 人 | **< 2s** 完成（含写 312 行 receiver + 1 行 message） |
| 发布全员公告（极值） | 10,000 人 | **< 30s**，超时转后台任务（返回「发布中」，列表显示进度） |
| 公告详情 | 正文 200KB 富文本 | P95 **< 100ms** |
| 已读接口 | — | P95 **< 20ms**，并发 200 QPS 无锁等待（唯一索引 `ON CONFLICT DO NOTHING`） |

**容量估算**：312 人 × 每年 300 条公告 = 9.36 万行 `sys_announcement_read`/年；5 年 47 万行，索引完全够用。若客户规模到 5000 人，5 年 750 万行 —— 仍无需分区（24 个月清理滚动，稳态约 300 万行）。

---

## 3.12 定时任务（3 个，避开现有 03:00–03:40 的日志清理，F15）

| 任务名（`invoke_target`） | cron（五段） | 做什么 | 失败影响 |
|---|---|---|---|
| `announcement_expire` | `*/5 * * * *` | ①`PUBLISHED AND expire_at<=now()` → `EXPIRED`；②`SCHEDULED AND publish_at<=now()` → 执行发布 | 用户端不受影响（谓词兜底，D7）；仅管理端状态列滞后 |
| `announcement_top_expire` | `*/10 * * * *` | `is_top AND top_expire_at<=now()` → `is_top=FALSE` | 置顶多挂 10 分钟 |
| `announcement_read_cleanup` | `20 4 * * *` | 物理删除 `read_at < now()-24 months` 的已读行，每批 1000 行循环 | 表变大，不影响功能 |

> 用**五段 cron**（与 `backend/src/task/mod.rs:45-55` 现有写法一致，F15/F26）；不要写批次 A 文档里的六段写法（`0 50 3 * * *?`）——本仓库尚未用过六段，统一一种减少踩坑。

---

## 3.13 边界清单（逐条给行为，不留空白）

| # | 边界 | 行为 |
|---|---|---|
| 1 | 标题为空 / 纯空格 | 拒绝：`请输入公告标题` |
| 2 | 标题 201 字 | 拒绝：`标题不能超过 200 个字` |
| 3 | 正文为空（只有图片没有文字） | **允许**（图片公告是真实需求），但 `content_text` 为空时摘要必须填，否则拒绝：`正文为空时请填写摘要` |
| 4 | 正文 500KB | 拒绝：`正文过大（超过 500KB），请精简或改用附件` |
| 5 | 附件 11 个 | 拒绝：`最多上传 10 个附件` |
| 6 | 有效期结束时间早于开始 | 拒绝：`失效时间必须晚于生效时间` |
| 7 | 选了角色但该角色无人 | 发布前提示跳过；全部为空则拒绝（3.3.3） |
| 8 | 公告已发布后又改了正文 | 允许，但二次确认（3.2 硬规则 2），并写变更日志 |
| 9 | 发布时站内信写入失败 | 公告仍发布成功，`target_count=0`，列表标红 + 重试按钮（3.3.2 第 5 步） |
| 10 | 撤回后用户点旧站内信 | 404 → 「该公告已撤回或已下线」页 |
| 11 | 用户无部门 | 只能看 `ALL` 与 `ROLE` 公告（3.4.5） |
| 12 | 同一公告被两人同时点开 | 各自 `ON CONFLICT DO NOTHING`，无锁冲突 |
| 13 | 定时发布的公告在草稿态被删除 | 允许（删除即取消） |
| 14 | 管理员删除自己发的、已发布的公告 | **不允许**，必须先撤回（3.2 硬规则 1） |
| 15 | 富文本里带 `<iframe>` 视频 | 保存后标签被净化删除（F19），**保存接口在净化前后 diff 出被删除的标签时，返回一句提示**：`已移除不支持的标签：iframe（公告不支持内嵌视频）` |

> 第 15 条是"净化不静默"原则：**净化必须让作者知道内容被改了**，否则作者以为发出去了，读者看到的却是缺一块的公告。

---

# 第 4 章 帮助文档详设

## 4.0 一句话：本模块在做什么

> **现状**：平台没有"说明书"。新人问"怎么请假"只能问同事；同一类问题每年被问 300 次；系统改了流程，没人知道去哪儿看新版。
> **本批**：建一个**可维护、可搜索、可导航**的帮助中心：管理端用 Markdown 写文档（带实时预览），用户端左侧分类树、中间正文、右侧目录，顶部一个搜索框就能找到答案。

---

## 4.1 格式选型：Markdown（D2）—— 三个硬理由

| # | 理由 | 依据 |
|---|---|---|
| 1 | **代码块要语言标记做高亮**。富文本路线下 `class="language-xx"` 会被净化器删除（F19 实测），代码块永远没有高亮 | 3.9.2 实测表 |
| 2 | **TOC 天然可得**：Markdown 的 `#` 层级就是目录结构，渲染后按 h2–h4 直接生成；富文本要靠编辑器维护大纲 | — |
| 3 | **文档要能被人 diff**：纯文本才能做"版本对比"（H1-14），富文本 HTML 的 diff 输出没人看得懂 | — |

**代价（必须承认）**：编辑者需要会一点 Markdown。缓解措施：
- 编辑器提供**工具栏**（标题/粗体/列表/表格/代码块/链接/图片），点按钮自动插入语法，不要求背语法；
- 右侧**实时预览**，写错了立刻看见；
- 提供 3 篇模板（"操作步骤型"/"常见问题型"/"权限申请型"）一键新建。

**渲染链路（必须严格按此，不许换实现）**

```
DB(content_md 原文)
  → 后端返回原文（不做任何 HTML 转换）
  → 前端 markdown-it({ html: false, linkify: true, breaks: true })
  → 前端 DOMPurify.sanitize(html, 白名单配置)
  → v-html 输出
  → 生成 TOC（遍历渲染后的 h2/h3/h4 加 id）
```

> `html: false` 是关键：Markdown 里的裸 HTML（`<script>`、`<img onerror>`）会被**当成文本转义显示**，而不是执行。这样"作者写什么就是什么"，天然规避了最大一类 XSS。DOMPurify 是第二层（防 markdown-it 插件/未来配置变更引入的漏洞）。

---

## 4.2 数据模型（2 张新表 + 1 张 P1 预留）

### 4.2.1 `sys_help_category`（分类树）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | PK | — |
| `parent_id` | UUID | 可空 | 根分类为空；**树深 ≤3 层**（后端保存时校验，超过报「分类最多 3 层」） |
| `name` | VARCHAR(100) | NOT NULL | 分类名；**同父下同名唯一**（软删下唯一索引） |
| `icon` | VARCHAR(50) | — | TDesign 图标名（从 F26 的 2356 个里选，前端有 `IconSelect` 组件） |
| `sort_order` | INT | NOT NULL DEFAULT 0 | 同级排序，越小越前 |
| `visible_scope` | VARCHAR(20) | NOT NULL DEFAULT 'ALL' | `ALL` / `ROLE`（P1 用，D8；P0 恒为 `ALL`） |
| `status` | SMALLINT | NOT NULL DEFAULT 1 | 1 启用 / 0 停用（停用=整棵子树在用户端隐藏） |
| 审计字段 | — | — | 含 `deleted_at` |

- 索引：`idx_help_cat_parent (parent_id, sort_order)`；`UNIQUE (parent_id, name) WHERE deleted_at IS NULL`

### 4.2.2 `sys_help_doc`（文档）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | PK | — |
| `category_id` | UUID | 可空 | 允许"未分类"（草稿期常见）；发布时若为空则拒绝：「请选择所属分类」 |
| `title` | VARCHAR(200) | NOT NULL | 标题（同时作为 TOC 顶部与搜索结果主行） |
| `content_md` | TEXT | NOT NULL | **Markdown 原文**（唯一事实源） |
| `content_text` | TEXT | NOT NULL | 纯文本（去 Markdown 语法后，用于搜索与片段）。由服务端在保存时生成：去掉 `#`/`*`/`` ` ``/链接语法/图片语法，保留文字 |
| `summary` | VARCHAR(300) | — | 摘要（搜索结果显示；为空时取 `content_text` 前 120 字） |
| `sort_order` | INT | NOT NULL DEFAULT 0 | 分类内排序 |
| `status` | VARCHAR(20) | NOT NULL, CHECK ∈ {DRAFT, PUBLISHED, OFFLINE} | 草稿 / 已发布 / 已下架 |
| `visible_scope` | VARCHAR(20) | NOT NULL DEFAULT 'ALL' | 同分类（P1） |
| `version` | INT | NOT NULL DEFAULT 1 | 发布时 +1（H1-12） |
| `author_id` / `author_name` | UUID / VARCHAR(64) | — | 最后编辑人（快照） |
| `published_at` | TIMESTAMPTZ | — | 最近一次发布时间 |
| 审计字段 | — | — | 含 `deleted_at` |

- 索引：
  - `idx_help_doc_cat (category_id, sort_order) WHERE deleted_at IS NULL`
  - `idx_help_doc_title_trgm USING gin (title gin_trgm_ops)`
  - `idx_help_doc_body_trgm USING gin (content_text gin_trgm_ops)`
  - `idx_help_doc_search (status, published_at DESC) WHERE deleted_at IS NULL AND status='PUBLISHED'`

### 4.2.3 `sys_help_doc_version`（P1 预留）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | — |
| `doc_id` | UUID | 文档 |
| `version` | INT | 版本号 |
| `title` / `content_md` | VARCHAR(200) / TEXT | 该版本的快照 |
| `created_by` / `created_at` | — | 谁在什么时候发布的 |

保留策略：每篇文档**保留最近 10 个版本**，超出删最旧的。P1 做"版本对比 + 回滚"，P0 只建表不写数据也可（避免二次迁移）。

---

## 4.3 分类树规则

| 规则 | 定法 |
|---|---|
| 层级 | 最多 3 层（一、二、三级）；第 4 层保存报错 |
| 排序 | 拖拽（`sort_order` 交换）+ 手动填数字；同级生效 |
| 停用分类 | 整棵子树在用户端隐藏；管理端显示为灰色 + 「已停用」，其下文档仍可编辑 |
| 删除分类 | **有子分类或有已发布文档 → 拒绝**：「该分类下还有 3 篇已发布文档 / 2 个子分类，不能删除」；仅可删除"空分类" |
| 改名 | 允许；用户端面包屑立即变（不做 URL 变更，见 4.9.2） |
| 用户端展示 | 只展示"启用 + 至少有一篇已发布文档"的分类（空分类不显示，避免点了没内容）；无任何可见文档的分类整枝隐藏 |

---

## 4.4 文档状态与版本

```
DRAFT（草稿）──发布──▶ PUBLISHED（已发布）──下架──▶ OFFLINE（已下架）
   ▲                        │                            │
   └────── 编辑（不改状态）───┘                            │
                            └────── 重新发布 ─────────────┘
```

| 状态 | 用户端可见 | 可编辑 | 说明 |
|---|---|---|---|
| `DRAFT` | ❌ | ✅ | 只有作者与管理端能看到 |
| `PUBLISHED` | ✅ | ✅（改了立即生效，`version` 不变，发布时才 +1） | 见下方"编辑即生效"说明 |
| `OFFLINE` | ❌（直达 URL 也拒绝，提示「该文档已下架」） | ✅ | 管理端标灰 |

> **"编辑即生效"的取舍**：P0 不做"草稿分叉"（即已发布文档另存草稿）——那需要 `sys_help_doc_draft` 表和"预览草稿/发布草稿"的完整流程，属于版本管理范畴（P1）。P0 的规则是：**改了就生效**，界面明确提示「保存后立即对用户生效」。
> 若管理员的改动需要"先内部看一遍"，用「先下架 → 改 → 再发布」的方式（下架期间用户看不到，界面上会提示）。

---

## 4.5 搜索（本模块体验的重点）

### 4.5.1 匹配范围与排序

| 项 | 定法 |
|---|---|
| 匹配字段 | `title` + `content_text`（**不匹配 Markdown 语法符号**，因为已在 `content_text` 里去掉） |
| 匹配方式 | `ILIKE '%kw%'`（大小写不敏感，中文无大小写，等价于 `LIKE`） |
| 多关键词 | 空格分词，**AND** 语义（每个词都要命中，可以分别在标题或正文里）；最多 5 个词 |
| 特殊字符 | `%` `_` `\` 必须转义（否则用户输 `%` 会命中全部） |
| 排序 | ①标题命中 > 正文命中；②标题命中位置越靠前越前；③`published_at DESC`；④`sort_order` |
| 分页 | 默认 20 条/页；总数用 `count(*)` 精确值（不估算，因为要显示"共 63 条"） |
| 范围 | 只搜 `status='PUBLISHED'` 且分类启用的文档（P1 加 `visible_scope`） |

### 4.5.2 结果呈现（三处高亮，缺一处用户就觉得"没搜到"）

| 位置 | 做法 |
|---|---|
| 标题 | 命中词用 `<mark>` 包裹（前端在纯文本上做替换，不拼接 HTML） |
| 片段 | 后端返回**命中位置前后各 30 字**的片段（共 60 字，命中词在中间），前端高亮 |
| 正文内 | 进入详情时，正文渲染完成后按关键词在**文本节点**里做包裹（`TreeWalker` 遍历文本节点，`document.createTextNode` + `<mark>`），**绝不用正则替换 innerHTML**（会破坏标签结构并可能引入注入） |

### 4.5.3 性能实测（F33）与 P0 决策

| 关键词长度 | 例子 | 执行计划 | 5000 篇耗时 |
|---|---|---|---|
| 2 字 | `请假` | **Seq Scan**（trgm 无法为 2 字生成三元组） | **9.75ms** |
| 3 字 | `请假流` | Bitmap Index Scan | **0.25ms** |
| 4 字 | `请假流程` | Bitmap Index Scan | **0.21ms** |

**P0 决策**：
1. **两个 trgm 索引照样建**（3 字以上直接受益，且随文档增长收益变大）；
2. **2 字关键词走全表扫描是可接受的**：5000 篇 9.75ms，20000 篇约 40ms（线性），远低于 200ms 红线；文档量是关键变量，不是关键词长度；
3. **不上"中文分词"**（`zhparser`/`pg_bigm` 都不在 `postgres:16-alpine` 默认镜像里，F34），本批不引入新扩展；
4. **性能红线**：`pg_relation_size('sys_help_doc')` 超过 **200MB** 或 P95 > 200ms 时，启动优化项（见附录 B5）。

### 4.5.4 `pg_trgm` 不可用时的降级（F34：底座没有任何 `CREATE EXTENSION` 迁移）

| 情况 | 做法 |
|---|---|
| 迁移里 `CREATE EXTENSION IF NOT EXISTS pg_trgm` 成功 | 正常建 trgm 索引 |
| 无权限（DB 用户非属主 / 云托管不允许） | **迁移不失败**：把扩展与索引拆成两个独立迁移文件，索引迁移捕获异常并记录日志后跳过；搜索功能用纯 `ILIKE` 跑（3000 篇内 < 10ms，可接受） |
| 明确要求"不许装扩展" | 同上，且把 `SysConfig` 键 `help.search.trgm=false` 写死，界面不做任何提示（用户无感知） |

> 这条要求开发**不要**把 `CREATE EXTENSION` 写进主迁移的第一行——否则在没权限的环境里整个迁移事务失败，导致**上线卡死**。

---

## 4.6 渲染规范（前端实现约定）

| 项 | 定法 |
|---|---|
| 标题锚点 | h1–h4 生成 `id="h-{序号}-{去标点后的标题}"`；TOC 点击滚动到锚点（考虑顶栏固定高度，`scroll-margin-top: 80px`） |
| TOC 生成 | 只取 h2/h3/h4（h1 视为文档标题，不进 TOC）；无标题时 TOC 区域显示「本文暂无目录」 |
| TOC 高亮 | `IntersectionObserver` 监听标题进入视口，高亮当前项；**不要**用 scroll 事件算 offset（抖动） |
| 代码块高亮 | `markdown-it` + `highlight.js`（只装常用语言：bash/sql/json/js/ts/rust/java/python/yaml/http），语言未识别时不高亮但保留 `<pre><code>` |
| 代码块 UI | 右上角「复制」按钮；横向滚动不换行（`overflow-x:auto`） |
| 表格 | 外层包 `div.table-wrap{overflow-x:auto}`（宽表格在窄屏可横滚） |
| 图片 | `max-width:100%`；点击在 `t-dialog` 里放大预览（复用批次 A 的图片预览组件） |
| 外链 | Markdown 链接统一加 `target="_blank" rel="noopener noreferrer"`（**在 DOMPurify 之后**用属性设置，不是在 HTML 字符串里拼） |
| 换行 | `breaks: true`（单换行输出 `<br>`，符合"写文档的人"的直觉） |
| 字体/字号 | 全部用 CSS 变量，继承主题（22 套配色切换时正文颜色跟着变，**禁止硬编码色值**） |
| 打印 | P1；P0 保证 `@media print` 下隐藏 TOC/顶栏即可 |

---

## 4.7 XSS（Markdown 路线的三条防线）

| 防线 | 做法 | 为什么 |
|---|---|---|
| ① 后端**不渲染** Markdown | 后端只存原文、只做纯文本提取；**任何情况下不在服务端把 Markdown 转 HTML** | 服务端一旦渲染，就必须在服务端净化；而且渲染结果要落库/缓存 → 又多一份可被污染的 HTML。**不渲染 = 不产生 HTML = 没有服务端 XSS 面** |
| ② `markdown-it` 关闭 raw HTML | `html: false`（默认就是 false，但必须**显式写出并加注释**，防止后人改） | 裸 `<script>` 会被转义成文本，最危险的一类直接消失 |
| ③ `DOMPurify` 二次净化 | 白名单标签：`p,br,hr,h1-h6,strong,em,u,s,del,code,pre,blockquote,ul,ol,li,table,thead,tbody,tr,th,td,a,img,span,div`；**禁止** `style`/`class`/`script`/`iframe`/`svg`；`a` 只允许 `href,title,target,rel`；`img` 只允许 `src,alt,width,height`；`ALLOW_DATA_ATTR: false` | 防"未来某个 markdown-it 插件引入 HTML"、"作者用 `javascript:` 链接"等 |

**图片与链接的额外规则**
- `img src` 只允许 `http/https`（DOMPurify 的 `ALLOWED_URI_REGEXP`）；
- 图片若来自本系统的私有文件，需要 `file:download-private` 才能显示——**帮助文档的图片必须是公有文件**（与 3.9.4 一致，保存时校验）。

**测试必须覆盖的 8 个载荷**（写进测试用例，任一执行即不通过）
```
1) <script>alert(1)</script>
2) <img src=x onerror=alert(1)>
3) [点我](javascript:alert(1))
4) ![x](javascript:alert(1))
5) <a href="javascript:alert(1)">x</a>
6) <svg onload=alert(1)>
7) <iframe src="javascript:alert(1)">
8) [x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)
```
预期：1/5/6/7 显示为**文本**（`html:false` 转义）；3/4/8 链接被 **DOMPurify 移除 href/src**，点击无反应、元素仍在。

---

## 4.8 是否支持"按角色可见"（任务书要求给建议）—— **建议 P0 不做，P1 做**

| 维度 | 不做（P0） | 做（P1） |
|---|---|---|
| 好处 | 简单；帮助文档本来就是"给所有人看的" | 可做"主管专属 FAQ""财务专属流程" |
| 代价 | — | ①分类树要按角色过滤（看不见的分类不显示，空分类隐藏）；②搜索也要过滤；③"我搜不到是因为没权限还是真没有"的困惑；④每加一个角色可见文档，就多一处权限判定 |
| **结论** | ✅ **P0 全部登录用户可读**（与任务书一致），但**列 `visible_scope` 与统一谓词先埋好** | 当出现"某类文档不能给全员看"的真实诉求时，只改谓词 + 加一个可见范围选择器，**零迁移** |

> 这是"预留但不实现"的典型做法：**表结构与谓词先到位**，P1 只开界面开关。不要等到 P1 再改表（那时已有数据，加 NOT NULL 列要停机）。

---

## 4.9 用户端体验细节

### 4.9.1 页面结构（三栏）

```
┌──────────────────────────────────────────────────────────────────┐
│ 顶栏（面包屑：首页 / 帮助中心 / 请假流程与审批时限）              │
├───────────────┬──────────────────────────────────┬───────────────┤
│ 分类树(240px) │ 正文（自适应，max-width 860px）   │ TOC(200px)    │
│ ▾ 人事         │ # 请假流程与审批时限             │ 一、适用范围  │
│   · 请假流程   │ > 适用范围说明…                   │ 二、申请步骤  │
│ ▾ 财务         │ ## 一、适用范围 …                 │ 三、审批时限  │
│ ▾ 系统         │ ## 二、申请步骤 …                 │ 四、常见问题  │
│               │ ## 三、审批时限 …                 │               │
│               │ 最近更新 2026-09-12 · 版本 4      │               │
└───────────────┴──────────────────────────────────┴───────────────┘
```

- 搜索框在分类树上方（顶栏下沿），占满左栏宽度；
- 搜索激活时，**中间正文区切换为结果列表**（不跳页），点结果回来；
- 窄屏（<1200px）：TOC 收起为"目录"按钮（`t-drawer`），分类树可折叠。

### 4.9.2 URL 规则（为什么用 ID 不用标题）

| 页面 | URL |
|---|---|
| 帮助中心首页 | `/help` |
| 分类 | `/help?cat={category_id}` |
| 文档 | `/help/doc/{doc_id}?kw={关键词}` |
| 搜索 | `/help?kw={关键词}` |

**用 ID 不用 slug**：分类改名/文档改名后旧链接仍然有效（改 slug 会造成 404 与 SEO 灾难，而后台系统没有 SEO 需求，只有"分享给别人"的需求）。

### 4.9.3 空态与异常态

| 场景 | 展示 |
|---|---|
| 没有任何可见文档 | 插画 + 「帮助文档正在建设中」（不是"暂无数据"这种机械文案） |
| 搜索无结果 | 「没有找到与「请假」相关的文档」+ 三条建议：①换个说法（如"休假"）②检查错别字 ③联系管理员 |
| 文档已下架 | 「该文档已下架，可能正在更新中」+ 「返回帮助中心」按钮 |
| 文档不存在 | 404 页（全局） |
| 分类为空（所有文档都下架了） | 该分类整枝隐藏（4.3） |

---

## 4.10 性能

| 场景 | 数据规模 | 目标 |
|---|---|---|
| 帮助中心首页（分类树 + 默认文档） | 300 分类 / 5000 文档 | P95 **< 200ms** |
| 搜索（2 字关键词，最坏） | 5000 文档 | **< 150ms**（实测 9.75ms，留余量） |
| 文档详情（含 2000 行大文档） | 500KB Markdown | P95 **< 300ms**（主要是前端渲染耗时，需实测） |
| 分类树接口 | 300 节点 | **< 20ms**（一次查全表在内存组树，不要递归查库） |

---

## 4.11 边界清单

| # | 边界 | 行为 |
|---|---|---|
| 1 | 分类第 4 层 | 拒绝：`分类最多 3 层` |
| 2 | 同父下重名分类 | 拒绝：`同级下已存在同名分类` |
| 3 | 删除有内容的分类 | 拒绝并给出数量（4.3） |
| 4 | 文档标题为空 / 200+ 字 | 拒绝：`请输入标题` / `标题不能超过 200 个字` |
| 5 | 文档正文 2MB | 拒绝：`文档过大（超过 2MB）` |
| 6 | 搜索关键词 1 个字符 | **允许**（中文单字有意义，如"假"），但提示「搜索结果可能较多，建议输入更完整的词」 |
| 7 | 搜索 100 个字符 | 截断到 50 字并提示 |
| 8 | 搜索含 `%` / `_` | 转义后当普通字符匹配 |
| 9 | 已发布文档改标题 | 允许；URL 用 ID 所以旧链接不断（4.9.2） |
| 10 | 同一文档两人同时编辑 | **后保存者覆盖**（P0 不做乐观锁），但保存前对比 `updated_at`：若已被他人修改，弹出「该文档已被 王芳 于 14:31 修改，你的修改将覆盖，是否继续？」 |
| 11 | 文档引用不存在的图片 URL | 显示破图；保存时静态检查 `![](…)` 中的域名是否在本系统白名单，不在则提示（不阻断） |
| 12 | 编辑器未保存就关闭 | 草稿自动存 localStorage（30s 一次），重进时提示「检测到未保存的草稿，是否恢复？」 |

---

# 第 5 章 变更日志详设

## 5.0 一句话：本模块在做什么

> **现状**：系统有 5 张日志表（`operation_logs` / `audit_logs` / `auth_logs` / `security_logs` / `password_change_logs`），日志页有 5 个 tab（F16）。但**没有任何一处**能回答"某条数据被谁改成什么样"：审计 tab 只有操作码和对象 ID，没有字段级前后值，不能按模块筛、不能导出、没有"结果"列；更要紧的是——**审计日志是异步写的，业务回滚后它照样落库（F10 实测 T3）**，即"日志说改了、数据没改"。
> **本批**：建一张**与业务同生共死**的变更日志表 `sys_change_log`：事务内同步写、只记成功事实、字段级 diff、写入即脱敏、按月分区、可归档可导出。它不替代审计日志，两者分工见 5.2。

---

## 5.1 数据模型

### 5.1.1 `sys_change_log`（在线表，按月 RANGE 分区）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | NOT NULL，与 `created_at` 组成 PK | `UUID v7`（时间有序，写入局部性好） |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT now()，**分区键** | 发生时间 |
| `request_id` | VARCHAR(36) | NOT NULL | 一次请求贯穿（与 `operation_logs.request_id` 同值，可跨表串联） |
| `module_code` | VARCHAR(20) | NOT NULL | 模块：`system` / `content` / `support`（列表筛选"模块"列用它） |
| `operation` | VARCHAR(50) | NOT NULL | 操作码：`USER_UPDATE` / `ANNOUNCEMENT_PUBLISH` …（复用 `log_operation` 字典） |
| `action` | VARCHAR(10) | NOT NULL, CHECK ∈ {CREATE, UPDATE, DELETE} | 动作：新增/修改/删除（**只这三个**，"发布/撤回/置顶"归入 UPDATE，见 5.5.3） |
| `target_type` | VARCHAR(50) | NOT NULL | 对象类型：`User` / `Role` / `Dept` / `Announcement` / `HelpDoc`（复用 `log_target_type` 字典并扩充） |
| `target_id` | VARCHAR(100) | NOT NULL | 对象 ID（字符串，兼容未来非 UUID 主键的业务） |
| `target_name` | VARCHAR(200) | — | 对象名称**快照**（对象被删后仍可读："用户 zhangwei"） |
| `result` | VARCHAR(10) | NOT NULL DEFAULT 'SUCCESS', CHECK ∈ {SUCCESS, FAILED} | 结果 |
| `changed_fields` | JSONB | NOT NULL DEFAULT `'[]'` | **明文比较**得出的真实变更字段数组：`["mobile","dept_id"]`（脱敏前的判定结果，5.4.3） |
| `change_summary` | VARCHAR(500) | — | 人读摘要：`手机号、所属部门`（列表"变更内容"列直接显示） |
| `before_data` | JSONB | — | **已脱敏**的旧值，只含 `changed_fields` 中的字段 |
| `after_data` | JSONB | — | **已脱敏**的新值，只含 `changed_fields` 中的字段 |
| `operator_id` | UUID | — | 操作人 |
| `operator_name` | VARCHAR(50) | — | 操作人账号快照 |
| `operator_dept_id` | UUID | — | 操作人部门快照（P1 做"部门维度统计"用） |
| `ip` | INET | — | 客户端 IP |
| `user_agent` | TEXT | — | UA |
| `extra` | JSONB | — | 预留：批量操作的 `batch_id`、导入的 `task_id` 等 |

**主键与分区的硬约束**（实测 F31）
- `PRIMARY KEY (id, created_at)` —— **分区表的唯一约束必须包含分区键**，只写 `PRIMARY KEY (id)` 会建表失败。
- 分区策略：`PARTITION BY RANGE (created_at)`，**每月一个分区**（`sys_change_log_2026_10` …），另建 `sys_change_log_default` 兜底分区（防止定时建分区失败时写入报错）。

**CHECK 约束（第三道闸门，实测 T4 生效）**
```sql
CONSTRAINT ck_cl_result  CHECK (result IN ('SUCCESS','FAILED')),
CONSTRAINT ck_cl_action  CHECK (action IN ('CREATE','UPDATE','DELETE')),
CONSTRAINT ck_cl_failed_no_snapshot CHECK (result <> 'FAILED' OR after_data IS NULL)
```
> 最后一条是**产品级护栏**：失败记录**不允许**带"变更后快照"。这样即使开发写错代码，也无法在库层面产生"看起来成功了"的失败记录。实测：插入 `result='FAILED'` + `after_data` 非空 → `check_violation`（附录 C.7 T4）。

**索引（6 个）**

| 索引名 | 定义 | 用途 |
|---|---|---|
| `idx_cl_target` | `(target_type, target_id, created_at DESC)` | **按对象查历史**（最高频） |
| `idx_cl_operator` | `(operator_id, created_at DESC)` | 按操作人筛 |
| `idx_cl_module` | `(module_code, action, created_at DESC)` | 按模块 + 动作筛 |
| `idx_cl_created` | `(created_at DESC)` | 时间范围查询 / 归档扫描 |
| `idx_cl_before_gin` | `USING gin (before_data)` | 按字段名筛（"谁改过手机号"）。**必须用默认 `jsonb_ops`**，`jsonb_path_ops` 不支持 `?` 操作符（F32 实测） |
| `idx_cl_after_gin` | `USING gin (after_data)` | 同上 |

> ⚠️ **不要**改成 `jsonb_path_ops`：它体积小 30%，但不支持 `?`/`?|`（键存在查询），而"筛某个字段被改过"正是本模块的核心筛选之一（F32 实测：`after_data ? 'mobile'` 在 `jsonb_path_ops` 下退化为 Seq Scan）。

### 5.1.2 `sys_change_log_archive`（归档表）

| 项 | 定法 |
|---|---|
| 结构 | **与在线表完全同构**（同列、同 CHECK、同分区键），也是按月 RANGE 分区 |
| 用途 | 只读查询（P1 的"含归档"开关）、合规调阅、到期销毁 |
| 写入 | **只有归档任务会往里 ATTACH 分区**，业务代码永不写入 |
| 索引 | 只建两个：`(target_type, target_id, created_at DESC)` 与 `(created_at DESC)`（归档查询低频，少建省空间） |

---

## 5.2 与既有四张日志表的分工（**必须讲清，否则会被问"这不是重复造轮子吗"**）

### 5.2.1 一张表说清各自负责什么

| 表 | 谁写 | 回答什么问题 | 写入时机 | 保留 | **能否用来判定"操作成功"** |
|---|---|---|---|---|---|
| **`sys_change_log`（本批新建）** | 业务 handler 显式调用 | **"某个对象的某个字段，被谁从什么改成了什么"** | **业务事务内同步写** | 在线 12 个月 + 归档 5 年 | ✅ **可以**（强一致，回滚即无） |
| `audit_logs`（已有） | 各 handler 调 `record_audit` | "什么操作发生过"（含登录、导出、角色分配） | **异步总线，事务外**（F10） | 90 天（F15） | ❌ **不可以**（可能滞后或丢失，且不含模块/动作/结果列，F9） |
| `security_logs`（已有） | 中间件 / 认证 / 业务 | "谁被拒绝了、谁在试探"（登录失败、越权、注入） | 异步 | 90 天 | ❌ 只记"非成功"事件 |
| `operation_logs`（已有） | 中间件自动（`AUDITED_METHODS`，`constants.rs:211`） | "调了哪个接口、耗时多少、状态码多少" | 异步 | 90 天 | ⚠️ **接口级**成功，不是数据级 |
| `auth_logs` / `password_change_logs`（已有） | 认证 / 用户模块 | 登录、改密 | 异步 | 90 天 | — |

### 5.2.2 三句话记住分工

1. **`operation_logs` 是"谁按了哪个按钮"**（接口维度，排查性能与故障用）；
2. **`audit_logs` 是"发生过什么"**（事件维度，合规看"有没有做"，90 天滚动）；
3. **`sys_change_log` 是"数据变成了什么"**（数据维度，追责与回溯，长保留、可导出、可归档）。

### 5.2.3 为什么**不**改造 `audit_logs` 而是新建表（D9 的完整理由）

| 若改造 `audit_logs` | 代价 |
|---|---|
| 加 3 列（`module_code` / `action` / `result`） | 迁移 + 10 处 handler 补参数 |
| 加索引 `(target_type, target_id)` | 已有表 90 天滚动，还好 |
| **把异步总线改成事务内同步** | 要改 `bus.rs` 的全局投递语义，牵动 5 张日志表（`flush`、批量落库、`plugin.stop()` 全要重审），且**登录日志、文件日志等"非业务事务"事件没有 tx 可挂** |
| 结果 | 一个表承担两种语义（"尽力而为的事件" + "强一致的变更事实"），**两种语义会互相污染**：要么变更日志变得可以丢，要么登录日志被迫进事务 |

**结论**：新建表，两种语义物理隔离；`audit_logs` **一行不改**（零回归风险）。这不是"重复"，是**按一致性要求分区**。

---

## 5.3 事务一致性：**回滚不留假日志**（本批最关键的一节）

### 5.3.1 问题是怎么产生的（实测证据）

| 实验 | 做法 | 结果 |
|---|---|---|
| 现状做法（T3） | **事务外**先写日志，再执行业务，业务 `ROLLBACK` | 日志表**仍留 1 行**（`result='SUCCESS'`）→ **假日志** |
| 正确做法（T2） | 日志 `INSERT` **与业务同一事务**，事务 `ROLLBACK` | 日志表 **0 行** → 一致 |

**根因**（F10）：现有审计日志走 `bus.rs` 的**无界异步队列**，worker 每 500ms 或每 256 行批量落库，与业务事务**毫无关系**。业务怎么回滚，都不影响已经入队的日志。这不是 bug，是"日志不得影响业务性能"这个设计目标的**必然副作用**。

### 5.3.2 三道闸门（缺一不可）

| # | 闸门 | 内容 | 谁保证 |
|---|---|---|---|
| **①** | **同事务写** | 日志写入**必须**接收业务事务句柄 `&mut Transaction`，与业务 SQL 在同一个 `COMMIT` 里 | 代码契约（5.3.3）+ 代码评审（附录 E.4） |
| **②** | **失败留痕用独立连接，只在事务失败后写** | 事务返回 `Err` 之后，用 `db()` 新连接写 `result='FAILED'`，且 `before_data/after_data` 均为 NULL | 代码契约 + 单测 |
| **③** | **数据库 CHECK 兜底** | `CHECK (result <> 'FAILED' OR after_data IS NULL)` | 迁移（实测 T4 生效） |

### 5.3.3 唯一写入契约（开发照抄，不要自创）

```rust
// backend/src/shared/change_log.rs（本次新建）
/// 记录一条成功变更。**必须在业务事务闭包内调用**。
pub async fn record(tx: &mut Transaction, input: ChangeLogInput) -> Result<(), DbError>;
//   input: { module_code, operation, action, target_type, target_id, target_name,
//            before: Option<&impl Serialize>,  // 变更前实体（明文）
//            after:  Option<&impl Serialize>,  // 变更后实体（明文）
//            field_policy: &FieldPolicy,       // 受管对象注册表里查到的字段白名单+脱敏类型
//            operator: OperatorCtx, ip, user_agent, request_id, extra }

/// 记录一条失败尝试。**只能在事务返回 Err 之后调用**（用独立连接）。
pub async fn record_failure(db: &Database, input: FailureInput) -> Result<(), DbError>;
//   FailureInput: { module_code, operation, action, target_type, target_id,
//                   operator, ip, user_agent, request_id, error_message }
//   强制：result='FAILED'，before_data=NULL，after_data=NULL，changed_fields=[]
```

调用方写法（业务 handler）：

```rust
let result = db().transaction(|tx| async move {
    // 1) 读旧值（同一个 tx 里读，保证读到的是快照）
    let before = load_user_tx(&tx, &id).await?;
    // 2) 写业务
    to_save.update_tx(&tx).await?;
    // 3) 同事务写变更日志（关键：传 &mut tx）
    change_log::record(&mut tx, ChangeLogInput {
        module_code: "system", operation: "USER_UPDATE", action: "UPDATE",
        target_type: "User", target_id: id.to_string(),
        before: Some(&before), after: Some(&to_save),
        field_policy: policies::USER, operator, ip, user_agent, request_id, extra: None,
    }).await?;
    Ok(())
}).await;

// 4) 事务失败 → 用独立连接留一条 FAILED（可选，允许不写）
if let Err(e) = result {
    let _ = change_log::record_failure(db(), FailureInput { /* ... */ }).await; // 忽略其错误
    return Err(map_err(e));
}
```

### 5.3.4 六条禁止（评审时逐条 grep，命中即打回）

| # | 禁止 | 后果 | 怎么查 |
|---|---|---|---|
| 1 | 在事务**外**调 `change_log::record` | 回滚后留成功日志（T3 实测） | `grep -n "change_log::record" -A3` 看是否在 `transaction(` 闭包内 |
| 2 | 在事务**内**调 `record_failure` | 回滚时失败日志一并消失，等于没记 | 同上 |
| 3 | 用 `log::record_audit`（异步总线）代替 `change_log::record` | 回滚后留成功日志 + 缺字段 diff | `grep enqueue_audit` |
| 4 | **先写日志再写业务** | 顺序颠倒，业务失败时日志已"成功" | 看闭包内语句顺序：读旧值 → 写业务 → 写日志 |
| 5 | 在 `catch`/错误分支里写 `SUCCESS` | 失败被记成成功 | grep `result: "SUCCESS"` 是否出现在 `Err` 分支 |
| 6 | 给 `change_log` 做 `ON CONFLICT DO NOTHING` 静默重试 | 真正的重复写入被吞掉，掩盖并发问题 | 迁移/代码里不得出现 ON CONFLICT |

### 5.3.5 必须补事务的存量写操作（F12 的改动清单）

| # | handler | 现状 | 本批改成 |
|---|---|---|---|
| 1 | `system/user/handler.rs::update` | 单条 UPDATE，无事务 | 包 `db().transaction`（读旧值 + 写 + 日志） |
| 2 | `::create` | 已有事务（`:209`） | 在事务内加 `record` |
| 3 | `::delete` | 已有事务（`:385`） | 加 `record` |
| 4 | `::change_status` | 单条 UPDATE，无事务 | 包事务 + `record` |
| 5 | `::assign_roles` | 已有事务（`:699`） | 加 `record`（action=UPDATE，target=User，字段 `role_ids`） |
| 6 | `::batch_dept` | 已有事务（`:781`） | 每个用户一条 `record`（批量 = N 条，见 5.5.4） |
| 7 | `::reset_password` | 无事务 | **不写变更日志**（密码不属于"可读变更"，只写已有 `password_change_logs`） |

> 第 7 条是一条重要的产品判断：**密码、密钥、令牌永不出现在变更日志里**（连"HASH 变了"都不记，只记"重置了密码"这个事件到 `password_change_logs`）。

---

## 5.4 diff 是怎么算出来的

### 5.4.1 受管对象注册表（表 → 字段白名单 → 脱敏类型）

> 这张表是**变更日志能看懂的前提**。没有它，日志里只有 `dept_id: aaaa-… → bbbb-…`（用户读不懂）。

| `target_type` | 物理表 | `module_code` | 记录字段（白名单） | 脱敏类型（逐字段） |
|---|---|---|---|---|
| `User` | `sys_user` | `system` | `username` / `real_name` / `email` / `mobile` / `dept_id` / `status` / `remark` / `role_ids` | 明文 / `NAME` / `EMAIL` / `MOBILE` / **标签化** / **枚举标签** / 明文 / **标签化** |
| `Role` | `sys_role` | `system` | `role_code` / `role_name` / `description` / `status` / `menu_ids` | 明文 / 明文 / 明文 / 枚举标签 / **集合 diff** |
| `UserRole` | `sys_user_role` | `system` | `role_ids` | **集合 diff**（`+技术主管 / -财务查看`） |
| `Dept` | `sys_dept` | `system` | `dept_name` / `parent_id` / `sort_order` / `leader` / `status` | 明文 / 标签化 / 明文 / `NAME` / 枚举标签 |
| `Announcement` | `sys_announcement` | `content` | `title` / `scope_type` / `status` / `publish_at` / `expire_at` / `is_top` / `attachments` | 明文 / 枚举标签 / 枚举标签 / 时间 / 时间 / 布尔 / **附件增减** |
| `HelpDoc` | `sys_help_doc` | `content` | `title` / `category_id` / `status` / `version` | 明文 / 标签化 / 枚举标签 / 明文 |

**不在白名单里的字段一律不记**，包括：`password`、`password_hash`、`login_fail_count`、`last_login_at`、`*_token`、`*_secret`、富文本正文（`content_html` / `content_md` 只记"正文已修改"这个事实，不记 500KB 正文——日志表不是文件柜）。**这一条要写成单元测试**：把整行实体序列化后跑一遍白名单，断言敏感字段不在输出里。

**"标签化"是什么意思**：值不是字符串而是 ID（`dept_id`、`category_id`、`parent_id`、`role_ids`），日志里要存**人读的名称**而不是 UUID：

| 类型 | 存法 | 展示 |
|---|---|---|
| 单值关联 | 存名称快照字符串 `"市场部"`；同时在 `extra.rel` 里存 `{"dept_id":"bbbb…"}` 供追溯 | 「市场部」 |
| 集合关联 | 存 `{"added":["技术主管"],"removed":["财务查看"]}` | 「+技术主管 -财务查看」 |
| 枚举 | 存码值 + 展示标签 `{"v":1,"t":"启用"}` | 「启用」（P0 也可存 `"启用"` 简化，**统一存标签**即可） |

> 为什么存名称快照而不是 UUID：日志的读者是人（合规/管理员/法务），UUID 对他们等于噪声。而"部门改名了"这件事不影响历史正确性——历史日志本来就该显示**当时的名字**。

### 5.4.2 计算步骤（算法，五步，顺序不能改）

```
输入：before_entity（明文）、after_entity（明文）、field_policy
① 白名单过滤：只保留 policy.fields 里的字段（其余丢弃，含 password 等）
② 规范化：UUID → 字符串；时间 → ISO8601；枚举 → 标签；集合 → 排序后的数组
③ 明文比较：对每个字段，按规范化后的值做 IS DISTINCT FROM
   changed_fields = [ 明文不同的字段 ]
④ 脱敏：对 changed_fields 的 before/after 值按 policy.mask_type 打码
   （before_data / after_data 只写 changed_fields 里的字段）
⑤ 生成摘要：change_summary = changed_fields 的中文名 join("、")，超过 10 个字段时写"手机号等 12 个字段"
```

**为什么 ③ 必须在 ④ 之前**：这是 D11 的核心。反例：手机号 `13812345678 → 13899995678`（改中间 4 位），脱敏后两边都是 `138****5678`——若先脱敏再比较，会得出"没变"，日志里这条变更**凭空消失**。

### 5.4.3 脱敏后相同但确实变了：必须显式告知

| 情形 | `changed_fields` | `before_data` / `after_data` | 详情页展示 |
|---|---|---|---|
| 普通变更 | 含 `mobile` | `138****1234` / `139****5678` | 「手机号：138\*\*\*\*1234 → 139\*\*\*\*5678」 |
| **脱敏后相同** | 含 `mobile` | `138****5678` / `138****5678` | 「手机号：138\*\*\*\*5678 → 138\*\*\*\*5678」+ 灰色小字「**已变更，脱敏后显示相同**」 |

判定规则（详情接口现算，不额外存列）：字段在 `changed_fields` 里 **且** `before_data->>字段 = after_data->>字段` → 命中第二种，加提示。

> 这条提示是**产品诚实的体现**：不给提示，管理员会以为"日志写错了"；给了提示，他知道"确实改了，只是打码掩盖了差异"。

### 5.4.4 三类特殊字段的处理

| 类型 | 处理 | 例子 |
|---|---|---|
| **集合（角色/菜单）** | 不做逐项前后对比，做**集合 diff**：`added` / `removed` | 「角色：+技术主管 / -财务查看」 |
| **附件** | 只记数量与名称变化，不记文件内容与 URL | 「附件：+值班表.xlsx / -旧版值班表.xlsx」 |
| **长文本（`remark`/`description`）** | 截断到 200 字，超出加 `…`，并在 `extra.truncated=true` | 「备注：（已截断）」 |

---

## 5.5 记什么、不记什么（边界必须写死）

### 5.5.1 记录的三个原则

1. **只记成功**：`result='SUCCESS'` 的记录 ⟺ 数据真的变了（5.3 保证）。失败另走 `record_failure`，且无快照。
2. **只记业务对象的"有意义字段"**：白名单（5.4.1）。技术字段（`updated_at`、`update_by`、`version`）**不记**（每次都变，记了等于噪声）。
3. **只记人做的**：系统任务（如"公告到期翻转"）**不记变更日志**（无操作人），把状态变化写进 `sys_job_log`（已有表）。

### 5.5.2 哪些模块进变更日志（本批 / 后续）

| 模块 | 本批 | 后续 |
|---|---|---|
| `system`（用户/角色/用户角色授权/部门） | ✅ 4 个对象类型 | — |
| `content`（公告/帮助文档） | ✅ 2 个对象类型 | — |
| `support`（字典/参数/存储配置/文件） | ❌ **不进**（配置类，有 `audit_logs` 足够） | 若合规要求，P1 追加 |
| ERP / BI / MDM（批次已上线/待上线） | ❌ 不在本批范围 | 按附录 E.2 的接入契约各自接入 |

> **为什么不给配置类也做变更日志**：配置变更用 `audit_logs` 看"谁改过参数"够用（配置是"可以随时看一眼当前值"的东西，不需要历史 diff）；而业务数据是"错了要追到哪一步"的，才需要字段级历史。**避免把日志表当成"什么都往里塞"的垃圾桶**。

### 5.5.3 动作归类（`action` 只有三个值）

| 业务动作 | `action` | `operation` |
|---|---|---|
| 新建用户 / 新建公告 / 新建角色 / 新建部门 | `CREATE` | `USER_CREATE` / `ANNOUNCEMENT_CREATE` … |
| 修改任何字段 / 分配角色 / 发布 / 撤回 / 置顶 / 上下架 / 改状态 | `UPDATE` | `USER_UPDATE` / `ANNOUNCEMENT_PUBLISH` / `HELPDOC_OFFLINE` … |
| 删除（软删） | `DELETE` | `USER_DELETE` … |

> 发布/撤回/置顶**不单独设 `action` 值**，而是用不同的 `operation` 码区分。理由：列表筛选"动作"只需三选一（新增/修改/删除），**管理员的心智就是这三种**；而"发布还是撤回"在操作码里能看到，且详情页会显示。若把 action 扩成 8 个值，筛选下拉会变成没人愿意看的长列表。

### 5.5.4 批量操作的粒度

| 场景 | 记录条数 | 说明 |
|---|---|---|
| 批量删除 5 个用户 | **5 条**（每对象 1 条） | 共享同一 `request_id` + `extra.batch_id` |
| 批量调整 20 个用户部门 | **20 条** | 同上 |
| 导入 5000 行 | **5000 条** | 每条对象 1 条（`extra.task_id` 关联导入任务）。**性能注意**：导入事务会变大，见 5.10 |
| 一次保存改了 3 个字段 | **1 条** | 字段级差异在 `changed_fields` 里，不是 3 条 |

---

## 5.6 脱敏（写入即脱敏，导出再脱敏一次）

### 5.6.1 规则

| 环节 | 做法 |
|---|---|
| **写入** | 按 5.4.1 的 `mask_type` 逐字段调 `Mask::mobile / email / name / id_card / bank_card`，**显式指定类型** |
| **禁止** | 使用 `Mask::mask_by_type()` 的 `_ =>` 兜底分支（F38：按字节切片，中文值 panic → 5xx）。**列入评审强制项**（附录 E.4） |
| **未知类型** | 一律按"不展示"处理：写 `"***"`，并在 `change_summary` 里标注该字段名。宁可打码过度，不可漏 |
| **短值** | 值长度不足（如 5 位"手机号"）→ 全部 `***`（沿用批次 B D8 第 ④ 条：短值原样返回是泄露点） |
| **明文查看（P1）** | 需 `business:changelog:export-plain` **且** `system:field-mask:view-plain`（双闸门，沿用批次 B D10）；每次明文查看写一条 `operation_logs` 之外的**安全事件** `security_logs(event_type='PLAIN_VIEW')` |
| **导出** | 导出用的行数据 = 列表接口返回的同一份（已脱敏）。**不存在"导出专用明文路径"**，P1 的明文导出也必须过双闸门 |

### 5.6.2 脱敏示例（验收按这张表对）

| 字段 | 明文 | 存进日志的值 |
|---|---|---|
| `mobile` | `13812345678` | `138****5678` |
| `email` | `zhangwei@example.com` | `z***@example.com` |
| `real_name` | `张伟` | `张*` |
| `id_card` | `110101199001011234` | `1101**********1234` |
| `bank_card` | `6222021234567890` | `6222 **** 7890` ⚠️ 注意带空格（框架 `Mask::bank_card` 实测输出格式，F38） |
| `remark`（长文本） | `这是一段备注…` | 原样（备注不是敏感字段，但截断 200 字） |
| 未知/未配置 | 任意 | `***` |

> `bank_card` 的输出 `6222 **** 7890`（带空格）与框架文档注释里的 `6222****7890` **不一致**，以实测为准（F38 同源问题）。前端展示层不要自己再格式化一次。

### 5.6.3 为什么不加密存原文

有同事会问："存明文 + 加密列不就行了？" 三个理由否掉：

1. **日志表不需要真值**：变更日志的用途是"知道改了什么"，打码后仍能判断"手机号从 138 段变成 139 段"，足够。
2. **多一份明文 = 多一个泄露面**：`sys_user` 已经有一份明文（受批次 B 列脱敏保护），日志表再来一份，等于把保护面积翻倍。
3. **合规上更安全**：审计日志里出现大量明文 PII，本身就是很多内审条款里的"风险项"。

---

## 5.7 查询与导出

### 5.7.1 列表

| 项 | 定法 |
|---|---|
| 列 | 时间 / 操作人 / 模块 / 动作 / 对象 / 变更内容（`change_summary`）/ IP / 结果 |
| 默认筛选 | 最近 **7 天**（不默认全量，避免一进来就扫 12 个月分区） |
| 筛选条件 | 时间范围（默认 7 天，单次 ≤366 天）、操作人（模糊：账号或姓名）、模块（下拉）、动作（三选一）、对象类型（下拉）、对象 ID（精确）、结果（成功/失败/全部） |
| 排序 | `created_at DESC`（固定，不提供自定义排序——日志按时间看是唯一有意义的顺序） |
| 分页 | 默认 20 条，上限 100（日志行很宽，100 条已经够看） |
| 总数 | 只返回 `page ≤ 10` 时的精确总数；更深分页返回 `total: null` + 「已超过 200 条，请缩小时间范围」（避免深分页 count 扫全表，见 5.10） |

### 5.7.2 详情（抽屉，不跳页）

```
┌────────────────────────────────────────────────────────────┐
│ 变更详情  · 2026-10-22 14:31:08                            │
├────────────────────────────────────────────────────────────┤
│ 操作人：王芳（admin）· 系统管理 · 用户管理                  │
│ 对  象：用户 zhangwei（a3f2…9c1）           [查看该对象历史]│
│ 动  作：修改        结果：✅ 成功       IP：10.20.3.88      │
│ 请求链路：req-7f3a…（点击可看同一请求的全部日志）           │
├────────────────────────────────────────────────────────────┤
│ 字段变更（共 2 项，只显示变更）        [ ] 显示未变更字段   │
│ ┌──────────┬────────────────┬────────────────┐             │
│ │ 字段名   │ 旧值           │ 新值           │             │
│ ├──────────┼────────────────┼────────────────┤             │
│ │ 手机号   │ 138****1234    │ 139****5678    │             │
│ │ 所属部门 │ 市场部         │ 市场部         │ ← 灰色"未变"│
│ └──────────┴────────────────┴────────────────┘             │
│ 备注：（本批不含变更备注）                                  │
└────────────────────────────────────────────────────────────┘
```

**关键交互**
- 默认**只显示变更字段**；勾「显示未变更字段」才显示灰行（用于"我确定他只改了手机号"这种确认场景，未变更字段值来自白名单全量对比——**注意**：未变更字段的值**不在库里**（5.4.2 只存变更字段），所以"显示未变更"实际是**空值占位**，界面上这行只能显示字段名 + 「未变更」。**这是设计取舍：不为了这个勾选框把全量字段都存进去（PII 最小化优先）**。）
- 「查看该对象历史」→ 带 `target_type + target_id` 跳到列表（筛选自动填好）。
- 「请求链路」→ 带 `request_id` 跳列表（看"同一批操作"）。

### 5.7.3 导出（Excel，必须脱敏）

| 项 | 定法 |
|---|---|
| 触发 | 列表页「导出」，用**当前筛选条件**（不允许"导出全部"，必须先筛） |
| 同步/异步 | 命中 ≤1000 行 → 同步下载（xlsx）；>1000 行 → 走批次 A 的 `sys_impexp_task` 异步导出，完成后站内信通知 |
| 列 | 时间 / 操作人账号 / 操作人姓名 / 模块 / 动作 / 对象类型 / 对象名称 / 对象 ID / 变更内容 / IP / 结果 / 请求链路 |
| 脱敏 | **与列表同一份数据**（已脱敏）；导出文件里**不含** `before_data/after_data` 的原始 JSON（只给 `change_summary` 的中文摘要） |
| 文件命名 | `变更日志_{yyyyMMddHHmmss}.xlsx` |
| 权限 | `business:changelog:export`；**无此码时按钮不显示**（`v-privilege`） |
| 审计 | 每次导出写一条 `audit_logs`（`operation='CHANGELOG_EXPORT'`，`detail={filters, rows, request_id}`）——**导出行为本身要被审计**（谁把台账拿走了） |
| 有水印吗 | P1：页脚加"导出人 + 导出时间"（沿用批次 A 的 I4-06） |

### 5.7.4 按对象查历史的入口（三处）

| 入口 | 位置 | 跳转 |
|---|---|---|
| 用户详情页 | 右上角「变更历史」按钮（`v-privilege="['business:changelog:list']"`） | `/platform/changelog?target_type=User&target_id={id}` |
| 角色详情页 | 同上 | `?target_type=Role&target_id={id}` |
| 部门详情页 | 同上 | `?target_type=Dept&target_id={id}` |

---

## 5.8 保留与归档（D12）

### 5.8.1 时间轴

```
写入 ──▶ 在线分区（12 个月，可查可导）
        │
        │ 每月 1 日 04:00：为"下下个月"预建分区
        │ 每日 04:00：把"created_at 早于 now()-12 个月"的整月分区 DETACH → ATTACH 到归档表
        ▼
      归档表（5 年，只读，需显式勾选"含归档"才查）
        │
        │ 每日 04:30：把"早于 now()-5 年"的归档分区导出 xlsx 到对象存储（批次 A 的 storage），
        │             成功后 DROP PARTITION（不是 DELETE）
        ▼
      对象存储（冷数据，按需申请）
```

### 5.8.2 归档操作为什么是 `DETACH → ATTACH`（实测 F31）

| 方案 | 耗时（100 万行/月分区） | 锁 | 结论 |
|---|---|---|---|
| `INSERT INTO archive SELECT …; DELETE FROM online …` | 分钟级（两次全扫，WAL 巨大） | 长期持有行锁，可能阻塞写入 | ❌ |
| `DETACH PARTITION` + `ATTACH PARTITION`（同构归档表） | **秒级**（只改元数据，不碰数据） | 短暂 `ACCESS EXCLUSIVE`（毫秒级） | ✅ **采用** |
| `DETACH` 后单独留存、不合并 | 秒级 | 同左 | 可用但查询要 UNION 多表，P1 复杂化 |

**实测注意**（开发者必读）
1. 归档表**必须是同样按月 RANGE 分区的表**（用 `CREATE TABLE … (LIKE …) PARTITION BY RANGE (created_at)`），否则 `ATTACH` 报 `cannot be performed on relation`；
2. `DETACH` 出来的分区**已继承主键约束**，**不要**再 `ADD PRIMARY KEY`（会报 `multiple primary keys`，实测踩过）；
3. `ATTACH` 时 `FOR VALUES` 区间必须与被 DETACH 的分区**完全一致**，否则报 `partition constraint is violated`；
4. 归档任务必须**幂等**：先查 `pg_class` 判断该分区是否已存在/是否已 `ATTACH`，重复执行不报错。

### 5.8.3 定时任务（2 个，与公告的 3 个错峰）

| 任务名（`invoke_target`） | cron（五段） | 做什么 |
|---|---|---|
| `changelog_maintain` | `0 4 * * *` | ①为"下下个月"预建分区；②把早于 `changelog.retention.months` 的整月分区 `DETACH` → `ATTACH` 到归档表 |
| `changelog_purge` | `30 4 * * *` | 把早于 `changelog.archive.years` 的归档分区**先导出 xlsx 到对象存储**，成功后 `DROP PARTITION`；导出失败则跳过并告警（**绝不先删后导**） |

> 两个任务都**幂等**（先查 `pg_class` / 元数据再动手）；失败不影响业务写入（`default` 分区兜底，5.11 第 9 条）。

### 5.8.4 保留期配置（`sys_config`，`category='内容管理'`）

| 键 | 默认 | 说明 |
|---|---|---|
| `changelog.retention.months` | `12` | 在线保留月数（1–36） |
| `changelog.archive.years` | `5` | 归档保留年数（≥ 在线保留） |
| `changelog.page.default_days` | `7` | 列表默认时间范围 |
| `changelog.export.sync_limit` | `1000` | 同步导出上限，超过转异步 |

> **改保留期不会立即删数据**：只影响**归档任务**下次运行时的判断。把 12 改成 6，下次 04:00 任务就会把 6–12 个月的分区搬进归档（数据不丢，只是从"在线"变"归档"）。真正的销毁只发生在归档满 5 年时，且要**先成功导出到对象存储**才 `DROP`。

---

## 5.9 安全红线（5 条，评审逐条打勾）

| # | 红线 | 违反后果 | 怎么验 |
|---|---|---|---|
| 1 | **日志写入必须在业务事务内**（5.3） | 回滚留假日志 → 审计被引向不存在的变更 | 9.6 的验收场景 + 代码 grep |
| 2 | **写入即脱敏，禁止明文落库** | 日志表成为 PII 明文副本，且导出必泄露 | 查库里 `before_data` 是否出现 11 位连续数字：`SELECT count(*) FROM sys_change_log WHERE after_data::text ~ '\d{11}'` 必须为 **0** |
| 3 | **密码/密钥/令牌永不入日志**（5.5.2） | 凭据泄露 | 白名单单测 + 查库 `after_data::text ILIKE '%password%'` 必须为 0 |
| 4 | **导出只能走脱敏数据**，不存在明文导出接口 | 一次导出泄露全公司 PII | 接口清单里**只有** `/export`（脱敏）与 P1 的 `/export-plain`（双闸门）；grep 不得出现第三个导出接口 |
| 5 | **日志表不提供删除接口**（连超管也没有） | 有人删掉自己的痕迹 | 接口清单里无 `DELETE /business/changelog/*`；过期只走"归档 + 到期销毁"（有审批与导出前置） |

---

## 5.10 性能

| 场景 | 规模 | 目标 | 说明 |
|---|---|---|---|
| 单次写入（含 diff + 脱敏） | 1 行 | **< 1ms** 额外耗时 | diff 在内存算（10 个字段的 JSON 比较），只有一次 INSERT |
| 列表查询（带 target 筛选） | 在线 12 个月 ≈ 300 万行 | P95 **< 80ms** | 走 `idx_cl_target`；分区裁剪把范围收窄到 1–2 个月 |
| 列表查询（仅时间范围） | 同上 | P95 **< 120ms** | 走 `idx_cl_created` + 分区裁剪 |
| 详情（含 diff 计算） | — | **< 15ms** | 单行读 + 内存组装，不需要额外查询（字段中文名来自字典缓存） |
| 同步导出 1000 行 | — | **< 3s** | 一次查询 + 内存写 xlsx |
| 异步导出 10 万行 | — | **< 90s** | 分批查询（5000/批）+ 流式写（复用批次 A 的导出通道） |
| 归档任务（搬 1 个月分区） | 100 万行 | **< 10s** | DETACH + ATTACH 是元数据操作（F31） |
| 导入 5000 行对象 | — | 事务追加 **< 3s** | 变更日志 5000 行批量 INSERT（一次多值 INSERT，不是 5000 次单插入）；**若导入本身已接近超时，允许把变更日志改为"随导入任务分片提交"**（每 1000 行一批事务） |

**容量估算**：312 人的公司，每天约 200 次业务变更 → 年 7.3 万行 → 5 年 36 万行，约 4GB（含索引）。5000 人规模年 120 万行 → 5 年 600 万行，约 60GB —— **必须分区 + 归档**，这也是 D12 的理由。

---

## 5.11 边界清单

| # | 边界 | 行为 |
|---|---|---|
| 1 | 对象被删后查历史 | 正常显示（`target_id` + `target_name` 快照，不 JOIN 业务表） |
| 2 | 操作人被删 | 正常显示（`operator_name` 快照） |
| 3 | 一次请求改了 3 个对象 | 3 条记录，同 `request_id`，列表显示为 3 行（不做折叠，避免"点开一层还有一层"） |
| 4 | 同一字段被改两次（一次请求内） | 只记 1 条（before=最初，after=最终）——中间态没有审计价值 |
| 5 | 对象无变化仍提交（用户点了保存但没改） | **不记**（`changed_fields` 为空 → 直接 return，不写库）。这条能省掉大量"改了又改回来"的噪声 |
| 6 | 超长 `remark`（10000 字） | 截断 200 字 + `extra.truncated=true` |
| 7 | 时区 | 全部存 UTC（TIMESTAMPTZ），展示按 `Asia/Shanghai` |
| 8 | 跨月边界的写入 | 分区由 `created_at` 自动路由；`default` 分区兜底（若预建分区缺失，写入不报错，但归档任务会告警） |
| 9 | 预建分区失败 | 写入落在 `default` 分区；每日任务补建 + 写 `security_logs` 告警（不阻断业务） |
| 10 | 查 400 天范围 | 拒绝：「单次查询时间范围不能超过 366 天」 |
| 11 | 导出 0 行 | 拒绝：「当前筛选条件没有数据，无需导出」 |
| 12 | 归档表查询 | 默认不查；勾"含归档"才 UNION 查询，且**强制要求时间范围 ≤31 天**（归档表大） |
| 13 | 明文查看（P1）无权限 | 按钮不显示（不是点了报错——避免诱导尝试） |
| 14 | 结果列筛选"失败" | 只出 `record_failure` 的记录（`after_data` 必为 NULL） |
| 15 | 日志表被 DBA 直接 UPDATE | 不阻止（数据库层不设触发器），但每日任务巡检 `updated_at`（表**不设** `updated_at` 列——**没有任何列记录"最后修改"**，即从结构上让"改了也无法掩盖痕迹"变成"改了会丢字段"）。**登记为已知限制**，见附录 D |

---

# 第 6 章 权限模型

## 6.0 三条设计前提

1. **权限码格式固定** `{模块}:{资源}:{动作}`，存 `sys_menu.auth_code`（F1）。本批全部用 `business` 段（与批次 A 的 `business:storage|impexp|notice` 一致），**不新增 module_code 值**（`sys_menu.module_code` 仍是 `business`，F5）。
2. **`hasPermission()` 是全等匹配**（F2）：所以**每一个要判断的动作都必须有自己独立的一行 `auth_code`**。以下写法**全部无效**（永远 false，按钮永远不显示）：
   - `business:announcement:*`（前端不认前缀通配）
   - `business:help:*`
   - `business:changelog:*`
3. **权限点必须落成 `sys_menu` 种子数据**（F1）：`menu_type=3`（按钮）、`is_visible=FALSE`、`status=1`。前端导航不渲染按钮行（F3），但角色分配页与 `v-privilege` 都靠它们。

---

## 6.1 权限码清单（21 个）

### 6.1.1 公告管理 `business:announcement:*`（10 个）

| # | 权限码 | 名称 | menu_type | 敏感 | 说明 |
|---|---|---|---|---|---|
| 1 | `business:announcement:list` | 查看公告管理 | 2（菜单） | — | 管理端列表页**必须**有这个码才能进；用户端不受影响 |
| 2 | `business:announcement:create` | 新增公告 | 3 | — | 含保存草稿 |
| 3 | `business:announcement:update` | 编辑公告 | 3 | ✅ | 含编辑已发布公告（有二次确认） |
| 4 | `business:announcement:delete` | 删除公告 | 3 | ✅ | 仅草稿/已撤回/已过期可删 |
| 5 | `business:announcement:publish` | 发布公告（受限范围） | 3 | ✅ | 发布到"角色/本部门及下级"；**不含全员**（见 #6） |
| 6 | `business:announcement:publish-all` | **发布全员公告** | 3 | ✅✅ | **单独拆码**：这是"能打扰全公司 312 人"的权力。有它才能选「全员」或选到本部门以外的目标 |
| 7 | `business:announcement:revoke` | 撤回公告 | 3 | ✅ | 撤回后所有人不可见 |
| 8 | `business:announcement:top` | 置顶 / 取消置顶 | 3 | ✅ | 能编辑不等于能霸占首页第一位 |
| 9 | `business:announcement:read-stat` | 查看阅读明细 | 3 | ✅ | P1；不看明细时列表仍显示聚合的"已读 X/Y" |
| 10 | `business:announcement:view` | 阅读公告（用户端） | 2（菜单，**隐藏**） | — | **所有登录用户默认给**；菜单 `is_visible=FALSE`，从顶栏图标进入 |

### 6.1.2 帮助文档 `business:help:*`（7 个）

| # | 权限码 | 名称 | menu_type | 敏感 | 说明 |
|---|---|---|---|---|---|
| 11 | `business:help:doc:list` | 查看帮助文档管理 | 2（菜单） | — | 管理端列表页 |
| 12 | `business:help:doc:create` | 新增文档 | 3 | — | 草稿 |
| 13 | `business:help:doc:update` | **编辑文档** | 3 | ✅ | 任务书要求的"编辑帮助单独授权"；含改标题/正文/分类/排序 |
| 14 | `business:help:doc:delete` | 删除文档 | 3 | ✅ | 软删 |
| 15 | `business:help:doc:publish` | 发布 / 下架文档 | 3 | ✅ | 与编辑分开：能写不等于能对外发 |
| 16 | `business:help:category:manage` | 维护分类树 | 3 | ✅ | 增删改分类 + 拖拽排序（分类影响全站导航，单独授权） |
| 17 | `business:help:view` | 阅读帮助中心（用户端） | 2（菜单，**隐藏**） | — | **所有登录用户默认给**；`/help` |

### 6.1.3 变更日志 `business:changelog:*`（4 个）

| # | 权限码 | 名称 | menu_type | 敏感 | 说明 |
|---|---|---|---|---|---|
| 18 | `business:changelog:list` | 查看变更日志 | 2（菜单） | ✅ | 能看到全公司所有模块的变更（D14：不做行级过滤，所以这个码本身要有分量） |
| 19 | `business:changelog:detail` | 查看变更详情（diff） | 3 | ✅ | 与列表分开：某些岗位"能看有谁改过"，但不该看到具体值 |
| 20 | `business:changelog:export` | **导出变更日志** | 3 | ✅ | 导出的是**脱敏**数据；每次导出写审计 |
| 21 | `business:changelog:export-plain` | 导出/查看明文敏感字段 | 3 | ✅✅ | **P1**；还必须同时持有批次 B 的 `system:field-mask:view-plain`（双闸门） |

### 6.1.4 统计

| 维度 | 数量 |
|---|---|
| 权限码总数 | **21** |
| 其中 `menu_type=2`（菜单级） | **5**（3 个可见：公告管理/帮助文档/变更日志；2 个隐藏：公告中心/帮助中心） |
| 其中 `menu_type=3`（按钮级） | **16** |
| 标记敏感 ✅ | **15** |
| 其中极高敏 ✅✅ | **2**（`publish-all`、`export-plain`） |
| P1 权限码（先建码不开放） | **2**（`read-stat`、`export-plain`） |

> P1 的码**也要先建**（`status=1` 但页面不显示对应按钮）：等 P1 上线时只需加按钮，不必再改种子数据、不用重新分配角色。

---

## 6.2 敏感操作单独授权（任务书点名的三组）

| 敏感操作 | 为什么必须单独授权 | 若与谁合并会出事 |
|---|---|---|
| **发布全员公告** `business:announcement:publish-all` | 一次点击惊动全公司；发错无法"收回注意力"（撤回也只能减少后续误读） | 若与 `publish` 合并 → 部门主管能发全员公告（故事 3 的边界直接失效） |
| **编辑帮助文档** `business:help:doc:update` | 帮助文档是"官方口径"，被改一个字就可能误导全公司操作（如把审批时限从 3 天改成 1 天） | 若与 `list` 合并 → 任何能看的人都能改；若与 `publish` 合并 → 改完立即对外，没有复核机会 |
| **导出变更日志** `business:changelog:export` | 一次导出 = 带走一份全公司变更台账（含打码后的手机号、部门、操作时间） | 若与 `list` 合并 → 能看的人就能批量拿走；台账外流后无法追查 |

**其余单独拆出的码及理由**

| 码 | 拆出的理由 |
|---|---|
| `business:announcement:revoke` | 撤回是"销毁已发布内容"的动作，应独立于"发布"（防止"发错的被人一怒之下撤回"） |
| `business:announcement:top` | 置顶=占用首页最显眼位置，应独立于"编辑" |
| `business:help:doc:publish` | 写的和发的是两个人（四眼原则的雏形，完整审核流在批次 D） |
| `business:help:category:manage` | 分类树是全站导航结构，一个人乱建分类会让全公司找不到文档 |
| `business:changelog:detail` | "知道谁改过"与"知道改成什么"是两个敏感度（前者是监督，后者是数据） |

---

## 6.3 数据权限：三个模块的结论（任务书要求给建议）

> **前提**：`sys_role` 没有 `data_scope` 列（F7），数据权限只能服务层硬实现。

### 6.3.1 公告：**不受数据范围限制，但发布目标受部门约束**

| 问题 | 结论 | 理由 |
|---|---|---|
| 管理端列表是否按 `created_by` 隔离？ | **不隔离**。有 `list` 码即可看全部公告 | 公告是"组织的公告"，不是"我的草稿箱"。按创建人隔离会导致：HR 离职后他发的公告没人能改（必须换账号或改库）。草稿的私密性由 `status=DRAFT` 保证（DRAFT 只在管理端可见，用户端看不到） |
| 编辑是否按 `created_by` 限制？ | **不限制**（D13） | 同上 |
| 发布范围是否受部门约束？ | **受**。不持有 `publish-all` 的人，所选目标部门必须**全部落在自己部门的子树内**（用 F8 的 `find_dept_subtree_ids` 校验），否则拒绝 | 这是本模块唯一的"数据权限"，也是最关键的一条：防止"任何有发布权的人都能给全公司发" |
| 用户端可见性 | 见 3.4（受众谓词） | 这才是用户侧的"数据权限" |

**后端校验伪码**（必须做，不能只靠前端下拉过滤）

```
if !has("business:announcement:publish-all") {
    let my_subtree = find_dept_subtree_ids(当前用户.dept_id);
    for t in 目标部门 {
        if !my_subtree.contains(t) → 403「你没有向该部门发布公告的权限」
    }
    if 目标是角色 || 目标是全员 → 403「你没有『发布全员公告』权限，请选择本部门范围」
    // 角色目标一律要求 publish-all：角色可能跨部门（"所有主管"横跨全公司）
}
```

### 6.3.2 帮助文档：**无数据权限**

所有登录用户可读全部已发布文档（D8）；编辑权由权限码控制，不按分类/创建人隔离。理由：帮助中心是"公司说明书"，按部门隔离等于"市场部看不到人事流程"——恰好相反，跨部门流程正是最需要被看到的内容。

### 6.3.3 变更日志：**不做行级数据范围（D14）**

| 问题 | 结论 | 理由 |
|---|---|---|
| 普通管理员能看别人的操作吗？ | **能**（只要有 `list` 码） | ①日志的价值是横向监督；②按部门过滤会出现"市场部管理员改了自己部门的敏感数据，却只有市场部能查到"——这等于让被审计者保管审计记录；③实操上"谁改了你的数据"是最高频的问题，过滤会让答案缺失 |
| 那怎么防止滥用？ | 三道限制：①`list` 码本身只给管理岗；②详情 `detail` 码再拆一层（能看"谁改过"≠能看"改成什么"）；③导出单独 `export` 码 + 每次写审计 + 明文导出双闸门 | 权限收紧（谁能看）比数据过滤（能看哪些行）更适合日志场景：过滤会破坏完整性，收紧不会 |
| 超管自己的操作要不要隐藏？ | **不隐藏**（连超管改数据也留痕） | 若隐藏，"谁改的"这一栏会出现空白，反而暴露"这里有超管操作" |

---

## 6.4 预置角色包（上线时建议直接建好，减少配置工作量）

| 角色 | `role_code` | 分配的权限码 | 说明 |
|---|---|---|---|
| **行政 / HR** | `content_hr` | 公告 1/2/3/5/6/7/8/9/10 + 帮助 11~17 | 可发全员、可置顶、可撤回、可维护帮助文档 |
| **部门主管** | `content_dept_manager` | 公告 1/2/3/5/7/10 | **无 `publish-all`、无 `top`、无 `delete`** |
| **系统管理员** | 复用已有管理角色 | 全部 21 个（或走 `*:*:*` 超管） | — |
| **审计 / 合规** | `content_auditor` | 公告 1/10 + 帮助 11/17 + 变更日志 18/19/20 | **无任何写权限**；只读 + 导出 |
| **普通员工** | 复用已有普通角色 | 公告 10 + 帮助 17 | 只有两个"阅读"码（**登录即有**） |

> **普通员工的两个码必须默认给所有角色**（种子里通过 `sys_role_menu` 给到内置普通角色；新建角色时前端"分配菜单"面板把这两项默认勾上）。否则新员工登录后顶栏图标点不开、帮助中心进不去。

---

## 6.5 审计动作码与字典扩充

变更日志与审计日志都要"看得懂"，所以三个字典要扩（`sys_dict_item`，F 已有的字典机制）：

### 6.5.1 `log_target_type`（日志目标类型）新增 5 项

| 值 | 标签 | sort_order |
|---|---|---|
| `Announcement` | 公告 | 400 |
| `HelpDoc` | 帮助文档 | 410 |
| `HelpCategory` | 帮助分类 | 420 |
| `UserRole` | 用户角色授权 | 430 |
| `RoleMenu` | 角色菜单授权 | 440 |

> 已有 `User` / `Role` / `Dept` / `File` 等沿用，不重复添加。`sort_order` 从 400 起（现有最大 370 附近，见 `S__dict_data.sql` 的 `log_target_type` 段）。

### 6.5.2 `log_operation`（日志操作类型）新增 14 项

| 值 | 标签 | sort_order |
|---|---|---|
| `ANNOUNCEMENT_CREATE` | 新增公告 | 400 |
| `ANNOUNCEMENT_UPDATE` | 修改公告 | 410 |
| `ANNOUNCEMENT_PUBLISH` | 发布公告 | 420 |
| `ANNOUNCEMENT_REVOKE` | 撤回公告 | 430 |
| `ANNOUNCEMENT_TOP` | 置顶公告 | 440 |
| `ANNOUNCEMENT_DELETE` | 删除公告 | 450 |
| `HELPDOC_CREATE` | 新增帮助文档 | 460 |
| `HELPDOC_UPDATE` | 修改帮助文档 | 470 |
| `HELPDOC_PUBLISH` | 发布帮助文档 | 480 |
| `HELPDOC_OFFLINE` | 下架帮助文档 | 490 |
| `HELPDOC_DELETE` | 删除帮助文档 | 500 |
| `HELPCAT_UPDATE` | 维护帮助分类 | 510 |
| `CHANGELOG_EXPORT` | 导出变更日志 | 520 |
| `CHANGELOG_PLAIN_VIEW` | 查看变更日志明文 | 530 |

### 6.5.3 新建字典类型 `log_field_label`（字段中文名）

| 值 | 标签 |
|---|---|
| `username` | 账号 |
| `real_name` | 姓名 |
| `mobile` | 手机号 |
| `email` | 邮箱 |
| `dept_id` | 所属部门 |
| `status` | 状态 |
| `remark` | 备注 |
| `role_ids` | 角色 |
| `menu_ids` | 菜单权限 |
| `role_code` / `role_name` / `description` | 角色编码 / 角色名称 / 角色描述 |
| `dept_name` / `parent_id` / `leader` / `sort_order` | 部门名称 / 上级部门 / 负责人 / 排序 |
| `title` | 标题 |
| `scope_type` | 发布范围 |
| `publish_at` / `expire_at` | 生效时间 / 失效时间 |
| `is_top` | 置顶 |
| `attachments` | 附件 |
| `category_id` | 所属分类 |
| `version` | 版本号 |

**为什么要字典而不是硬编码**：字段中文名会被产品改（"手机号"→"手机号码"），字典改一行就生效，硬编码要发版。且字典有 `sort_order`，可以让详情页的字段顺序稳定（不随 JSON key 排序乱跳）。

---

# 第 7 章 菜单结构

## 7.1 决策：不新建一级目录，挂在 `/platform` 下（D15）

**现状**：`/platform`（通用底座，sort 70）是批次 A 建的，下面已有 存储管理(10)/文件管理(20)/导入导出(30)/通知管理(40)/消息中心(50,隐藏)。

**本批新增**：3 个可见菜单（sort 60/70/80）+ 2 个隐藏菜单（用户端两个页面）。

**为什么不开新目录**：一级目录 sort 只剩 30（预留给协同办公，F4），为三个菜单占掉一个预留位不划算；且这三者都是平台级能力、权限码同属 `business`，与 `/platform` 内容天然同类。

## 7.2 菜单树（种子数据规格，供开发直接落 `sys_menu`）

```
通用底座                                      /platform          目录  sort 70  icon=system-storage  （批次 A 已有，本批不改）
├─ 存储管理 …（批次 A，sort 10）
├─ 文件管理 …（批次 A，sort 20）
├─ 导入导出 …（批次 A，sort 30）
├─ 通知管理 …（批次 A，sort 40）
├─ 消息中心 …（批次 A，sort 50，隐藏）
│
├─ 公告管理                                  /platform/announcement   菜单  sort 60  icon=article
│   auth_code = business:announcement:list
│   component = business/announcement/index
│   ├─ [按钮] 新增公告          business:announcement:create
│   ├─ [按钮] 编辑公告          business:announcement:update
│   ├─ [按钮] 删除公告          business:announcement:delete
│   ├─ [按钮] 发布公告          business:announcement:publish
│   ├─ [按钮] 发布全员公告      business:announcement:publish-all
│   ├─ [按钮] 撤回公告          business:announcement:revoke
│   ├─ [按钮] 置顶/取消置顶     business:announcement:top
│   └─ [按钮] 查看阅读明细      business:announcement:read-stat      （P1）
│
├─ 帮助文档                                  /platform/help        菜单  sort 70  icon=help-circle
│   auth_code = business:help:doc:list
│   component = business/help/manage/index
│   ├─ [按钮] 新增文档          business:help:doc:create
│   ├─ [按钮] 编辑文档          business:help:doc:update
│   ├─ [按钮] 删除文档          business:help:doc:delete
│   ├─ [按钮] 发布/下架         business:help:doc:publish
│   └─ [按钮] 维护分类          business:help:category:manage
│
└─ 变更日志                                  /platform/changelog   菜单  sort 80  icon=history
    auth_code = business:changelog:list
    component = business/changelog/index
    ├─ [按钮] 查看变更详情      business:changelog:detail
    ├─ [按钮] 导出变更日志      business:changelog:export
    └─ [按钮] 查看/导出明文     business:changelog:export-plain      （P1）

—— 以下两个是"用户端"页面：不挂任何目录，菜单行存在但 is_visible=FALSE ——

公告中心          /notice           菜单  is_visible=FALSE  icon=pin         auth_code = business:announcement:view
                  component = business/announcement/center
帮助中心          /help             菜单  is_visible=FALSE  icon=help-circle auth_code = business:help:view
                  component = business/help/center/index
```

**另有 3 个非菜单路由**（不进 `sys_menu`，只在前端路由表里，`meta.permission` 控制）：

| 路由 | component | `meta.permission` |
|---|---|---|
| `/notice/detail/:id` | `business/announcement/detail` | `business:announcement:view` |
| `/platform/announcement/edit/:id?` | `business/announcement/edit` | `business:announcement:update`（新建时判断 `create`，由页面内 `v-privilege` 兜底） |
| `/platform/help/edit/:id?` | `business/help/manage/editor` | `business:help:doc:update` |
| `/help/doc/:id` | `business/help/center/doc` | `business:help:view` |

## 7.3 五个必须注意的落地细节

| # | 细节 | 说明 |
|---|---|---|
| 1 | **`module_code` 全部填 `business`**（含按钮行） | F5：`VARCHAR(20)`，`business`（8 字符）合法。菜单管理页该字段是自由输入（F6），不需要改前端下拉 |
| 2 | **`component` 路径中的页面文件必须真实存在** | 8 个组件路径：`business/announcement/index`、`/edit`、`/center`、`/detail`、`business/help/manage/index`、`/editor`、`business/help/center/index`、`/doc`、`business/changelog/index`。缺一个就是白屏 |
| 3 | **隐藏菜单也必须 `auth_code` 非空** | 否则 `filterNavMenus` 里 `hasPermission('')` 返回 true（F2：空码视为放行），等于"任何人都能访问公告中心路由"——虽然数据只看得到自己的，但应按最小权限给码 |
| 4 | **`icon` 只能从 F26 验证过的名单里选** | `article` / `help-circle` / `history` / `pin` 均实测存在；**不要写 `announcement` / `bulletin` / `log`**（不存在，菜单会显示为空白方块） |
| 5 | **`sort_order` 从 60 起，不与批次 A 的 10–50 冲突** | 种子数据里父节点用固定 UUID 引用（照 `S__menu_data.sql` 的写法），不要用子查询 |

## 7.4 顶栏入口（不在侧边栏，但是用户最高频的入口）

| 项 | 定法 |
|---|---|
| 公告入口 | `article` 图标 + `t-badge` 未读数，插在**主题切换按钮右边、消息铃铛（批次 A）左边**（F29 的插入点；实际 DOM 顺序：主题 → 公告 → 消息 → 用户下拉） |
| 帮助入口 | **P0 不放顶栏**（放侧边栏会占位，放顶栏又太挤）。用户从「公告详情页底部」、「用户下拉菜单 → 帮助中心」两处进入。P1 若数据证明需要，再在顶栏加 `help-circle` |
| 面包屑 | 用户端页面在面包屑显示「公告中心 / 标题」「帮助中心 / 分类 / 标题」 |
| 移动端（<992px） | 顶栏只保留公告 + 消息两个图标，用户下拉收进"更多" |

---

# 第 8 章 页面与交互

## 8.0 全局约定（先定这 12 条，后面 7 个页面不再重复）

| # | 约定 | 具体做法 |
|---|---|---|
| 1 | **布局基线** | 沿用底座范式：`t-card :bordered="false"` 筛选卡 + 表格卡；页面外边距 16px；卡片间距 16px |
| 2 | **配色只用变量** | 品牌色 `var(--td-brand-color-7)`（或 `-hover`/`-active`）；成功 `var(--td-success-color)`、警告 `var(--td-warning-color)`、错误 `var(--td-error-color)`、正文 `var(--td-text-color-primary)`、次要 `var(--td-text-color-secondary)`、边框 `var(--td-component-border)`、页面底 `var(--td-bg-color-page)`。**禁止硬编码色值**（切配色时会变成"一套配色里混着另一套的蓝"） |
| 3 | **状态色语义固定** | 草稿/默认 → `default`（灰）；已发布/成功 → `success`；待发布/进行中 → `brand`（主色）；已撤回/已过期/失败 → `error`；警告类 → `warning`。**同一个状态在全站同一个颜色** |
| 4 | **按钮权限** | 一律用 `v-privilege="['code']"`（F27），不写 `v-if="hasPermission(...)"` |
| 5 | **空态** | 一律"插画 + 一句话 + 一个动作按钮"，禁止只写"暂无数据"。三种空态必须区分：**没数据 / 没搜到 / 没权限**（文案见各页） |
| 6 | **加载态** | 表格用 `:loading`；提交按钮用 `:loading` 且**禁止重复点击**（`disabled` 直到响应回来） |
| 7 | **删除/危险操作** | 一律 `DialogPlugin.confirm` 二次确认；文案里必须写清**影响范围**（"将删除 5 条""已有 287 人读过"） |
| 8 | **时间展示** | 列表：`YYYY-MM-DD HH:mm`（`dayjs` 格式化）；详情：同上 + 悬浮显示相对时间（"3 天前"） |
| 9 | **长文本** | 表格列 `ellipsis` + `t-tooltip` 显示全文；**禁止**让一列把表格撑破 |
| 10 | **错误提示** | 统一用 `MessagePlugin.error(文案)`（顶部轻提示）；表单校验用 `t-form` 的 `rules`（字段下方红字）。**文案里禁止出现错误码**（如"错误 5000x"），用户看不懂 |
| 11 | **危险操作的取消位** | 二次确认弹窗里，**取消按钮在左、确认（危险色）在右**（TDesign 默认，不要反着来） |
| 12 | **移动端** | <992px：列表转卡片流；筛选区折叠为「筛选」按钮 + `t-drawer` |

---

## 8.1 【页面 A】公告列表（管理端）—— `/platform/announcement`

### 8.1.1 入口

| 入口 | 位置 |
|---|---|
| 侧边栏 | 通用底座 → 公告管理（`icon=article`，sort 60） |
| 顶栏（用户端） | `article` 图标 + 未读角标 → 跳 `/notice`（公告中心），**注意与"公告管理"不是同一个页面** |

### 8.1.2 筛选区（`t-card` + `t-form :layout="inline"`）

| 控件 | 类型 | 说明 |
|---|---|---|
| 标题 | `t-input` | 模糊，placeholder「输入标题关键词」 |
| 状态 | `t-select` | 草稿 / 待发布 / 已发布 / 已撤回 / 已过期（**默认不选=全部**） |
| 范围 | `t-select` | 全员 / 指定角色 / 指定部门（默认全部） |
| 发布人 | `t-input` | 模糊（账号或姓名） |
| 时间 | `t-date-range-picker` | 按"创建时间"筛，默认最近 90 天 |
| 按钮 | `t-button` | 「查询」（主色）+「重置」（默认） |
| 右侧 | `t-button` | 「+ 新增公告」（`v-privilege="['business:announcement:create']"`，主色） |

**筛选默认值**：状态=全部、时间=最近 90 天。**不要默认只显示"已发布"**（HR 经常回来改草稿，找不到会以为丢了）。

### 8.1.3 列表（`t-table`，`row-key="id"`）

| 列 | 宽度 | 内容 | 特殊 |
|---|---|---|---|
| 标题 | 240（ellipsis） | 标题 + 附件徽标（有附件显示 `📎 2`） | 置顶的标题前加 `t-tag theme="warning" variant="light"` 「置顶」 |
| 范围 | 140 | 「全员」/「角色：技术主管 等 2 个」/「部门：技术部 等 3 个（含下级）」 | 超过 1 个用「等 N 个」 |
| 状态 | 100 | `t-tag`：草稿(灰) / 待发布(主色) / 已发布(绿) / 已撤回(红) / 已过期(灰) | — |
| 有效期 | 170 | `10-01 00:00 ~ 10-08 00:00`；长期有效显示「长期」 | 已过期的时间标红 |
| 阅读 | 110 | 「287 / 312」+ 进度条（细，`t-progress`） | 扇形推送失败时显示红色「推送失败 ⟳」 |
| 发布人 | 100 | 姓名 | — |
| 创建时间 | 150 | `YYYY-MM-DD HH:mm` | — |
| 操作 | 200（fixed right） | 见下表 | — |

**操作列（按状态给不同按钮，不要一次给 8 个）**

| 状态 | 按钮 |
|---|---|
| 草稿 | 编辑 · 发布 · 删除 |
| 待发布 | 编辑 · 取消定时 · 删除 |
| 已发布 | 编辑 · 置顶/取消置顶 · 撤回 · 阅读明细(P1) |
| 已撤回 | 编辑 · 重新发布 · 删除 |
| 已过期 | 编辑 · 删除 |

- 按钮超过 3 个时，把次要的收进「更多」下拉（`t-dropdown`）；
- 每个按钮都带 `v-privilege`；**没有权限的按钮直接不渲染**（`v-privilege` 是 `display:none`，DOM 仍在但不占位）。

### 8.1.4 关键交互与反馈

| 操作 | 反馈（原文） |
|---|---|
| 点「发布」 | 弹窗：标题「发布公告」；内容：「将向 **312** 人推送此公告，并发送站内信。发布后用户端立即可见。是否发布？」；按钮「取消 / 发布」 |
| 点「撤回」 | 弹窗：标题「撤回公告」；内容：「撤回后**所有人都看不到了**，已有 **8** 人读过。撤回不会删除已发送的站内信。是否撤回？」；按钮「取消 / 撤回」（危险色） |
| 点「删除」（草稿） | 弹窗：「确定删除公告《国庆放假通知》？此操作不可恢复。」 |
| 点「删除」（已发布） | ❌ 按钮不显示（3.2 硬规则 1）；若通过接口硬调 → 400「已发布的公告请先撤回，再删除」 |
| 发布成功 | `MessagePlugin.success('发布成功，已推送给 312 人')`，列表自动刷新，该行状态变「已发布」 |
| 撤回成功 | `MessagePlugin.success('已撤回，用户端已不可见')` |
| 推送失败 | 行内显示红色「推送失败 ⟳」，点 ⟳ 调重试接口，成功提示「已补发 312 条通知」 |
| 置顶成功 | `MessagePlugin.success('已置顶，7 天后自动取消')`（用了默认置顶期时带上这句话） |

### 8.1.5 空态

| 场景 | 文案 |
|---|---|
| 没有任何公告 | 插画 + 「还没有发布过公告」+ 按钮「发布第一条公告」（有 `create` 权限才显示） |
| 筛选无结果 | 插画 + 「没有符合筛选条件的公告」+ 按钮「清空筛选」 |
| 无 `list` 权限 | **不进入该页面**（路由守卫拦截，跳 403 页） |

---

## 8.2 【页面 B】公告编辑 —— `/platform/announcement/edit/:id?`

**布局**：左右两栏，左栏表单（自适应），右栏「实时预览 + 发布检查」（固定 320px）。

### 8.2.1 表单（`t-form label-width="96px"`，`t-card` 分三块）

**块 1：基本信息**

| 字段 | 控件 | 校验 | 说明 |
|---|---|---|---|
| 标题 | `t-input` | 必填、≤200 字 | 输入框下方显示字数 `12/200` |
| 摘要 | `t-textarea :autosize="{minRows:2,maxRows:4}"` | ≤500 字 | placeholder「一句话说清这条公告是干什么的（会作为站内信正文）」 |
| 正文 | 富文本编辑器（见 8.2.3） | 必填（正文或摘要至少要有一个） | 高度 420px，可全屏 |

**块 2：发布范围**（`t-radio-group` 三选一）

```
○ 全员（312 人）        ← 无 publish-all 权限时此项 disabled + 右侧灰字「需『发布全员公告』权限」
○ 指定角色   [选择角色 ▾]
○ 指定部门   [选择部门 ▾]
```

| 选项 | 交互 |
|---|---|
| 全员 | 选中后下方显示「将发送给 **312** 人」 |
| 指定角色 | `t-select multiple` 多选角色；下方实时显示「预计发送 **87** 人」 |
| 指定部门 | `t-tree-select multiple check-strictly={false}`（部门树）；**每个已选部门右侧带「含下级」开关**（`t-switch size="small"`，默认开）；下方显示「预计发送 **28** 人」 |

- **人数实时算**：选完调 `POST /business/announcement/preview-target`，返回 `{total, by_target:[{name,count}]}`，界面显示「预计发送 **28** 人」+ 悬浮明细「技术部 9 人 / 前端组 12 人 / 测试组 7 人」。
- **无 `publish-all` 权限时**：部门树里**只出现本部门及下级**（前端过滤），且角色选项整体禁用并提示「角色可能跨部门，需要『发布全员公告』权限」。后端仍会再校验一次（6.3.1）。

**块 3：有效期与置顶**

| 字段 | 控件 | 校验 |
|---|---|---|
| 生效时间 | `t-date-picker enable-time-picker` | 默认 now；可选未来（定时发布） |
| 失效时间 | 同上 + 「长期有效」`t-checkbox` | 勾「长期有效」则禁用输入并置空；否则必须 > 生效时间 |
| 置顶 | `t-switch` | 打开后出现「置顶至」`t-date-picker`，默认 now+7 天 |
| 附件 | `t-upload`（`theme="file"`） | ≤10 个、合计 ≤50MB；上传中显示进度；已上传的显示文件名 + 大小 + 删除 |

**块 4：内部备注（可选）** `t-input` ≤255 字，placeholder「仅管理员可见，例如：为什么发这条公告」。

### 8.2.2 右栏「发布检查」清单（**这个面板是本页的灵魂**）

```
┌─ 发布前检查 ────────────────┐
│ ✅ 标题已填写               │
│ ✅ 正文非空                 │
│ ✅ 发布范围：技术部（含下级）│
│    预计发送 28 人           │
│ ⚠️ 正文中的 1 个 iframe 将被 │
│    移除（不支持内嵌视频）    │
│ ✅ 附件 2 个（3.1MB）       │
│ ⚠️ 有效期未设置（将长期可见）│
└─────────────────────────────┘
[ 保存草稿 ]  [ 发布 ]
```

- ✅/⚠️/❌ 三态，**有一个 ❌ 就不允许发布**（按钮 disabled + 悬浮说明原因）；
- ⚠️ 不阻断，但必须让作者看见（**净化必须显式告知**，3.13 第 15 条）；
- 「发布」按钮点击后：先弹 8.1.4 的确认框 → 成功后 `MessagePlugin.success('发布成功，已推送给 28 人')` 并**跳回列表**（不停留在编辑页）。

### 8.2.3 富文本编辑器（受限工具栏）

**工具栏只提供**：撤销/重做、标题(H1–H4)、粗体、斜体、下划线、删除线、有序列表、无序列表、引用、代码块、表格、链接、图片、对齐、清除格式、全屏。

**不提供**（并在工具栏最右侧放一个 `?` 图标，悬浮说明原因）：
```
为什么不能设置字体和颜色？
公告正文会在保存时做安全净化，字体、字号、颜色等样式会被移除。
如需固定版式，请使用「公告模板」（后续提供）。
```

**粘贴行为**：从 Word/网页粘贴时，**只保留文字与结构**（标题/列表/表格/粗斜体），丢弃字体、颜色、背景、外链样式。前端用编辑器的 `pasteFilter` 处理；**后端仍会再净化一次**（3.9.1，前端净化不承担安全职责）。

**图片插入**：点「图片」→ 上传（走批次 A 接口，`category='announcement'`，**强制公有**）→ 插入 https URL。若上传的是私有文件 → 拒绝并提示（3.9.4）。

### 8.2.4 关键反馈

| 场景 | 反馈 |
|---|---|
| 保存成功 | `MessagePlugin.success('已保存草稿')` |
| 发布成功 | `MessagePlugin.success('发布成功，已推送给 28 人')` + 跳列表 |
| 校验失败 | 滚到第一个错误字段 + 字段下方红字 |
| 净化移除了内容 | 保存成功提示里追加：「已移除不支持的标签：iframe」（见 3.13 第 15 条） |
| 网络超时（F24：全局 15s） | `MessagePlugin.error('保存超时，请检查网络后重试（内容已暂存在本地）')` —— 本地保留草稿（4.11 第 12 条同款机制） |
| 并发编辑冲突 | 打开时若 `updated_at` 已变：横幅提示「这条公告已被 王芳 于 14:31 修改，你看到的是最新版本」（P0 只提示，不做合并） |

---

## 8.3 【页面 C】公告中心（用户端）—— `/notice`

### 8.3.1 入口

| 入口 | 位置 |
|---|---|
| 顶栏 | `article` 图标 + 未读角标（8.0 第 12 条外的补充：与消息铃铛并排） |
| 站内信 | 点「查看公告」→ 直达 `/notice/detail/{id}` |
| 首页 | P1：首页仪表盘加「最新公告」卡片（本批不做，避免动批次 A 的 dashboard） |

### 8.3.2 列表

- 布局：单列卡片流（**不用 `t-table`**——公告是内容，不是数据行）；
- 每张卡片：`未读圆点 + 标题（未读加粗）+ 摘要（2 行 ellipsis）+ 底部「发布人 · 时间 · 📎附件数」`；置顶的卡片带**顶部左侧色条**（`var(--td-warning-color)`）与「置顶」标签；
- 顶部 tab：「全部(N) / 未读(M)」；
- 排序：置顶（未过期）→ `publish_at DESC`；
- 分页：每页 20，滚动加载（`t-list` + 触底加载）；
- 点卡片 → `/notice/detail/{id}`，**同时**：本地未读圆点消失、顶栏角标 -1、发标已读请求。

### 8.3.3 空态

| 场景 | 文案 |
|---|---|
| 一条公告都没有 | 插画 + 「暂时没有公告」 |
| 未读 tab 为空 | 「公告都看完啦 🎉」 |
| 无部门用户 | 列表上方灰色提示条：「你尚未归属部门，只能看到全员公告」 |

---

## 8.4 【页面 D】公告详情（用户端）—— `/notice/detail/:id`

**布局**：单栏居中（`max-width: 860px`），顶部返回：「‹ 返回公告中心」。

| 区域 | 内容 |
|---|---|
| 头部 | 标题（24px 加粗）+ 标签行：`置顶`(橙) + 范围标签（部门公告显示「技术部」）+ 已读状态（「已读」灰字，或首次进入时闪一下「已标记为已读」提示） |
| 元信息 | 「王芳 · 2026-10-29 16:20 · 有效期至 10-08 00:00」 |
| 正文 | `v-html="content_html"`（**已净化，不再二次处理**）；图片懒加载；表格横向滚动 |
| 附件 | 卡片列表：图标（按扩展名 `file-pdf`/`file-word`/`file-excel`/`file-zip`）+ 文件名 + 大小 + 「下载」按钮 |
| 底部 | 「这条公告对你有帮助吗？」👍 / 👎（**P1**，本批只留占位不埋点，避免半成品功能） |

**行为**
- 进入页面 **立即**调 `POST .../read`（不等用户滚动到底）——"点开即已读"符合直觉；
- 附件下载走批次 A 的下载接口；非私有文件直接下；下载失败提示「文件不存在或已被删除」（不暴露内部路径）；
- 公告已撤回/已过期/不可见 → 渲染 8.4.1 的"已下线"页；
- 底部固定「上一篇 / 下一篇」导航（按当前用户的可见列表顺序），减少"返回再点"的来回。

### 8.4.1 「已下线」页（撤回/过期/无权 统一落地页）

```
            [ 插画 ]
      该公告已撤回或已下线
   它可能已被发布人撤回，或已过有效期
        [ 返回公告中心 ]
```

- **不区分**"撤回"与"无权"（3.4.4：404 的统一落地页），避免探测；
- 有站内信来源时额外显示「你正在查看一条已失效的站内信提醒」。

---

## 8.5 【页面 E】帮助中心（用户端）—— `/help`、`/help/doc/:id`

### 8.5.1 三栏布局（4.9.1 已给结构图，这里给交互细节）

| 区域 | 交互 |
|---|---|
| 搜索框（左栏顶部） | 输入即搜（**300ms 防抖**）；回车立即搜；清空恢复浏览态；搜索结果**替换中间栏**，左栏分类树保留（方便放弃搜索直接点分类） |
| 分类树（左栏，240px） | `t-tree` 展开/折叠；当前分类高亮（`var(--td-brand-color-1)` 底）；分类旁显示文档数（灰字小号）；**空分类不显示**（4.3） |
| 正文（中间，max-width 860px） | 渲染规范见 4.6；滚动时 TOC 跟随高亮 |
| TOC（右栏，200px） | 吸顶（`position:sticky; top:80px`）；h2 一行、h3 缩进 12px、h4 缩进 24px；点击平滑滚动；当前项左侧竖线高亮 |

### 8.5.2 搜索结果列表（中间栏）

```
找到 63 条与「请假」相关的结果                       [按相关度 ▾]
────────────────────────────────────────────────
请假流程与审批时限                         人事 / 考勤
先在 OA 提交~~请假~~申请…（命中片段 60 字）  更新于 2026-09-12
────────────────────────────────────────────────
年假与调休的区别                            人事 / 考勤
~~请假~~类型共 5 种…                        更新于 2026-08-30
────────────────────────────────────────────────
                                    [ 加载更多（20/63）]
```

- 标题命中时标题内的词高亮；片段命中的词也高亮（`<mark>` 底 `var(--td-warning-color-2)`）；
- 搜「请假」但只命中正文的，片段**以命中位置为中心**（不是从头截 60 字）——否则用户看不出为什么命中；
- 无结果 → 4.9.3 的文案。

### 8.5.3 文档详情页

- 顶部面包屑：「帮助中心 / 人事 / 考勤 / 请假流程与审批时限」；
- 正文底部：「最近更新 2026-09-12 · 版本 4 · 编辑：王芳」+ 「这篇文档有帮助吗？」（P1）；
- 「复制链接」按钮（复制 `/help/doc/{id}`，不带 `kw` 参数——避免把别人的搜索词传下去）；
- 若从搜索进来（URL 带 `kw`）：进入后**自动高亮正文中的关键词**并滚到**第一个命中位置**（+ 顶部小提示「已为你高亮 3 处「请假」」+ 「清除高亮」）。

### 8.5.4 空态/异常态

| 场景 | 文案 |
|---|---|
| 帮助中心无任何文档 | 「帮助文档正在建设中，敬请期待」 |
| 分类下无文档 | 该分类整枝隐藏（4.3），不会出现"空分类页" |
| 文档已下架 | 「该文档已下架，可能正在更新中」+ 「返回帮助中心」 |
| 文档不存在 | 404 页 |
| 渲染失败（DOMPurify 异常） | 降级为 `<pre>` 显示 Markdown 原文 + 顶部提示「文档渲染异常，已为你显示原文」 |

---

## 8.6 【页面 F】帮助文档编辑（管理端）—— `/platform/help`

### 8.6.1 列表页（`/platform/help`）

左栏分类树（带「+ 分类」按钮，`v-privilege="['business:help:category:manage']"`）+ 右栏文档表格：

| 列 | 内容 |
|---|---|
| 标题 | 标题 + 状态标签（草稿灰/已发布绿/已下架红） |
| 分类 | 分类路径（"人事 / 考勤"） |
| 排序 | 数字（可直接编辑，失焦保存） |
| 版本 | `v4` |
| 最后编辑 | 姓名 + 时间 |
| 操作 | 编辑 · 发布/下架 · 删除 · 预览 |

顶部按钮：「+ 新建文档」（`v-privilege="['business:help:doc:create']"`）、「调整排序」（进入拖拽模式）。

**分类维护弹窗**（`t-dialog width=520`）：树形列表，每行「名称 + 图标 + 排序 + 启停开关 + 编辑/删除」；新增时选父级（`t-tree-select`）+ 名称 + 图标（复用 `IconSelect` 组件）。

### 8.6.2 编辑页（`/platform/help/edit/:id?`）

```
┌──────────────────────────────────────────────────────────────────┐
│ [← 返回]  编辑文档                            [预览] [保存草稿] [发布] │
├───────────────────────────────┬──────────────────────────────────┤
│ 标题 [如何申请 VPN 权限      ] │  预览（实时，300ms 防抖）         │
│ 分类 [人事 / 考勤  ▾] 排序[10] │  ┌────────────────────────────┐  │
│ ┌───────────────────────────┐ │  │ # 如何申请 VPN 权限         │  │
│ │ # 如何申请 VPN 权限        │ │  │ ## 一、适用对象             │  │
│ │                            │ │  │ 需要远程接入内网的同事…      │  │
│ │ ## 一、适用对象            │ │  │ ```bash                     │  │
│ │ 需要远程接入内网的同事…     │ │  │ ping 10.0.0.1               │  │
│ │                            │ │  │ ```                         │  │
│ │ ## 二、申请步骤            │ │  └────────────────────────────┘  │
│ │ 1. 打开 OA…                │ │  字数：1,284 · 预计阅读 5 分钟    │
│ └───────────────────────────┘ │                                  │
│ 工具栏：B I U S H1 H2 H3 列表 │  ⚠️ 预览仅本机渲染，发布后用户端   │
│        引用 代码 表格 链接 图片│     会再做一次安全净化           │
└───────────────────────────────┴──────────────────────────────────┘
```

| 项 | 定法 |
|---|---|
| 编辑器 | `t-textarea`（等宽字体，`min-height: 480px`）+ 工具栏按钮插入语法（不依赖第三方编辑器，**降低 P0 依赖风险**） |
| 预览 | 用与用户端**完全相同**的渲染管线（markdown-it + DOMPurify），保证"所见即所得"（若用不同管线，预览能用、正式页报错，就是灾难） |
| 快捷键 | `Ctrl/Cmd+B` 粗体、`I` 斜体、`K` 链接、`S` 保存草稿 |
| 自动保存 | 每 30s 存 localStorage；关闭/刷新时 `beforeunload` 提示 |
| 发布前检查 | 标题非空、分类已选、正文非空、代码块语言标记是否闭合（常见错误）、图片 URL 是否可达（异步 check，不阻断） |
| 保存后 | `MessagePlugin.success('已发布，用户端立即可见')`；若只是草稿：「已保存草稿」 |

### 8.6.3 关键反馈

| 场景 | 反馈 |
|---|---|
| 下架 | 弹窗：「下架后用户端立即不可见（直接访问链接也会提示已下架）。是否下架？」 |
| 删除 | 弹窗：「删除后不可恢复，已有 3 篇文章引用了这篇文档（引用不影响删除）。是否删除？」 |
| 分类被删 | 拒绝 + 数量（4.3） |
| 并发编辑 | 与 8.2.4 同款横幅（"已有人修改过"） |

---

## 8.7 【页面 G】变更日志（管理端）—— `/platform/changelog`

### 8.7.1 筛选区

| 控件 | 类型 | 默认 |
|---|---|---|
| 时间范围 | `t-date-range-picker enable-time-picker` | **最近 7 天** |
| 操作人 | `t-input` | 空（模糊：账号或姓名） |
| 模块 | `t-select` | 全部（系统管理 / 内容管理 / 支撑功能） |
| 动作 | `t-select` | 全部（新增/修改/删除） |
| 对象类型 | `t-select` | 全部（用户/角色/部门/公告/帮助文档） |
| 对象 ID | `t-input` | 空（精确；从"变更历史"入口进来时自动填） |
| 结果 | `t-radio-group` | 全部 / 仅成功 / 仅失败 |
| 按钮 | — | 查询 / 重置 / **导出**（`v-privilege="['business:changelog:export']"`，右上） |

**联动**：从用户详情页点「变更历史」跳过来时，URL 带 `target_type=User&target_id=xxx`，页面顶部显示可关闭的筛选标签：「对象：用户 zhangwei ✕」，时间范围自动放宽到 90 天（历史查询的默认 7 天太窄）。

### 8.7.2 列表

| 列 | 宽度 | 内容 |
|---|---|---|
| 时间 | 150 | `10-22 14:31:08` |
| 操作人 | 120 | 姓名（悬浮显示账号） |
| 模块 | 100 | `t-tag variant="light"`（系统管理/内容管理） |
| 动作 | 80 | 新增(绿)/修改(主色)/删除(红) |
| 对象 | 200 | 「用户 zhangwei」（`target_name` + 类型），ellipsis |
| 变更内容 | 220 | `change_summary`：「手机号、所属部门」；失败记录显示 `—` |
| IP | 130 | `10.20.3.88` |
| 结果 | 80 | ✅成功 / ❌失败 |
| 操作 | 80 | 「详情」（`v-privilege="['business:changelog:detail']"`） |

- 行点击**不跳页**，右侧滑出 `t-drawer` 详情（5.7.2 的布局）——日志是"连着看"的场景，跳页会破坏节奏；
- `request_id` 分组：同一 `request_id` 的连续多行（批量操作）在时间列左侧显示一个**竖向合并标记**，悬浮提示「同一批操作（5 条）」。

### 8.7.3 详情抽屉（diff 表）

严格按 5.7.2 的布局。补充三条交互规则：

| 规则 | 做法 |
|---|---|
| **只看变更**（默认开） | 只渲染 `changed_fields` 里的字段；关掉后追加白名单里的其他字段名 + 「未变更」（**值为空**，5.7.2 已说明取舍） |
| **脱敏后相同** | 该行新值/旧值下方加灰字「已变更，脱敏后显示相同」（5.4.3） |
| **长值** | `t-tooltip` 显示全文；超过 200 字的值显示「（已截断）」 |

### 8.7.4 导出交互

| 场景 | 反馈 |
|---|---|
| 命中 0 行 | 「当前筛选条件没有数据，无需导出」（按钮直接 disabled） |
| ≤1000 行 | 直接下载 xlsx；`MessagePlugin.success('已导出 1,284 行…')`（此处示例超限，见下） |
| >1000 行 | 弹窗：「本次将导出 **1,284** 行，超过同步导出上限（1000 行），已转后台导出。完成后会通过站内信通知你。」按钮「知道了」 |
| 异步导出完成 | 站内信：「变更日志导出完成，点击下载」（复用批次 A 的通知） |
| 导出失败 | `MessagePlugin.error('导出失败，请缩小时间范围后重试')` |

### 8.7.5 空态

| 场景 | 文案 |
|---|---|
| 无任何日志（新部署） | 「还没有变更记录。当有人修改业务数据时，这里会自动记录。」 |
| 筛选无结果 | 「没有符合条件的变化记录」+ 「尝试放宽时间范围」（因为日志常见问题是"时间选窄了"） |

---

## 8.8 三处错误提示文案总表（**用户看到的原文，开发逐条照抄**）

> 原则：①**不出现错误码**；②说清"为什么不行"；③尽量给"下一步怎么做"；④同一错误在三个模块里的说法保持一致（如"权限不足"全站统一）。

### 8.8.1 公告处

| 场景 | HTTP | 文案 | 展示方式 |
|---|---|---|---|
| 标题为空 | 400 | 请输入公告标题 | 字段下红字 |
| 标题超长 | 400 | 标题不能超过 200 个字 | 字段下红字 |
| 正文与摘要都为空 | 400 | 正文和摘要至少要填一个 | 字段下红字 |
| 正文过大 | 400 | 正文过大（超过 500KB），请精简内容或改用附件 | `MessagePlugin.error` |
| 失效时间早于生效时间 | 400 | 失效时间必须晚于生效时间 | 字段下红字 |
| 附件超数量 | 400 | 最多上传 10 个附件 | `MessagePlugin.error` |
| 附件超体积 | 400 | 附件总大小不能超过 50MB | `MessagePlugin.error` |
| 附件类型不允许 | 422 | 不支持的文件类型，请上传 PDF、Word、Excel、PPT、TXT 或 ZIP | `MessagePlugin.error` |
| 公告内图片用了私有文件 | 400 | 公告图片不能使用私有文件，请重新上传 | `MessagePlugin.error` |
| 目标范围无人 | 400 | 本次发布的目标范围内没有任何用户，请检查发布范围 | 右栏检查面板 ❌ |
| 无全员发布权限 | 403 | 你没有「发布全员公告」权限，请选择本部门范围 | `MessagePlugin.error` |
| 向范围外部门发布 | 403 | 你没有向该部门发布公告的权限 | `MessagePlugin.error` |
| 删除已发布公告 | 400 | 已发布的公告请先撤回，再删除 | `MessagePlugin.error` |
| 公告不存在/不可见 | 404 | 公告不存在或已下线 | 落地页 |
| 公告已撤回 | 404 | 该公告已撤回或已下线 | 落地页 + 「返回公告中心」 |
| 撤回他人公告无权限 | 403 | 你没有撤回该公告的权限 | `MessagePlugin.error` |
| 站内信推送失败 | — | 公告已发布，但站内信推送失败。可点列表中的「⟳」重试 | 行内红字 |
| 富文本被净化 | 200 | 发布成功，但已移除不支持的标签：iframe（公告不支持内嵌视频） | `MessagePlugin.warning` |

### 8.8.2 帮助文档处

| 场景 | HTTP | 文案 |
|---|---|---|
| 分类超过 3 层 | 400 | 分类最多 3 层 |
| 同级重名分类 | 409 | 同级下已存在同名分类 |
| 删除有内容的分类 | 409 | 该分类下还有 3 篇已发布文档、2 个子分类，不能删除 |
| 标题为空 | 400 | 请输入文档标题 |
| 发布时未选分类 | 400 | 请选择所属分类后再发布 |
| 正文为空 | 400 | 请输入文档正文 |
| 文档过大 | 400 | 文档过大（超过 2MB），请拆分或精简 |
| 代码块未闭合 | 400 | 第 37 行附近有未闭合的代码块（```），请检查 |
| 搜索为空结果 | 200 | 没有找到与「请假」相关的文档 |
| 文档已下架 | 404 | 该文档已下架，可能正在更新中 |
| 无编辑权限 | 403 | 你没有编辑帮助文档的权限，请联系系统管理员 |
| 并发编辑 | 409 | 该文档已被 王芳 于 14:31 修改，请刷新后重新编辑 |

### 8.8.3 变更日志处

| 场景 | HTTP | 文案 |
|---|---|---|
| 时间范围超限 | 400 | 单次查询时间范围不能超过 366 天 |
| 时间范围倒置 | 400 | 开始时间不能晚于结束时间 |
| 未选时间范围 | 400 | 请选择时间范围（最长 366 天） |
| 无查看权限 | 403 | 你没有查看变更日志的权限 |
| 无详情权限 | 403 | 你需要「查看变更详情」权限才能看到字段变化 |
| 无导出权限 | 403 | 你没有导出变更日志的权限 |
| 导出 0 行 | 400 | 当前筛选条件没有数据，无需导出 |
| 导出超限转异步 | 200 | 本次将导出 1,284 行，超过同步导出上限（1000 行），已转后台导出 |
| 导出失败 | 500 | 导出失败，请缩小时间范围后重试 |
| 归档查询范围过宽 | 400 | 查询归档数据时，时间范围不能超过 31 天 |
| 日志不存在 | 404 | 这条变更记录不存在或已归档 |
| 明文无权限（P1） | 403 | 查看明文需要「查看明文敏感字段」权限，请联系管理员申请 |

---

# 第 9 章 验收场景

> **判定原则**：每个场景都给"步骤 → 预期 → 必须记录的数据 → **判定语句/判定方式**"。验收时**不许只看界面**——界面可能是前端假装的，必须查库。判定语句可直接粘进 `psql`。

## 9.0 基础测试数据集（所有场景共用，**必须照此造数**）

### 部门树（6 个）

| 部门 | 父级 | 人数 |
|---|---|---|
| 云枢科技（根） | — | 0（虚拟根，不挂人） |
| ├ 技术部 | 云枢科技 | 9 |
| │ ├ 前端组 | 技术部 | 12 |
| │ └ 测试组 | 技术部 | 7 |
| ├ 市场部 | 云枢科技 | 47 |
| └ 财务部 | 云枢科技 | 3 |
| **合计** | — | **78**（另有 234 个用户不挂部门，用于"全员 312 人"与"无部门用户"两个边界） |

> **总用户数 312**（78 + 234），其中 `status=1` 的 310 人、`status=0` 的 2 人（用于验证"停用用户不收公告"）。

### 角色（4 个）

| 角色 | `role_code` | 权限 |
|---|---|---|
| 超级管理员 | `SUPER_ADMIN` | `*:*:*` |
| 行政 HR | `content_hr` | 6.4 的 HR 包（含 `publish-all`） |
| 部门主管 | `content_dept_manager` | 6.4 的主管包（**无 `publish-all`**） |
| 普通员工 | `normal_user` | `business:announcement:view` + `business:help:view` |
| 审计员 | `content_auditor` | 公告 list/view + 帮助 view + 变更日志 list/detail/export |

### 关键账号（5 个）

| 账号 | 姓名 | 部门 | 角色 |
|---|---|---|---|
| `wangfang` | 王芳 | 云枢科技（挂根） | `content_hr` |
| `liqiang` | 李强 | 技术部 | `content_dept_manager` |
| `zhangwei` | 张伟 | 市场部 | `normal_user` |
| `chentest` | 陈测 | 技术部/前端组 | `normal_user` |
| `zhaoshen` | 赵审 | 云枢科技 | `content_auditor` |

---

## 9.1 【公告 · 全员】发一条置顶公告 → 全员可见、未读 +1、点击变已读 ← 任务书场景 1

| 步骤 | 操作 | 预期 |
|---|---|---|
| 1 | `wangfang` 登录，进入公告管理 → 新增 | 打开编辑页，右栏"发布前检查"列出 5 项检查 |
| 2 | 填标题《国庆放假通知》、摘要、正文（含 3 行表格）、上传 `测试.pdf`（1.2MB） | 右栏检查全 ✅，附件显示「2 个 / 1.2MB」 |
| 3 | 范围选「全员」 | 下方显示「预计发送 **310** 人」（312 减去 2 个停用） |
| 4 | 有效期 今天 00:00 ~ +7 天；置顶打开（默认 7 天） | 「置顶至」自动填 now+7d |
| 5 | 点「发布」→ 确认弹窗 | 弹窗写「将向 **310** 人推送此公告…」 |
| 6 | 确认 | `MessagePlugin.success('发布成功，已推送给 310 人')`，跳回列表，该行状态「已发布」，置顶标签橙色 |
| 7 | `zhangwei`（市场部）登录，看顶栏 | `article` 图标角标显示 **1** |
| 8 | 点开公告中心 | 列表仅有这一条，**未读加粗 + 圆点**，带「置顶」标签 |
| 9 | 点开详情 | 正文表格渲染正常；附件可下载；**0.5s 内**角标 1→0，列表圆点消失 |
| 10 | 刷新页面 | 角标仍为 0（已读不回退） |
| 11 | `wangfang` 看管理端列表 | 阅读列显示「1 / 310」 |

**必须记录的数据**：`announcement_id`、发布瞬间时间、第 7 步角标数字、第 11 步阅读人数。

**判定语句**

```sql
-- ① 公告已发布且置顶生效
SELECT status, is_top, target_count FROM sys_announcement WHERE id = :aid;
-- 期望：PUBLISHED | t | 310

-- ② 站内信收件人 = 310（且不含停用用户）
SELECT count(*) FROM sys_notice_receiver r
  JOIN sys_notice_message m ON m.id = r.message_id
  JOIN sys_user u ON u.id = r.receiver_id
 WHERE m.biz_ref_id = :aid::text AND u.status = 1;
-- 期望：310

-- ③ zhangwei 已读记录存在且只有一条
SELECT count(*) FROM sys_announcement_read
 WHERE announcement_id = :aid
   AND user_id = (SELECT id FROM sys_user WHERE username='zhangwei');
-- 期望：1

-- ④ 站内信的跳转地址正确
SELECT business_key, action_url, action_text FROM sys_notice_message WHERE biz_ref_id = :aid::text;
-- 期望：content.announcement | /notice/detail/<aid> | 查看公告
```

**边界追加验证**：把 `zhangwei` 的 `status` 改为 0 后重新发布一条公告 → 该用户**收不到**站内信、角标不 +1（扇出算法第 1 步带 `status=1`，3.3.2）。

---

## 9.2 【公告 · 部门】只给技术部发 → 其他部门看不到（**不能靠 ID 直查绕过**）← 任务书场景 2

| 步骤 | 操作 | 预期 |
|---|---|---|
| 1 | `liqiang`（技术部主管）新增公告《春节值班安排》，范围选「指定部门 → 技术部（含下级）」 | 部门树里**只能看到**技术部及其下级（前端过滤） |
| 2 | 实时人数 | 显示「预计发送 **28** 人」（9+12+7，与 9.0 数据集一致） |
| 3 | 发布 | 成功，`MessagePlugin.success('发布成功，已推送给 28 人')` |
| 4 | `zhangwei`（市场部）登录公告中心 | **看不到**这条；角标不 +1 |
| 5 | `zhaoshen`（无部门）登录 | **看不到**（3.4.5：DEPT 公告对无部门用户不可见） |
| 6 | `chentest`（技术部/前端组）登录 | **看得到**（父部门「含下级」生效） |
| 7 | `zhangwei` 用浏览器直接访问 `/notice/detail/{aid}` | **404 落地页**「该公告已撤回或已下线」（**不是 403**，3.4.4） |
| 8 | `zhangwei` 直接 `curl` 详情接口 | 同样返回 404 + `code=10002` |
| 9 | `zhangwei` 直接 `curl` 标已读接口 `POST /business/announcement/portal/{aid}/read` | 404，且**阅读人数不变** |
| 10 | `liqiang` 尝试把范围改成「全员」 | 前端选项 disabled + 灰字「需『发布全员公告』权限」 |
| 11 | `liqiang` 用 `curl` 强行提交 `scope_type=ALL` | **403**「你没有『发布全员公告』权限，请选择本部门范围」，**公告未被创建** |
| 12 | `liqiang` 用 `curl` 强行提交目标部门 = 市场部 | **403**「你没有向该部门发布公告的权限」 |

**必须记录的数据**：`announcement_id`、`target_count`、第 7/8/9 步的 HTTP 状态码与响应体 `code`、第 11/12 步是否产生新公告行。

**判定语句**

```sql
-- ① 目标只有技术部一个节点，且含下级
SELECT target_type, target_id, include_children FROM sys_announcement_target WHERE announcement_id = :aid;
-- 期望：1 行，DEPT, <技术部 id>, TRUE

-- ② 市场部用户没有已读记录（第 9 步被拦截 → 无插入）
SELECT count(*) FROM sys_announcement_read
 WHERE announcement_id = :aid AND user_id = (SELECT id FROM sys_user WHERE username='zhangwei');
-- 期望：0

-- ③ 第 11/12 步失败后没有新公告
SELECT count(*) FROM sys_announcement WHERE title = '春节值班安排';
-- 期望：1（只有第 3 步成功的那一条）
```

> **这条验收的核心是"四条读取路径全部要拦"**（3.4.2）：列表、详情、未读数、标已读。只拦列表是最常见的偷懒做法，必须逐条用 `curl` 打一遍。

---

## 9.3 【公告 · 有效期】设置今天 18:00 下线 → 18:00 后自动消失 ← 任务书场景 3

**准备**：把服务器时间调到 17:58（或用 `expire_at = now()+2min` 造一条），发布一条全员公告。

| 步骤 | 时间点 | 操作 | 预期 |
|---|---|---|---|
| 1 | 17:58 | `wangfang` 发布公告，失效时间 18:00 | 发布成功，`zhangwei` 能看到 |
| 2 | 17:59:30 | `zhangwei` 刷新公告中心 | **仍能看到** |
| 3 | 18:00:05 | `zhangwei` 刷新（**不等定时任务**） | **立即看不到**（谓词生效，D7 第一条） |
| 4 | 18:00:05 | `zhangwei` 访问详情 URL | 404「该公告已撤回或已下线」 |
| 5 | 18:00:05 | 查库 | `status` **仍是 `PUBLISHED`**（任务还没跑，5 分钟一次） |
| 6 | 18:06 | `wangfang` 看管理端列表 | 状态列变「已过期」（灰），操作只剩「编辑/删除」 |
| 7 | 18:06 | 查库 | `status='EXPIRED'` |
| 8 | — | 把失效时间改成明天，重新发布 | 又可见 |

**判定语句**

```sql
-- ③ 谓词立即生效（不依赖任务）：模拟 zhangwei 的可见性
SELECT count(*) FROM sys_announcement a
 WHERE a.id = :aid AND a.status IN ('PUBLISHED','EXPIRED')
   AND (a.publish_at IS NULL OR a.publish_at <= now())
   AND (a.expire_at IS NULL OR a.expire_at > now());
-- 18:00 之后期望：0

-- ⑥ 任务翻转状态（等待 ≤5 分钟后）
SELECT status FROM sys_announcement WHERE id = :aid;
-- 期望：EXPIRED
```

**允许的偏差**：状态翻转最长滞后 5 分钟（任务间隔）；**但用户端不可见必须秒级**。这两条要分别验，不能合成一条。

---

## 9.4 【帮助 · 搜索】搜「请假」→ 命中标题或正文含该词的文档，关键词高亮 ← 任务书场景 4

**准备**：造 5000 篇已发布文档（可用脚本），其中 **51 篇标题含「请假」**、**12 篇仅正文含「请假」**（参考 F33 的实测数据集）。

| 步骤 | 操作 | 预期 |
|---|---|---|
| 1 | `zhangwei` 打开 `/help`，搜索框输入「请假」 | 300ms 后自动搜；结果区显示「找到 **63** 条与「请假」相关的结果」 |
| 2 | 看结果列表 | 标题命中的排在前（51 条），每条的**标题内「请假」高亮** |
| 3 | 看某条仅正文命中的 | 片段**以命中位置为中心**截 60 字，「请假」高亮 |
| 4 | 点进《请假流程与审批时限》 | 正文渲染正常；**正文内关键词自动高亮**；顶部提示「已为你高亮 3 处「请假」」 |
| 5 | 看右侧 TOC | 列出 h2–h4（一、适用范围 / 二、申请步骤 / 三、审批时限 / 四、常见问题） |
| 6 | 滚动正文 | TOC 当前项跟随高亮（`IntersectionObserver`） |
| 7 | 搜「%」 | 不返回全部（`%` 被转义），返回 0 条，提示无结果 |
| 8 | 搜「请假流程」 | 走 trgm 索引，结果 ≤ 63 条，响应更快 |
| 9 | 搜「请假 howto」（两词） | AND 语义：只返回同时含两词的文档（预期 0 条，提示无结果） |
| 10 | 下架一篇命中文档 | 再搜，该文档消失 |

**判定语句**

```sql
-- ① 命中数（标题 + 正文）
SELECT count(*) FROM sys_help_doc
 WHERE deleted_at IS NULL AND status='PUBLISHED'
   AND (title ILIKE '%请假%' OR content_text ILIKE '%请假%');
-- 期望：63

-- ② 标题命中数（用于验证排序：标题命中优先）
SELECT count(*) FROM sys_help_doc WHERE title ILIKE '%请假%' AND status='PUBLISHED';
-- 期望：51

-- ④ 已下架的文档不出现
SELECT count(*) FROM sys_help_doc
 WHERE status='OFFLINE' AND (title ILIKE '%请假%' OR content_text ILIKE '%请假%');
-- 记录数量，并确认搜索接口返回里不含这些 id
```

**性能记录**（验收必填）：2 字关键词「请假」的接口耗时。实测基线：5000 篇 9.75ms（F33），**红线 P95 < 150ms**。

---

## 9.5 【变更日志 · diff】改某用户手机号 → 详情显示脱敏前后值 ← 任务书场景 5

| 步骤 | 操作 | 预期 |
|---|---|---|
| 1 | 记录 `zhangwei` 当前 `mobile = 13812345678` | — |
| 2 | `wangfang` 在用户管理把其手机号改为 `13900005678` | 保存成功 |
| 3 | 打开「变更日志」 | 最新一条：`刚刚 · 王芳 · 系统管理 · 修改 · 用户 zhangwei · 成功`，变更内容列「手机号」 |
| 4 | 点「详情」 | diff 表：**手机号 \| 138\*\*\*\*1234 \| 139\*\*\*\*5678**；只有一行（其他字段未变） |
| 5 | 查库 `after_data` | 存的是 `139****5678`，**不是明文** |
| 6 | 从用户详情页点「变更历史」 | 跳到变更日志，筛选已自动填「对象：用户 zhangwei」，列出该用户全部变更 |
| 7 | 把手机号从 `13812345678` 改成 `13899995678`（只改中间 4 位） | 详情页显示新旧值都是 `138****5678`，**并带灰字「已变更，脱敏后显示相同」** |
| 8 | 只改 `remark`（备注） | 记录 1 条，变更内容「备注」，值截断 200 字 |
| 9 | 保存一个**完全没改**的表单 | **不产生日志**（5.11 第 5 条） |

**判定语句**

```sql
-- ③/④ diff 数据来源
SELECT changed_fields, before_data, after_data, change_summary
  FROM sys_change_log
 WHERE target_type='User' AND target_id = (SELECT id::text FROM sys_user WHERE username='zhangwei')
 ORDER BY created_at DESC LIMIT 1;
-- 期望：changed_fields = ["mobile"]
--       before_data = {"mobile":"138****1234"}
--       after_data  = {"mobile":"139****5678"}
--       change_summary = "手机号"

-- ⑤ 库内无明文（全表扫描兜底，必须 0）
SELECT count(*) FROM sys_change_log WHERE (after_data::text || before_data::text) ~ '[0-9]{11}';
-- 期望：0

-- ⑦ 脱敏后相同的场景：changed_fields 里仍然有 mobile（明文比较的结果）
SELECT changed_fields FROM sys_change_log WHERE ... ORDER BY created_at DESC LIMIT 1;
-- 期望：["mobile"]（不是 []）

-- ⑨ 无变化不记
SELECT count(*) FROM sys_change_log WHERE created_at > :before_save AND target_id = :uid;
-- 期望：与保存前相比 **没有新增**（该次保存不产生行）
```

---

## 9.6 【变更日志 · 回滚不留假日志】**本批最重要的验收** ← 任务书场景 6

### 9.6.1 造一个"必然回滚"的操作

**方式（推荐，无需改代码）**：并发唯一键冲突。
1. 已知存在账号 `zhangwei`；
2. 用 `admin` 打开 `liqiang` 的编辑页，**同时**把「账号」改成 `zhangwei`（违反 `uk_sys_user_username` 唯一索引）、把「手机号」改成 `13911112222`；
3. 点保存。

**预期**：保存失败，前端提示「用户名已存在」（或「保存失败，请重试」+ 后端 409）；**手机号没变**。

### 9.6.2 判定（四条同时成立才算通过）

| # | 判定 | 语句 | 期望 |
|---|---|---|---|
| **①** | 业务数据未变 | `SELECT mobile, username FROM sys_user WHERE id = :liqiang_id;` | 手机号仍是旧值，账号仍是 `liqiang` |
| **②** | **变更日志里没有成功记录** | `SELECT count(*) FROM sys_change_log WHERE request_id = :rid AND result = 'SUCCESS';` | **0** |
| **③** | 失败留痕允许 0 或 1 条，且**必须无快照** | `SELECT result, before_data IS NULL AS b_null, after_data IS NULL AS a_null FROM sys_change_log WHERE request_id = :rid;` | 要么 0 行；要么 1 行且 `result='FAILED'`、`b_null=true`、`a_null=true` |
| **④** | 审计日志里也不能出现"成功" | `SELECT count(*) FROM audit_logs WHERE request_id = :rid AND operation='USER_UPDATE';` | **0**（`record_audit` 只在成功后调用，见 `handler.rs:316`，回滚走不到） |

### 9.6.3 反例复现（用来证明"三道闸门"真的在起作用）

| 实验 | 做法 | 期望结果 |
|---|---|---|
| **反例 A：事务外写日志** | 临时把 `change_log::record` 挪到事务外（仅测试分支） | ② 变成 **1** → **验收失败**，说明闸门①有效（这条实验做完必须改回去） |
| **反例 B：FAILED 带快照** | 手工 `INSERT INTO sys_change_log (result, after_data) VALUES ('FAILED', '{"mobile":"x"}')` | 数据库报 `check_violation`（`ck_cl_failed_no_snapshot`）→ 说明闸门③有效 |
| **反例 C：事务内写失败记录** | 在事务闭包内调 `record_failure` 并让事务回滚 | 失败日志一并消失（这正是"禁止清单第 2 条"的场景） |

### 9.6.4 额外的并发场景（P0 必须过）

两个请求同时改同一个用户的手机号（一个改 139…、一个改 137…）：

| 判定 | 期望 |
|---|---|
| 业务表 | 只有**一个**值（后提交者胜出，或第二个因乐观锁失败） |
| 变更日志 | 成功记录数 = **实际提交成功的事务数**（1 或 2），**不能出现"记了但没提交"的记录** |
| 请求链路 | 两条记录（若有 2 条）的 `request_id` 不同 |

> 这一条能抓出"用全局变量/请求上下文缓存 tx"的野路子实现。

---

## 9.7 【导出脱敏】导出变更日志 Excel → 手机号等字段已打码 ← 任务书场景 7

| 步骤 | 操作 | 预期 |
|---|---|---|
| 1 | `zhaoshen`（审计员）进变更日志，筛最近 90 天 | 列表有数据（≥1 条含手机号变更） |
| 2 | 点「导出」 | 命中行数 >1000 时提示转异步；测试数据 <1000 时直接下载 |
| 3 | 打开 xlsx | 12 列齐全，首行冻结；**手机号列全是 `138****1234` 形态** |
| 4 | 在文件里搜 11 位连续数字 | **一个都搜不到**（`Ctrl+F` 正则 `\d{11}`） |
| 5 | 查 `audit_logs` | 有一条 `CHANGELOG_EXPORT` 记录（谁、什么时候、筛了什么条件、多少行） |
| 6 | 用 `normal_user` 直接 `curl` 导出接口 | **403**「你没有导出变更日志的权限」 |
| 7 | 用 `zhaoshen` 把筛选清空（不选时间） | 拒绝：「请选择时间范围（最长 366 天）」 |
| 8 | 筛一个空结果集再导出 | 按钮 disabled；强行调接口 → 400「当前筛选条件没有数据，无需导出」 |

**判定语句**

```sql
-- ③ 导出文件的口径来自同一份脱敏数据（抽查）
SELECT before_data->>'mobile', after_data->>'mobile' FROM sys_change_log
 WHERE changed_fields @> '["mobile"]'::jsonb ORDER BY created_at DESC LIMIT 5;
-- 手工核对导出的这 5 行与库内一致

-- ④ 全库无明文（红线 2）
SELECT count(*) FROM sys_change_log WHERE (before_data::text || after_data::text) ~ '[0-9]{11}';
-- 期望：0

-- ⑤ 导出行为被审计
SELECT username, operation, detail FROM audit_logs WHERE operation='CHANGELOG_EXPORT' ORDER BY created_at DESC LIMIT 1;
-- 期望：有记录，detail 里含 filters 与 rows
```

---

## 9.8 反例组一：日志噪声与误记（防止"什么都记"）

| # | 场景 | 期望 |
|---|---|---|
| 1 | 打开编辑页但没改任何字段，直接点保存 | **不产生**变更日志 |
| 2 | 只改了 `updated_at`（系统自动） | **不产生**（技术字段不在白名单） |
| 3 | 重置密码 | **不写**变更日志；写 `password_change_logs`；`sys_change_log` 里 `after_data` 不含任何密码相关键 |
| 4 | 系统任务把公告置为"已过期" | **不写**变更日志（无操作人）；写 `sys_job_log` |
| 5 | 批量删除 5 个用户 | 产生 **5 条**日志，共享同一 `request_id` |
| 6 | 一次保存改 3 个字段 | 产生 **1 条**日志，`changed_fields` 有 3 项 |
| 7 | 导入 1000 行物料（批次 A 能力） | 本批**不接**入导入（5.5.2），不产生变更日志 |

---

## 9.9 反例组二：XSS（两个模块、8 个载荷，任一惊叹号即失败）

| # | 载荷 | 公告（富文本） | 帮助文档（Markdown） |
|---|---|---|---|
| 1 | `<script>alert(1)</script>` | 保存后 DB 里无 `<script>`；页面上**显示为文本或消失**，不弹窗 | 页面显示为**文本**（`html:false` 转义） |
| 2 | `<img src=x onerror=alert(1)>` | DB 里无 `onerror` | 显示为文本 |
| 3 | `[点我](javascript:alert(1))` | — | `href` 被移除，点击无反应 |
| 4 | `![x](javascript:alert(1))` | — | `src` 被移除 |
| 5 | `<p style="position:fixed;…">钓鱼</p>` | DB 里无 `style`（F19 实测） | 显示为文本 |
| 6 | `<svg onload=alert(1)>` | DB 里无 `<svg>` | 显示为文本 |
| 7 | `<iframe src="//evil.com">` | DB 里无 `<iframe>`，且**返回提示**「已移除不支持的标签：iframe」 | 显示为文本 |
| 8 | `<img src="data:text/html;base64,…">` | `src` 被清空 | `src` 被移除 |

**判定方式**：①用 `curl` **绕过前端**直接提交（这是关键——前端净化不算数）；②查 DB 断言无危险标签/属性；③用浏览器打开验证不弹窗；④**粘贴**到编辑器再保存一次（验证粘贴路径也被净化）。

**判定语句**

```sql
SELECT count(*) FROM sys_announcement
 WHERE content_html ~* '<(script|iframe|svg|style)' OR content_html ~* 'on[a-z]+=';
-- 期望：0

SELECT count(*) FROM sys_announcement WHERE content_html ~* 'style=' ;
-- 期望：0
```

---

## 9.10 反例组三：分区与归档（防"上线三个月就撑爆"）

| # | 场景 | 期望 |
|---|---|---|
| 1 | 写入 `created_at` 在两个月前的日志 | 进对应月份分区（`tableoid` 可查），不报错 |
| 2 | 写入一个"没有预建分区"的月份 | 落 `default` 分区，**不报错**；次日任务补建分区并告警 |
| 3 | 归档任务跑一次 | 目标月份分区从在线表 `DETACH` → `ATTACH` 到归档表；在线表该月数据为 0，归档表可查 |
| 4 | 归档任务重复跑 | 幂等，不报错（`pg_class` 判断） |
| 5 | 分区表插入（业务正常写入） | 与普通表无差异（框架 ORM 无需特殊处理） |
| 6 | `DROP PARTITION`（模拟 5 年到期） | 秒级完成，不影响其他分区写入 |

**判定语句**

```sql
-- ① 分区路由
SELECT tableoid::regclass AS part, count(*) FROM sys_change_log GROUP BY 1 ORDER BY 1;

-- ③ 归档结果
SELECT count(*) FROM sys_change_log  WHERE created_at < now() - interval '12 months';  -- 期望 0
SELECT count(*) FROM sys_change_log_archive WHERE created_at < now() - interval '12 months'; -- 期望 >0
```

---

# 第 10 章 后续规划

批次 C 交付后，云枢的"内容侧"具备了公告、帮助、变更审计三件套，而这正是批次 D（工作流审批）的输入端：公告的"定时发布/到期下线"已经是简易的时间驱动流程，D 批次会把它纳入统一的审批引擎（"公告需要主管审批后才能全员发布"），并把本批预留的两个口子接上——①帮助文档的 `visible_scope`（按角色可见）与公告的"发布前审核"合成为一条审批规则；②变更日志 P1 的"明文查看与明文导出"必须走 D 的审批流（申请 → 时限 → 二次审计 → 自动回收），绝不做成"点一下就给明文"，同时把"编辑 ↔ 审核不得同人"的 SoD 规则接上批次 B 的角色互斥表。此外本批的三项 P1 收尾（公告阅读明细与催办、帮助文档版本对比与回滚、变更日志归档查询与趋势看板）与 D 批次的第一个迭代并行排期，不单独占一个批次；若合规在 C 期间提出"变更日志留存延长到 10 年"或"变更日志需第三方存证（哈希链）"，按附录 B/B6 的既有结论走——**先确认是"留存期不够"还是"防篡改不够"，前者改配置，后者属独立的安全存证域，不在内容管理批次里做**。

---

# 附录 A 文案总表（用户可见文案索引）

> 全部原文集中在第 8.8 节（三处错误提示）。本表只做**索引**，避免同一句话在两个地方写法不同。

| 分类 | 文案 | 出处 |
|---|---|---|
| 公告·成功 | 发布成功，已推送给 312 人 | §8.1.4 |
| 公告·成功 | 已撤回，用户端已不可见 | §8.1.4 |
| 公告·成功 | 已置顶，7 天后自动取消 | §8.1.4 |
| 公告·成功 | 已保存草稿 | §8.2.4 |
| 公告·警告 | 发布成功，但已移除不支持的标签：iframe（公告不支持内嵌视频） | §8.8.1 |
| 公告·警告 | 公告已发布，但站内信推送失败。可点列表中的「⟳」重试 | §8.8.1 |
| 公告·确认 | 将向 312 人推送此公告，并发送站内信。发布后用户端立即可见。是否发布？ | §8.1.4 |
| 公告·确认 | 撤回后所有人都看不到了，已有 8 人读过。撤回不会删除已发送的站内信。是否撤回？ | §8.1.4 |
| 公告·确认 | 该公告已有 287 人读过，修改后他们看到的将是新内容。是否继续？ | §3.2 |
| 公告·空态 | 还没有发布过公告 / 没有符合筛选条件的公告 | §8.1.5 |
| 公告·用户端空态 | 暂时没有公告 / 公告都看完啦 🎉 | §8.3.3 |
| 公告·落地页 | 该公告已撤回或已下线 | §8.4.1 |
| 公告·无部门提示 | 你尚未归属部门，只能看到全员公告 | §8.3.3 |
| 公告·编辑器说明 | 为什么不能设置字体和颜色？…（见原文） | §8.2.3 |
| 帮助·成功 | 已发布，用户端立即可见 / 已保存草稿 | §8.6.2 |
| 帮助·空态 | 帮助文档正在建设中，敬请期待 | §8.5.4 |
| 帮助·空态 | 没有找到与「请假」相关的文档 | §4.9.3 |
| 帮助·落地页 | 该文档已下架，可能正在更新中 | §8.5.4 |
| 帮助·异常 | 文档渲染异常，已为你显示原文 | §8.5.4 |
| 帮助·搜索提示 | 已为你高亮 3 处「请假」/ 清除高亮 | §8.5.3 |
| 帮助·并发 | 该文档已被 王芳 于 14:31 修改，请刷新后重新编辑 | §8.8.2 |
| 变更日志·成功 | 已导出 1,284 行（同步）/ 已转后台导出 | §8.7.4 |
| 变更日志·空态 | 还没有变更记录。当有人修改业务数据时，这里会自动记录。 | §8.7.5 |
| 变更日志·空态 | 没有符合条件的变化记录 / 尝试放宽时间范围 | §8.7.5 |
| 变更日志·提示 | 已变更，脱敏后显示相同 | §5.4.3 |
| 变更日志·提示 | 本次将导出 1,284 行，超过同步导出上限（1000 行），已转后台导出… | §8.7.4 |
| 变更日志·确认 | 查询归档数据时，时间范围不能超过 31 天 | §8.8.3 |
| 通用·权限 | 你没有…权限，请联系系统管理员 | §8.8 各处 |
| 通用·404 | 数据不存在或你没有访问权限（批次 B D4 口径） | §3.4.4 |

---

# 附录 B 待拍板决策（6 条，本文已按"推荐值"落地，改主意只改一处）

| # | 问题 | 本文推荐值 | 备选 | 影响面 |
|---|---|---|---|---|
| **B1** | 公告正文的样式自由度 | **受限样式**（无字体/字号/颜色），靠"公告模板"满足版式需求 | 放开 `style` 白名单（需自建 ammonia Builder + CSS 过滤，且要评估 `position:fixed` 覆盖层钓鱼风险） | 3.9.3、8.2.3；放开 = 重做净化层，工期 +3~5 天 |
| **B2** | 公告富文本编辑器选型 | **P0 用 `t-textarea` + 工具栏按钮**（零新增依赖，功能够用）；**P1 引入 `@wangeditor/editor-for-vue@5`** | 直接上 wangEditor（更好用，但新增一个约 200KB 的前端依赖，且需评估隐私/维护性） | 8.2.3；P0 选 textarea 意味着"所见即所得"弱一些，但风险最低 |
| **B3** | 公告外链是否新窗口打开 | **接受默认**（净化后 `target` 被删，链接在当前页打开） | 自建 ammonia Builder 加 `target` + 自动补 `rel="noopener noreferrer"`（F21 已证零下载） | 3.9.2；改的话是后端一个函数，但必须重跑全部 XSS 用例 |
| **B4** | 是否要做"公告模板"（红头文件式版式） | **P1**，P0 不做 | P0 做（需要模板编辑器 + 变量占位，工期 +5 天） | 3.9.3、B1；这是 B1 的替代方案，两者绑在一起看 |
| **B5** | 帮助搜索超过性能红线后怎么做 | **先加"热门文档"缓存 + 限制 2 字词搜索频率（P1）**；不上中文分词扩展 | 引入 `zhparser`/`pg_bigm`（需换数据库镜像，运维成本高） | 4.5.3、4.5.4 |
| **B6** | 变更日志是否需要"防篡改"（哈希链/第三方存证） | **本批不做**；只做"表结构上不留 `updated_at`（改了会丢字段）+ 导出留痕 + 超管也留痕" | 做哈希链（每条含前一条哈希），需独立设计 | 附录 D 第 12 条；若合规硬要求，属独立批次 |

---

# 附录 C 非功能与交付前提

## C.1 数据库变更（9 张新表 = 8 张 P0 + 1 张 P1 预留；另 3 个字典扩充）

| # | 表 | 类型 | 说明 |
|---|---|---|---|
| 1 | `sys_announcement` | 新建（业务表，软删） | 公告主表 |
| 2 | `sys_announcement_target` | 新建（明细表，随主表硬删） | 范围明细 |
| 3 | `sys_announcement_read` | 新建（追加表，自然清理） | 已读 |
| 4 | `sys_announcement_file` | 新建（明细表） | 附件关联 |
| 5 | `sys_help_category` | 新建（树表，软删） | 分类 |
| 6 | `sys_help_doc` | 新建（业务表，软删） | 文档 |
| 7 | `sys_change_log` | 新建（**按月 RANGE 分区**，追加表，**无软删**） | 变更日志在线 |
| 8 | `sys_change_log_archive` | 新建（**同构分区**，只读） | 变更日志归档 |
| 9 | `sys_help_doc_version` | 新建（P1 用，P0 可只建不写） | 文档版本 |
| 10 | `sys_menu` | **追加种子行**（3 可见 + 2 隐藏 + 16 按钮 = 21 行） | 权限点 |
| 11 | `sys_dict_type` / `sys_dict_item` | **追加**：`log_field_label` 新类型；`log_operation` +14 项；`log_target_type` +5 项 | 字典 |
| 12 | `sys_job` | 追加 5 个内置任务行（公告 3 个 + 变更日志 2 个，见 3.12 与 5.8.3） | 定时任务 |
| 13 | `sys_config` | 追加 4 个配置键（`changelog.retention.months` 等） | 配置 |
| 14 | 既有表 | **零改动** | `sys_user`/`sys_role`/`sys_dept`/`sys_file`/`sys_notice_*`/`audit_logs` 全部不动 |

**分区表的三个硬约束（实测 F31，开发必读）**

1. `PRIMARY KEY (id, created_at)` —— 分区表主键**必须包含分区键**，否则建表失败；
2. 预建分区 + `default` 兜底分区（写入永不会因缺分区失败）；
3. 归档表必须是**同构分区表**，`ATTACH` 才能成功；`DETACH` 出来的分区**已带主键**，不要重复 `ADD PRIMARY KEY`。

**关键片段（本机实测通过的原样 SQL，供迁移文件参考）**

```sql
CREATE TABLE sys_change_log (
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    request_id VARCHAR(36) NOT NULL,
    module_code VARCHAR(20) NOT NULL,
    operation VARCHAR(50) NOT NULL,
    action VARCHAR(10) NOT NULL,
    target_type VARCHAR(50) NOT NULL,
    target_id VARCHAR(100) NOT NULL,
    target_name VARCHAR(200),
    result VARCHAR(10) NOT NULL DEFAULT 'SUCCESS',
    changed_fields JSONB NOT NULL DEFAULT '[]',
    change_summary VARCHAR(500),
    before_data JSONB,
    after_data JSONB,
    operator_id UUID, operator_name VARCHAR(50), operator_dept_id UUID,
    ip INET, user_agent TEXT, extra JSONB,
    PRIMARY KEY (id, created_at),
    CONSTRAINT ck_cl_action CHECK (action IN ('CREATE','UPDATE','DELETE')),
    CONSTRAINT ck_cl_result CHECK (result IN ('SUCCESS','FAILED')),
    CONSTRAINT ck_cl_failed_no_snapshot CHECK (result <> 'FAILED' OR after_data IS NULL)
) PARTITION BY RANGE (created_at);

CREATE TABLE sys_change_log_2026_10 PARTITION OF sys_change_log
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');
CREATE TABLE sys_change_log_default PARTITION OF sys_change_log DEFAULT;

CREATE INDEX idx_cl_target     ON sys_change_log (target_type, target_id, created_at DESC);
CREATE INDEX idx_cl_created    ON sys_change_log (created_at DESC);
CREATE INDEX idx_cl_before_gin ON sys_change_log USING gin (before_data);   -- 必须默认 opclass
CREATE INDEX idx_cl_after_gin  ON sys_change_log USING gin (after_data);

CREATE TABLE sys_change_log_archive (
    LIKE sys_change_log INCLUDING DEFAULTS INCLUDING CONSTRAINTS
) PARTITION BY RANGE (created_at);

-- 归档（实测：秒级，只改元数据）
ALTER TABLE sys_change_log DETACH PARTITION sys_change_log_2026_10;
ALTER TABLE sys_change_log_archive ATTACH PARTITION sys_change_log_2026_10
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');
```

## C.2 代码依赖新增

| # | 依赖 | 位置 | 是否零下载 | 说明 |
|---|---|---|---|---|
| 1 | `markdown-it`（+ `@types/markdown-it`） | 前端 `dependencies` | ❌ 需 pnpm 安装 | 帮助文档渲染（D2） |
| 2 | `dompurify`（+ `@types/dompurify`） | 前端 `dependencies` | ❌ 需 pnpm 安装 | Markdown 渲染后二次净化（4.7） |
| 3 | `highlight.js` | 前端 `dependencies` | ❌ 需 pnpm 安装 | 代码块高亮（只装常用语言，约 80KB gz） |
| 4 | `ammonia = "4.2"` | 后端 `Cargo.toml` | ✅ **零下载**（`Cargo.lock` 已有 4.2.0，F21） | **仅当**决定放开 `target`/自定义白名单时才需要；若接受默认净化则**不用加** |
| 5 | `@wangeditor/editor-for-vue@5` | 前端 | ❌ | **仅 P1**（附录 B2） |

> **P0 最小依赖集 = 前 3 个前端库**。若前端包管理受限（私服缺包），降级方案：帮助文档 P0 用**后端返回已渲染 HTML**（需后端引入 `pulldown-cmark`，同样要下载）——**两条路都要下载，没有"零依赖"选项**。这一条必须在开工前确认（列入交付前提）。

## C.3 环境前提（上线前必须确认，缺一个就要改方案）

| # | 前提 | 现状 | 不满足怎么办 |
|---|---|---|---|
| 1 | `pg_trgm` 扩展可创建 | 底座**无任何** `CREATE EXTENSION`（F34）；容器为官方 `postgres:16-alpine` | 拆成独立迁移文件并容错；搜索退回纯 `ILIKE`（4.5.4），文档 <3000 篇可接受 |
| 2 | 数据库用户有建表 / 建分区权限 | 迁移用同一账号 | 无权限则分区方案退回普通表 + 定期 DELETE（性能与归档体验下降） |
| 3 | 批次 A 的消息模块已合入 | 底座仓库**无 notice 实现**（F35） | 走 3.8.4 的降级 A：公告角标先上，站内信延后 |
| 4 | 前端可安装 3 个新依赖 | 需 pnpm 私服可用 | 见 C.2 的降级说明（转后端渲染） |
| 5 | 定时任务可新增（`sys_job` 或代码内置注册） | 已有 5 个内置清理任务（F15） | 全部改成"代码内置注册"（与现有 5 个同款写法），不依赖 `sys_job` 配置 |

## C.4 性能指标（验收硬指标汇总）

| 场景 | 规模 | 指标 |
|---|---|---|
| 用户端公告列表 | 2000 公告 / 可见 800 | P95 < 150ms |
| 未读数接口 | 同上 | P95 < 30ms |
| 发布全员公告扇出 | 310 人 | < 2s |
| 发布全员公告扇出（极值） | 10,000 人 | < 30s（超时转后台） |
| 公告详情（200KB 富文本） | — | P95 < 100ms |
| 帮助中心首页 | 300 分类 / 5000 文档 | P95 < 200ms |
| 帮助搜索（2 字词，最坏） | 5000 文档 | P95 < 150ms（实测 9.75ms） |
| 帮助搜索（4 字词） | 5000 文档 | < 30ms（实测 0.21ms） |
| 变更日志写入（含 diff + 脱敏） | 单条 | 额外耗时 < 1ms |
| 变更日志列表（按对象） | 在线 300 万行 | P95 < 80ms |
| 变更日志详情 | — | < 15ms |
| 同步导出 1000 行 | — | < 3s |
| 异步导出 10 万行 | — | < 90s |
| 归档搬一个月分区 | 100 万行 | < 10s |

## C.5 兼容与回归范围（**零契约破坏**是前提）

| 受影响模块 | 影响 | 回归要点 |
|---|---|---|
| 批次 A 文件接口 `/api/support/file/*` | **契约零改动**（公告/帮助只引用 `file_id`，F36） | 上传/下载/秒传/私有短链全部回归 |
| 批次 A 消息中心 | 只**调用**其写入能力，不改其表与接口 | 消息中心列表/未读/已读回归（新增 `business_key='content.announcement'` 的消息不影响既有过滤） |
| 批次 B 列脱敏 `shared::field_mask` | 变更日志的脱敏**不复用**它（用 `Mask::*` 直接打码，写入期而非响应期） | 业务页面脱敏回归不受影响 |
| `system/user` 5 个 handler | **唯一侵入式改造**：补事务 + 加 `change_log::record`（5.3.5） | 用户新增/编辑/删除/改状态/分配角色/批量调部门 6 个功能的**行为必须完全不变**（含审计日志仍照旧写） |
| `support/log` 日志页 | **不改**（变更日志是新页面） | 5 个 tab 全部回归 |
| 菜单与路由 | 新增 5 行菜单 + 4 个非菜单路由 | 侧边栏顺序、面包屑、权限过滤回归 |

**必须写回归用例的 6 个接口**（因为改了它们的实现）：`PUT /system/user/{id}`、`POST /system/user`、`DELETE /system/user/{id}`、`PUT /system/user/{id}/status`、`POST /system/user/{id}/roles`、`POST /system/user/batch-dept`。

## C.6 上线顺序与回滚

| 顺序 | 步骤 | 失败回滚 |
|---|---|---|
| 1 | 迁移：建 7 张表 + 字典/菜单种子 + 配置项（**先不含 handler 改造**） | 反向迁移 drop 表（新表无存量数据，安全） |
| 2 | 上线后端：`change_log` 模块 + 公告/帮助/变更日志接口（**新接口，不影响旧功能**） | 下线新接口（旧功能不受影响） |
| 3 | 上线前端：菜单 + 5 个页面 | 菜单 `status=0`（侧边栏消失，数据不丢） |
| 4 | **改造 `system/user` 的 5 个 handler（补事务 + 打点）** ← 唯一有回归风险的一步 | 回退该文件到上一版本（变更日志少数据，不影响业务） |
| 5 | 启动 4 个定时任务 | 停任务（用户端靠谓词兜底，D7） |
| 6 | 观察 24h：`sys_change_log` 行数增长、`security_logs` 是否有 `OPERATION_FAIL` 激增、公告角标接口 P95 | — |

> **第 4 步单独成批发布**（不要和 1–3 混在一个版本里），这样出问题能只回退这一块。

## C.7 实测记录（本机 PostgreSQL 18.6 + rustc 实测，可复现）

| 编号 | 实测内容 | 结果 |
|---|---|---|
| T1 | 全部建表 SQL（含分区表、同构归档表、trgm 索引、CHECK 约束） | ✅ 全部通过，无报错 |
| T2 | 事务内写日志 → `ROLLBACK` | 日志 **0 行** |
| T3 | 事务外写日志 → 业务 `ROLLBACK` | 日志 **1 行**（证明"假日志"风险真实存在） |
| T4 | 插入 `result='FAILED'` + `after_data` 非空 | ✅ 被 `ck_cl_failed_no_snapshot` 拦截（`check_violation`） |
| T5 | 公告可见性（祖先链版）：技术部 / 前端组 / 一组 / 市场部 四个用户 | 分别看到 2 / 3 / 2 / 1 条，**符合预期**；市场部按 ID 直查技术部公告 → **0 行**，技术部用户直查同 ID → 1 行 |
| T5' | 第一版（子树方向）修正记录 | 前端组用户**看不到**父部门公告 → 已改判据方向（3.4.3） |
| T6 | 5000 篇文档中文搜索 | 2 字「请假」：Seq Scan **9.75ms**，命中 63；3 字：Bitmap Index **0.25ms**；4 字：**0.21ms** |
| T7 | 分区路由（10 月 / 11 月） | 各行进各自分区 ✔ |
| T8 | `jsonb_path_ops` vs `jsonb_ops` 对 `?` 操作符 | `path_ops` **不支持**（Seq Scan）；`jsonb_ops` 支持（强制关 seqscan 后走 Bitmap Index Scan） |
| T9 | 从 JSONB 生成 diff（键集合并集 + `IS DISTINCT FROM`） | 输出正确（字段名/旧值/新值/判定） |
| T10 | `DETACH` → `ATTACH` 到同构归档表 | ✅ 成功，归档表立即可查；**不要**重复 `ADD PRIMARY KEY`（会报 multiple primary keys） |
| T11 | ammonia 4.2 净化 13 个载荷 | 见 3.9.2 表（script/iframe/svg/style 全删，style/class/target 属性删除，`javascript:` href 删除并补 `rel`） |

> 复现方式：`psql` 连一个空库跑附录 C.1 的 SQL；XSS 部分用 `cargo` 建一个只依赖 `ammonia = "4.2"` 的小程序跑 `ammonia::clean()`（本机 `Cargo.lock` 已有该 crate，离线可编）。

---

# 附录 D 明确不做（12 条，防止范围蔓延）

| # | 不做 | 理由 | 什么时候做 |
|---|---|---|---|
| 1 | 公告审批流（发布前需主管审批） | 属工作流审批（批次 D） | 批次 D |
| 2 | 内容审核（编辑 ↔ 审核不得同人） | 依赖 D 的审批引擎 + 批次 B 的互斥表 | 批次 D |
| 3 | 公告/文档的评论、点赞、收藏 | 无需求支撑，且评论要先有审核能力（否则是 UGC 风险） | 视需求 |
| 4 | 公告邮件/企业微信/钉钉推送 | 批次 A 只做了站内信，通道路由在批次 A | 批次 A 的通道能力扩展 |
| 5 | 帮助文档多语言 | 当前无外语用户 | 视需求 |
| 6 | 富文本内嵌视频（`<iframe>`） | **净化器会删掉**（F19 实测），做了也存不住 | 若要做需换净化策略（高风险） |
| 7 | 变更日志的"一键回滚/撤销" | 变更历史不是版本库；回滚要处理外键、关联数据、并发，属独立能力 | 独立评估 |
| 8 | 变更日志的"防篡改哈希链" | 需独立设计（附录 B6） | 合规硬要求时 |
| 9 | 变更日志的删除接口 | **产品红线**（5.9 第 5 条） | 永不提供（只归档 + 到期销毁） |
| 10 | 用户端"一键全部已读"公告 | 会让阅读人数失真（3.7.3） | 永不提供 |
| 11 | 配置类（字典/参数/存储配置）的字段级变更日志 | 用 `audit_logs` 足够（5.5.2） | 视合规要求 |
| 12 | 变更日志明细的"逐字段全文对比 + 富文本 diff" | 富文本正文不进日志（5.4.1 白名单），大文本 diff 无人看 | 视需求 |

---

# 附录 E 接口清单与内部契约

## E.1 接口清单（30 个）

**公告 · 管理端（10）**

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 1 | GET | `/api/business/announcement/page` | `business:announcement:list` | 管理端列表 |
| 2 | GET | `/api/business/announcement/{id}` | `business:announcement:list` | 管理端详情 |
| 3 | POST | `/api/business/announcement` | `business:announcement:create` | 新建（草稿） |
| 4 | PUT | `/api/business/announcement/{id}` | `business:announcement:update` | 编辑 |
| 5 | DELETE | `/api/business/announcement/{id}` | `business:announcement:delete` | 删除（软删） |
| 6 | POST | `/api/business/announcement/{id}/publish` | `business:announcement:publish` | 发布（`publish-all` 由服务层二次校验范围） |
| 7 | POST | `/api/business/announcement/{id}/revoke` | `business:announcement:revoke` | 撤回 |
| 8 | POST | `/api/business/announcement/{id}/top` | `business:announcement:top` | 置顶/取消（body `{is_top, top_expire_at}`） |
| 9 | POST | `/api/business/announcement/preview-target` | `business:announcement:create` 或 `update` | 受众预览（无副作用，不写日志） |
| 10 | GET | `/api/business/announcement/{id}/read-stat` | `business:announcement:read-stat` | 阅读明细（**P1**） |

**公告 · 用户端（4）**

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 11 | GET | `/api/business/announcement/portal/page` | `business:announcement:view` | 我的公告列表（含 `tab=all|unread`） |
| 12 | GET | `/api/business/announcement/portal/{id}` | `business:announcement:view` | 详情（**带可见性谓词**） |
| 13 | POST | `/api/business/announcement/portal/{id}/read` | `business:announcement:view` | 标已读（幂等，**带可见性谓词**） |
| 14 | GET | `/api/business/announcement/portal/unread-count` | `business:announcement:view` | 未读数（**带可见性谓词**） |

**帮助 · 分类（4）**

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 15 | GET | `/api/business/help/category/tree` | `business:help:view` **或** `business:help:doc:list`（管理端要读全树，用户端读可见树） |
| 16 | POST | `/api/business/help/category` | `business:help:category:manage` |
| 17 | PUT | `/api/business/help/category/{id}` | `business:help:category:manage` |
| 18 | DELETE | `/api/business/help/category/{id}` | `business:help:category:manage` |

**帮助 · 文档 · 管理端（6）**

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 19 | GET | `/api/business/help/doc/page` | `business:help:doc:list` |
| 20 | GET | `/api/business/help/doc/{id}` | `business:help:doc:list` |
| 21 | POST | `/api/business/help/doc` | `business:help:doc:create` |
| 22 | PUT | `/api/business/help/doc/{id}` | `business:help:doc:update` |
| 23 | DELETE | `/api/business/help/doc/{id}` | `business:help:doc:delete` |
| 24 | POST | `/api/business/help/doc/{id}/publish` | `business:help:doc:publish`（body `{offline: bool}` 复用为下架，或拆成 `/offline`） |

**帮助 · 用户端（3）**

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 25 | GET | `/api/business/help/doc/portal/page` | `business:help:view`（按分类列文档） |
| 26 | GET | `/api/business/help/doc/portal/{id}` | `business:help:view`（正文，下架/不存在 → 404） |
| 27 | GET | `/api/business/help/search` | `business:help:view`（`kw` / `page` / `page_size`） |

**变更日志（3）**

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 28 | GET | `/api/business/changelog/page` | `business:changelog:list` |
| 29 | GET | `/api/business/changelog/{id}` | `business:changelog:detail` |
| 30 | POST | `/api/business/changelog/export` | `business:changelog:export` |

> **没有** `DELETE /api/business/changelog/*`（5.9 第 5 条）；**没有**任何"明文导出"接口（P1 的 `export-plain` 是给**详情页单字段查看**用的读接口，不提供批量明文导出，附录 B6/D8）。

## E.2 表接入契约（后续批次接入变更日志必须遵守）

任何想让"变更可追溯"的业务表，必须满足：

| # | 要求 | 说明 |
|---|---|---|
| 1 | 主键可字符串化 | `target_id` 是 `VARCHAR(100)`；非 UUID 主键也能接 |
| 2 | 写操作在事务内 | 否则无法挂 `change_log::record(&mut tx, …)` |
| 3 | 在 `policies.rs` 注册 `FieldPolicy` | 字段白名单 + 每字段脱敏类型（5.4.1） |
| 4 | 字典补齐 `log_target_type` / `log_operation` | 否则列表显示成裸英文码 |
| 5 | 有"人类可读名称"字段 | 用于 `target_name` 快照（如 `username` / `title`） |

> 接入一个对象的工作量：**半天**（注册 policy + 3 处调用 + 2 条字典）。批次 D 的审批单、ERP 的采购单都按此接入。

## E.3 内部调用契约（供批次 D 与后续批次）

| 能力 | 契约 | 调用方 |
|---|---|---|
| 写变更日志 | `change_log::record(&mut tx, ChangeLogInput) -> Result<()>` | 任何业务 handler（**必须在事务内**） |
| 写失败留痕 | `change_log::record_failure(db, FailureInput)` | 任何业务 handler（**只能在事务 Err 后**） |
| 发站内信 | 批次 A 的 notice 模块公开函数（`business_key/biz_ref_id/action_url/action_text`） | 公告发布（本批已用）、D 批次的审批待办 |
| 挂附件 | `sys_file` 的 `file_id` 关联表（本批 `sys_announcement_file` 即是范例） | D 批次的审批附件 |
| 字段脱敏原语 | `nik_utils::Mask::mobile/email/name/id_card/bank_card`（**禁止 `mask_by_type` 兜底**） | 变更日志、后续任何要打码的模块 |

## E.4 代码评审强制清单（10 条，逐条打勾才能合）

| # | 检查项 | 怎么查 |
|---|---|---|
| 1 | `change_log::record` 出现在 `db().transaction(` 闭包内 | `grep -n "change_log::record" -B5` 看上下文 |
| 2 | 没有在事务内调 `record_failure` | 同上 |
| 3 | 事务内语句顺序：读旧值 → 写业务 → 写日志 | 人工读代码 |
| 4 | 没有用 `enqueue_audit` 代替 `change_log::record` | `grep -n enqueue_audit` / `grep record_audit` |
| 5 | 新增的业务查询都带了"可见性谓词"（用户端公告 4 条路径） | 对照 3.4.2 四条逐条看 |
| 6 | 富文本写入前调用了 `xss::sanitize_html`（或自建 Builder） | `grep -n sanitize_html` |
| 7 | 没有使用 `Mask::mask_by_type`（F38 的中文 panic） | `grep -n mask_by_type` 必须为 0 |
| 8 | 变更日志的字段白名单覆盖了所有可改字段，且**不含** password/token/富文本正文 | 对照 5.4.1 表 + 白名单单测 |
| 9 | 新接口都挂了 `#[nik::permission(...)]` 且权限码与种子一致 | `grep -n "permission ="` + 对照第 6 章 |
| 10 | 没有硬编码颜色（`#RRGGBB` / `rgb(`） | `grep -nE "#[0-9a-fA-F]{6}\|rgb\("` 在新 vue 文件里必须为 0 |

---

**文档完**

> 编制：product-manager ｜ 版本 v1.0 ｜ 事实核查 F1–F38 全部带路径行号 ｜ 数据库实测 T1–T11 在本机 PG 18.6 真跑通过 ｜ 待拍板 6 条见附录 B
