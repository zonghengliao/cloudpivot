# 批次 C · C0 + C1 验证证据 —— 底座 4 项 + Markdown 渲染与净化（ENABLE_HTML=false）

- 任务卡：`t_e5a01297`【批次C·C0+C1】底座4项 + Markdown渲染与净化(ENABLE_HTML=false)
- 依据：`docs/批次C-技术架构.md` §5.4.1（B-1~B-4 报批理由）、§5.2（62xxx 码表）、
  §2.4.3（XSS 四道闸与固定参数）、§3.2（TOC 锚点与 POC-13）、§5.5.1 A-13~A-16、§5.5.2 H-10~H-12、§7 C0/C1 行
- 代码基线：`dfbe469`（批次 A/B 全绿：30 表 / V0068 / 373 tests / 门禁 0/0）
- 功能提交：`410f84e`（10 文件 / +1485 行）
- 验证机：`172.18.10.144`（Rust 1.99.1 / cargo 1.99.1 / PG 18 / 独立实例 `127.0.0.1:8024`，profile=dev）

---

## 1. 交付物清单

| 文件 | 行数 | 职责 |
|---|---|---|
| `backend/src/business/shared/content.rs` | 1224 | **C1 主体**：渲染 + 三套净化配方 + TOC 树 + 12 个单测 |
| `backend/src/business/announcement/mod.rs` | 22 | C0 空壳（C2/C3 的模块入口，含两条不变量） |
| `backend/src/business/help/mod.rs` | 18 | C0 空壳（C4 的模块入口，含两条不变量） |
| `backend/src/business/changelog/mod.rs` | 26 | C0 空壳（C5/C6 的模块入口，含三条不变量） |

底座改动（`git show --stat 410f84e`，**改动面严格受控**）：

| 文件 | 改动 | 是否在架构 §5.4.1 报批清单内 |
|---|---|---|
| `backend/Cargo.toml` | +15 行（`pulldown-cmark` 新增 + `ammonia` 显式化） | ✅ B-1 |
| `backend/Cargo.lock` | +20 行（`pulldown-cmark 0.13.5` / `pulldown-cmark-escape 0.11.0`） | ✅ B-1 的锁文件 |
| `backend/src/shared/error_code.rs` | +83 行（13 个 62xxx 码 + 1 个码位自检单测） | ✅ B-3 |
| `backend/src/business/mod.rs` | +6 行（3 个 `pub mod` + 文档表 3 行） | ✅ B-4 |
| `backend/src/task/mod.rs` | +69 行（`BATCH_C_TASKS` 5 条 + 注册函数 + 文档表 5 行） | ✅ B-2 |
| `backend/src/business/shared/mod.rs` | **+2 行**（`pub mod content;` + 文档表 1 行） | ⚠️ 卡里没列，属**必需的接线**：不声明 `pub mod content` 新文件不参与编译。只加了这一行声明，`scope.rs` 一行未动。 |

> 红线自查：`shared/scope.rs` **未改动**（`git show --stat 410f84e` 里没有它）；`support/log`、
> `business/storage`、`business/notice`、`middleware/*` 均未触碰。

---

## 2. C0 四项落地证据

### 2.1 B-1 `Cargo.toml`：`pulldown-cmark` 新增 + `ammonia` 显式化

```toml
# Markdown 渲染（批次 C · C0，报批 §5.4.1 B-1，本批**唯一新增的非 nik 依赖**）
pulldown-cmark = { version = "0.13", default-features = false, features = ["html"] }

# HTML 净化（批次 C · C1，与上一行同批落地）：本 crate 本就在依赖树中
# （nik 的 `xss` feature → nik-utils → ammonia 4.2.0，见 Cargo.lock），
# 这里只是把既有的间接依赖显式化，不引入新 crate、不改版本。
ammonia = "4.2"
```

**为什么必须显式声明 `ammonia`**（报批理由的补充，不是随手加的）：

- `alun_utils::xss::sanitize_html` 只有「默认规则」，而帮助文档必须
  `Builder::add_generic_attributes(&["id"])` 才能保住 heading 锚点 —— POC-13 实测默认规则把
  `id` **静默剥离**（`<h2>维护时间</h2>`），TOC 跳转全线失效；
- `nik` 只 `pub use alun_utils::xss;`（`nik-0.4.2/src/lib.rs:1268-1270`），**没有**再导出
  `ammonia` 本身，拿不到 `Builder`；
- 版本与 `Cargo.lock` 已锁的 4.2.0 合并，依赖树里仍然**只有一份** ammonia：
  `cargo build` 输出只有 `pulldown-cmark` / `pulldown-cmark-escape` 两个新 crate。

**`default-features = false, features = ["html"]` 的原因**：默认 feature 是
`["getopts", "html"]`；`getopts` 是它自带 CLI 用的，服务端不需要。第一次写成
`default-features = false` 时编译报 `no 'html' in the root`（`pub mod html` 被 feature 关掉），
补回 `html` 后通过 —— 这条已写进 `Cargo.toml` 注释。

### 2.2 B-3 `shared/error_code.rs`：13 个 62xxx 码

```
$ grep -cE "= 620[0-9]{2}" backend/src/shared/error_code.rs
13
```

| 码 | 常量 | HTTP | 文案 |
|---|---|---|---|
| 62001 | `ANNOUNCEMENT_CONTENT_INVALID` | 400 | 公告标题/正文格式或长度不合法 |
| 62002 | `ANNOUNCEMENT_NOT_FOUND` | 404 | 公告不存在或已删除 |
| 62003 | `ANNOUNCEMENT_ATTACHMENT_NOT_FOUND` | 404 | 附件不存在或不属于该公告 |
| 62004 | `ANNOUNCEMENT_FORBIDDEN` | 403 | 无权访问该公告 |
| 62005 | `ANNOUNCEMENT_NOT_AVAILABLE` | 403 | 公告未发布或已过期 |
| 62006 | `ANNOUNCEMENT_SCOPE_INVALID` | 400 | 发布范围为空或超过 500 个对象 |
| 62007 | `ANNOUNCEMENT_TOP_INVALID` | 400 | 置顶必须指定未来到期时间 |
| 62008 | `ANNOUNCEMENT_EXPIRE_INVALID` | 400 | 有效期不合法（须晚于发布时间） |
| 62009 | `ANNOUNCEMENT_STATUS_CONFLICT` | 409 | 已发布公告不可直接编辑，请先下线 |
| 62010 | `HELP_CATEGORY_DEPTH_EXCEEDED` | 422 | 分类层级超过 3 层 |
| 62011 | `HELP_CATEGORY_NOT_EMPTY` | 409 | 分类下仍有文档，不可删除 |
| 62012 | `CHANGE_LOG_RESOURCE_UNREGISTERED` | 500 | 变更日志：资源未登记白名单（接入缺陷） |
| 62013 | `CHANGE_LOG_TIME_INVALID` | 400 | 变更时间不合法（未来时间或早于保留期） |

配套单测 `shared::error_code::tests::batch_c_error_codes_are_stable`：逐条断言码值、
断言 13 个不重复、断言 `62001 > 61012`（与批次 B 段不重叠）。
**这条测试的目的不是测代码，而是防止 C2/C4/C6 顺手改码值**（前端文案映射按此段规划）。

### 2.3 B-4 `business/mod.rs`：3 个 `pub mod`（空壳）

```rust
pub mod announcement;
pub mod changelog;
pub mod help;
pub mod impexp;
...
```

**命名口径说明（与任务卡文字的一处差）**：卡里写空壳名 `change_log`，架构 §5.4.1 B-4 与
§4.2/§4.12 的文件路径（`business/changelog/registry.rs`、`business/changelog/tests`）都是
`changelog`。取 `changelog` —— 下游 C5/C6 按架构路径建文件，两处不一致会让 C5 建出一个
`changelog/` 再被 `mod.rs` 指向 `change_log`，编译直接失败。模块文档里也写明了这个取舍。

### 2.4 B-2 `task/mod.rs`：5 个定时任务（空实现）

```rust
const BATCH_C_TASKS: [(&str, &str, &str); 5] = [
    ("announcement_auto_expire",     "0 5 0 * * *",   "系统任务：公告到期自动下线"),
    ("announcement_top_expire",      "0 10 0 * * *",  "系统任务：公告置顶到期取消"),
    ("announcement_dispatch_retry",  "0 */10 * * * *","系统任务：公告扇出补偿重试"),
    ("changelog_partition_maintain", "0 20 1 1 * *",  "系统任务：变更日志分区预建与清理"),
    ("help_version_cleanup",         "0 40 2 * * *",  "系统任务：帮助文档历史版本清理"),
];
```

注册点：`Scheduler::register_builtin()` 内 `self.register_batch_c_placeholders();`，
走 `register_cron`，**没有**写进 `bootstrap.rs`（框架 `start()` 取一次性快照，`reload()` 会把
`bootstrap.rs` 里的注册清空 —— B 批次已实测）。任务体是空实现：只 `tracing::info!` 一条
「占位执行」，不碰数据库。

**启动日志原文**（`timeout 45 ./target/debug/cloudpivot`，profile=dev，`127.0.0.1:8024`）：

```
INFO cloudpivot::task: 批次C内置任务已注册（空实现） invoke_target="announcement_auto_expire" cron="0 5 0 * * *"
INFO cloudpivot::task: 批次C内置任务已注册（空实现） invoke_target="announcement_top_expire" cron="0 10 0 * * *"
INFO cloudpivot::task: 批次C内置任务已注册（空实现） invoke_target="announcement_dispatch_retry" cron="0 */10 * * * *"
INFO cloudpivot::task: 批次C内置任务已注册（空实现） invoke_target="changelog_partition_maintain" cron="0 20 1 1 * *"
INFO cloudpivot::task: 批次C内置任务已注册（空实现） invoke_target="help_version_cleanup" cron="0 40 2 * * *"
INFO alun_plugin::scheduler: 定时任务 announcement_auto_expire [0 5 0 * * *] 已排程
INFO alun_plugin::scheduler: 定时任务 announcement_top_expire [0 10 0 * * *] 已排程
INFO alun_plugin::scheduler: 定时任务 changelog_partition_maintain [0 20 1 1 * *] 已排程
INFO alun_plugin::scheduler: 定时任务 announcement_dispatch_retry [0 */10 * * * *] 已排程
INFO alun_plugin::scheduler: 定时任务 help_version_cleanup [0 40 2 * * *] 已排程
INFO alun_plugin::scheduler: 定时任务插件: 16 个任务已启动
```

> 5 个任务名全部出现在「已注册」与「已排程」两类日志里，cron 均被框架解析成功
> （`16 = 原有 11 + 本卡 5`：日志清理 5 + 存储 1 + 导出 1 + 回收 1 + 通知 3 + 本卡 5）。
> 启动期 `ERROR` 计数 = 0，迁移「应用 22 个迁移」，无 panic。

---

## 3. 验收命令级结果（C0）

| 验收项 | 命令 | 结果 |
|---|---|---|
| 编译 | `cargo build` | `Finished dev profile`（0 error / 0 warning） |
| 单测不回归 | `cargo test` | **386 passed / 0 failed**（基线 **373**，+13 = 12 content + 1 码表） |
| 13 个码 | `grep -cE "= 620[0-9]{2}" src/shared/error_code.rs` | `13` |
| 5 个任务名 | `cargo run` 启动日志 | 见 §2.4（「已排程」5 条 + 「已启动」共 16 个任务） |
| 门禁 | `bash scripts/check.sh` | **EXIT=0**，闸①=0 / 闸②=0（B0 基线 20/21） |

`cargo test` 分段明细（本次）：

```
lib 单测          375 passed / 0 failed      ← 基线 362 + 13
tests/b2 scope      1 passed
tests/b7 mutex      1 passed
tests/impexp        7 passed
tests/worker        2 passed
doctests            6 ignored（基线相同）
合计               386 passed / 0 failed
```

`bash scripts/check.sh` 末尾原文：

```
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> 全部检查通过
```

（`cargo fmt --check` / `clippy --all-targets -D warnings` / `cargo check` / `pnpm typecheck` 全过；
clippy 抓到过两条：`print_literal` 与 `assertions_on_constants` —— 后者改成模块顶层
`const _: () = assert!(!ENABLE_HTML, ...)`，顺手把红线做成了**编译期**约束。）

---

## 4. C1：12 个单测输出（原文，`cargo test content -- --nocapture`）

```
[c1_01] # → <h2 id="h-2-1">维护时间</h2> ✓
[c1_02] 三个同名 `##` → ["h-3-1", "h-3-2", "h-3-3"] ✓
[c1_03] 公告侧 <h2>值班安排</h2>
[c1_04] <div onclick="alert(1)">点我</div> → &lt;div onclick="alert(1)"&gt;点我&lt;/div&gt; ✓
[c1_05] script → &lt;script&gt;alert(1)&lt;/script&gt;
[c1_06] 外链图片 → <p>看图：[图片已移除]</p> ✓
[c1_07] 站内图片 → <p><img src="/api/business/storage/file/abc.png" alt="拓扑"></p> ✓
[c1_08] javascript: 链接 → <p>点我</p> ✓
[c1_09] file: 降级、https 保留 → <p>读文件 与 <a href="https://example.com/a" rel="noopener noreferrer">外链</a></p> ✓
[c1_10] TOC 顶层 1 项，二级 2 项，三级/四级逐层嵌套 ✓
[c1_11] 纯文本 = "标题\n加粗 与 代码 与 链接ab12" ✓
[c1_12] help 保留 language-rust / sql；announcement 只保留 language-sql ✓

test result: ok. 17 passed; 0 failed; 0 ignored  ← 12 个 content 单测 + 5 个名字含 content 的既有测试
```

任务卡要求逐条对应的断言：

| 卡上的断言 | 对应单测 | 实测输出 |
|---|---|---|
| `<h2 id="h-2-1">`（heading 偏移 + id） | `c1_01` | `<h2 id="h-2-1">维护时间</h2>` |
| `content_md` 里 `<div onclick>` → `&lt;div` | `c1_04` | `&lt;div onclick="alert(1)"&gt;点我&lt;/div&gt;` |
| `![](http://evil/x.png)` → `[图片已移除]` | `c1_06` | `<p>看图：[图片已移除]</p>`，输出里无 `evil`、无 `<img>` |
| `<script>alert(1)</script>` 不产生可执行标签 | `c1_05` | `&lt;script&gt;alert(1)&lt;/script&gt;` |
| `javascript:` 链接被移除 | `c1_08` | `<p>点我</p>`（`<a>` 消失、文字保留） |
| TOC 层级正确（h2/h3/h4 嵌套） | `c1_10` | 顶层 1 → 二级 2 → 三级 1 → 四级 1 |

---

## 5. 安全设计要点（闸 1 / 闸 2）

1. **`ENABLE_HTML = false` 的真实实现方式**（重要，与架构措辞的落地差异）：
   `pulldown-cmark 0.13`（含 0.13.5）**没有** `Options::ENABLE_HTML` 这一位 ——
   `Options` 只有 `ENABLE_TABLES/FOOTNOTES/STRIKETHROUGH/TASKLISTS/SMART_PUNCTUATION/
   HEADING_ATTRIBUTES/METADATA/MATH/GFM/...`。原始 HTML 一定会以 `Event::Html` /
   `Event::InlineHtml` 进入事件流，而 `html::push_html` 会**原样写出**（那就是 XSS 入口）。
   所以「关闭 HTML」落在 `Walker::handle` 里：`ENABLE_HTML == false` 时把 HTML 事件
   降级成**文本事件**，转义交给 `push_html` → 输出 `&lt;div`。常量本身加了两道锁：
   - 模块文档写明「红线，不得改为 true」；
   - `const _: () = assert!(!ENABLE_HTML, ...)` —— **改成 true 直接编译失败**。

2. **heading 偏移与锚点**：`shift_level()` 把 `H1..H6` → `2..=6`（`H6` 封顶 6），
   `id = h-{偏移后层级}-{同层序号}`，序号计数器按偏移后层级独立递增。所以：
   - 正文 `#` → `<h2 id="h-2-1">`（卡上的断言）；
   - 正文 `##` → `<h3 id="h-3-1">`；
   - 三个同名 `##` → `h-3-1/2/3`（不重复）。
   `id` 完全由本模块生成（`h-数字-数字`），**不透传用户输入**；原始 HTML 已被转义，
   所以 `[x](...)` 里塞 `id="location"` 之类的 DOM clobbering 载荷进不来。

3. **三套配方差异**（`Recipe`）：

   | | 图片 | 链接 | 表格 | 代码语言 | heading `id` |
   |---|---|---|---|---|---|
   | `Announcement` | 仅 storage 前缀 | 协议白名单 | 允许 | 14 种（运维/配置） | **剥离**（公告无目录） |
   | `Help` | 仅 storage 前缀 | 协议白名单 | 允许 | 14 + 30 种（技术文档） | **保留** |
   | `PlainText` | 全部移除 | 展平为文字 | 只留文字 | 只留代码文字 | 不产生，`toc` 恒空 |

4. **图片**：只允许 `/api/business/storage/` 与 `/api/business/announcement/` 前缀
   （架构 §2.4.3 加固项），其余一律替换为 `[图片已移除]` 且计数入 `Rendered::removed_images`。
   注意此时**整段 `Start(Image)..End(Image)` 被丢弃**，`alt` 文本不会漏进正文。

5. **链接**：`http/https/mailto` + 站内相对路径（`/`、`#`、`?`、无 schema）保留；
   `javascript:` / `file:` / `data:` 其余一律**丢 `<a>` 留文字**（比 ammonia 只删 `href`
   更彻底：不会留下 `<a>` 空壳），计数入 `Rendered::removed_links`。

6. **ammonia 的两个坑（实测，已写进代码注释）**：
   - 声明 `a` 的 `target` 可以，但**不能**同时声明 `rel`，否则 4.2.0 panic
     （`assert!(tag_attributes["a"]["rel"].is_none())`，lib.rs:1889）；`rel="noopener
     noreferrer"` 由净化器自动补（实测输出已带）。
   - 要保留 `<code class="language-x">` **只能**用 `add_allowed_classes("code", ...)`；
     若同时把 `class` 写进 `tag_attributes`/`generic_attributes`，4.2.0 直接 panic
     （`assert!(tag_attributes[tag]["class"].is_none())`，lib.rs:1898）。所以类名全部是
     `&'static str` 字面量（`language_class()`），没有 `format!` 开销。
   - `id` 属于 `generic_attributes`（默认只有 `lang`/`title`），`Help` 配方显式
     `add_generic_attributes(&["id"])`；`Announcement` 不加 → id 被剥（断言 `!contains("id=")`）。

7. **闸 2 / 闸 3 的入口**：`render()` 内部已净化（写时）；出参再净化用
   `sanitize_html(&row.content_html, recipe)`（幂等，POC-3 已验证）。C2/C4 直接用，
   不要再手拼 `ammonia::Builder`。

---

## 6. 已知边界（C2 / C4 / C6 接手时必须知道）

| # | 事项 | 说明 |
|---|---|---|
| 1 | 任务列表的 checkbox 会被剥掉 | `ENABLE_TASKLISTS` 已开，但 pulldown 产出 `<input type="checkbox">`，ammonia 默认白名单没有 `input` → 只剩 `li` 文字。若产品要真 checkbox，C2 在配方里 `add_tag("input")` + `add_tag_attributes("input", &["type","checked","disabled"])`，属于放宽白名单，**要单独评审**。 |
| 2 | `plain_text` 不转义尖括号 | 它是给检索 `snippet` / 摘要用的原始文本。H-10 要求 `snippet` 里不出现 `<`/`>`，C4 出参时对 `snippet` 再做一次 HTML 转义（或直接用 `Recipe::PlainText` 的 `content_html`，那份是转义的）。 |
| 3 | 公告侧没有 heading `id` | 按 POC-13 与 §2.4.3「输出前去掉 id」，公告配方不保留 id。若将来公告也要 TOC，改成 `Recipe::Help` 即可（一行）。 |
| 4 | 代码语言白名单是封闭集合 | 新增语言要同时改 `*_CODE_LANGS` 与 `language_class()`（后者是 `allowed_classes` 的静态字面量来源），漏一处就会被剥类名。 |
| 5 | 5 个定时任务目前是空实现 | C3/C4/C6 落地时把 `register_batch_c_placeholders` 里的空 `tokio::spawn` 换成各模块 `run_once()`，并把注册搬进模块 `register_builtin`（与 `notice::cleanup` 同构），同时删掉 `task/mod.rs` 里的占位函数。 |

---

## 7. 本卡边界（未做的事）

- 没有接口、没有迁移（V0080–V0086 属 C2/C4/C5/C6/C8）、没有前端（C9）、没有权限种子；
- 没有改 `shared/scope.rs`（其内容按架构 §5.4.3 保持不动），公告/帮助/变更日志三张表
  **有意不登记** `SCOPED_TABLES` 的说明留给 C2 的 S-1 注释；
- 没有动 `support/log`、`business/notice::dispatch`、`business/storage`、`middleware/operation_log`。
