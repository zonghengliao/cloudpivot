# 批次 C · C2「公告（用户侧）」验证证据

> 交付卡：`t_48368bbf`｜执行者：manong｜环境：`172.18.10.144`（ubuntu-svr，Rust 1.99.1，PG 18.6）
> 基线：`57a6762`（C0+C1 `e6a023d` 之上，C5 的 `57a6762` 已合入 `main`）
> 验证库：**`cloudpivot_c2v`**（本卡自建，绝不使用 `cloudpivot` / `cloudpivot_dev` / 共享的 `cloudpivot_test`）
> 验收实例：`profile=test`、`ALUN_DATABASE_NAME=cloudpivot_c2v`、`127.0.0.1:8029`

## 0. 交付清单

| 文件 | 行数 | 说明 |
|------|------|------|
| `backend/migrations/business/V0080__create_announcement.{up,down}.sql` | 107 / 13 | 公告四表（主体 / 范围 / 已读 / 附件），DDL 逐字取自架构 §6 附录 A.1 |
| `backend/migrations/business/V0081__announcement_indexes.{up,down}.sql` | 40 / 11 | 7 个索引（含 3 个部分索引），取自附录 A.2 |
| `backend/migrations/business/V0087__seed_announcement_read_permission.{up,down}.sql` | 81 / 17 | `business:announcement:read` 权限点 + 全员授予 + 迁移期自检 |
| `backend/src/business/announcement/model.rs` | 745 | 可见性查询 / 已读 / 计数 / 附件绑定（+5 单测） |
| `backend/src/business/announcement/dto.rs` | 295 | 请求 / 响应 DTO（+3 单测） |
| `backend/src/business/announcement/handler.rs` | 832 | 用户侧 7 个接口 + 架构别名 + 附件代理下载（+8 单测） |
| `backend/src/business/announcement/mod.rs` | 39 | 模块声明与契约边界（改 `pub mod` 三行 + 文档） |
| `scripts/batchc/migrate_test.sh` | 104 | 迁移往返验证（up / down / verify） |
| `scripts/batchc/c2_accept.py` | 658 | 真机验收（seed / verify / explain） |

**用户侧接口（13 条路由 = 票面 7 条 + 架构 §2.8.1 别名 6 条，实现只有一份）**

| 方法 | 票面路径 | 架构 §2.8.1 别名 |
|------|----------|------------------|
| GET | `/business/announcement/visible` | `/business/announcement/mine/page` |
| GET | `/business/announcement/visible/page` | — |
| GET | `/business/announcement/unread-count` | `/business/announcement/mine/unread-count` |
| GET | `/business/announcement/{id}` | `/business/announcement/mine/{id}` |
| POST | `/business/announcement/{id}/read` | `/business/announcement/mine/{id}/read` |
| PUT | `/business/announcement/read-all` | `/business/announcement/mine/read-all` |
| GET | `/business/announcement/{id}/attachment/{att_id}` | `/business/announcement/{id}/attachments/{file_id}/download` |

> 票面把契约写成「按架构 §2.9」，但 §2.9 是「关键查询与索引验证」，真正的用户侧契约在 **§2.8.1**，且路径是 `/mine/*`。两套路径都注册（同一批内部函数），避免「前端照文档写、后端照票面写」的扯皮。三套路径的一致性由验收脚本末段交叉断言（`total` 相同）。

## 1. 迁移往返（测试库 `cloudpivot_c2v`）

### 1.1 首次应用（全新库）

```
==> 建库 cloudpivot_c2v（owner=cloudpivot）
==> 应用启动（profile=test db=cloudpivot_c2v :8029），让框架执行迁移
INFO alun_web::app: 数据库迁移完成: 应用 48 个迁移
INFO alun_web::app: cloudpivot 启动完成, 耗时 544ms
```

结构断言（`bash scripts/batchc/migrate_test.sh verify`）：

```
==> 四张表
 sys_announcement
 sys_announcement_attachment
 sys_announcement_read
 sys_announcement_scope
(4 rows)

==> 7 个索引
 idx_sys_announcement_attachment_file
 idx_sys_announcement_expire
 idx_sys_announcement_list
 idx_sys_announcement_read_user
 idx_sys_announcement_scope_reverse
 idx_sys_announcement_top_expire
 uk_sys_announcement_no
(7 rows)
```

### 1.2 `down` 回滚

```
==> 执行 down 脚本（逆序：V0087 → V0081 → V0080）
DELETE 6          ← V0087：收回 3 个角色 × 2 个节点的授权
DELETE 1          ← V0087：删读按钮
DELETE 1          ← V0087：删公告中心页面
DROP INDEX ×7     ← V0081
DROP TABLE ×4     ← V0080
==> 清 schema_migrations 记录（等价 rollback 簿记）
DELETE 3
```

回滚后断言（四表 0 行、7 索引 0 行、迁移记录 0 行）：

```
==> 四张表 → (0 rows)
==> 7 个索引 → (0 rows)
==> 迁移记录（80/81/87）→ (0 rows)
==> 权限种子  granted_roles=0  missing_roles=3
```

### 1.3 `up` 重新应用（往返闭合）

```
INFO alun_web::app: 数据库迁移完成: 应用 3 个迁移
WARN alun_db::migrate: 待应用迁移的版本低于已应用最大值（乱序补迁）… version=80 latest_applied=84
WARN alun_db::migrate: 待应用迁移的版本低于已应用最大值（乱序补迁）… version=81 latest_applied=84
```

- 「乱序补迁」告警是**本次手动回滚**造成的假象：C5 的 V0084 已在同库里应用过，我回滚 80/81 再重放，框架自然提示版本倒序。生产库上 V0080→V0081→V0087→V0084 是**升序**应用，不会出现该告警。
- 重放后 `verify` 恢复到 4 表 / 7 索引 / 3 条迁移记录，**且权限种子变成 `granted_roles=3 missing_roles=0`**（此时库里有 3 个角色）——这正是 A-20「种子覆盖所有角色」的直接证据。

### 1.4 关于 V0087 的号段与「全新库无角色」的说明（诚实记录）

- 架构 §5.3 把批次 C 的 20 个权限码集中在 **V0085**，但 C2 的接口注解已经写了 `permission = "business:announcement:read"`；种子缺席会让**除超管外所有人**打开公告栏就 403（用户侧等于没交付）。故按「单用途种子迁移」范式（同 V0068）从预留带取 **V0087**，只种用户侧这 **1 个**码。`V0085` 的全集仍留给 C3/C4/C5，重复 INSERT 会被 `ON CONFLICT DO NOTHING` 吸收。
- 菜单 UUID：本卡占用 `…0700`（公告中心页面）/`…0701`（读按钮）。**C3 的管理侧菜单请从 `…0710` 起排**。
- 全新库上 V0087 早于 `S__role_data`（种子迁移在版本化迁移之后执行），此刻库里还没有任何角色 → 授权语句 0 行，自检 0 缺失（真空通过）。这与批次 A 的 `V0045`（`business:notice:mine` 全员授予）**完全同口径**，属实机核对结论：全新库只有 `SUPER_ADMIN`，而超管本来 `role_code` 短路；非超管角色是运维后来创建的，需要按框架既有语义在「角色 → 分配菜单」里勾选。有角色的库上跑一次 V0087 即全覆盖（见 1.3）。

## 2. A-01 / A-02 / A-03 / A-10（真机 HTTP，`scripts/batchc/c2_accept.py verify`）

夹具：3 层部门树 `C2-HQ → C2-RD → C2-RD-BE`、`C2-SALES`；两个只持读权限的角色 `C2_R1`/`C2_R2`；用户 `c2_rd`(研发部,R1)、`c2_be`(后端组,R2)、`c2_sales`(销售部,R2)、`c2_none`(无角色)；14 条公告覆盖 4 类范围 × 4 种状态 × 有效期 × XSS × 附件。

### A-01 范围判定矩阵

```
  c2_rd     可见 8 条：['C2-A01-ALL','C2-A01-DEPT','C2-A01-ROLE','C2-A10-UNREAD','C2-ATT','C2-ATT-DEPT','C2-XSS-DIRTY','C2-XSS-MD']
  c2_be     可见 9 条：['C2-A01-ALL','C2-A01-DEPT','C2-A01-DEPTSUB','C2-A01-USER','C2-A10-UNREAD','C2-ATT','C2-ATT-DEPT','C2-XSS-DIRTY','C2-XSS-MD']
  c2_sales  可见 5 条：['C2-A01-ALL','C2-A10-UNREAD','C2-ATT','C2-XSS-DIRTY','C2-XSS-MD']
  ✅ ALL：3 人全见
  ✅ ROLE(C2_R1)：c2_rd 命中；c2_be / c2_sales（持 C2_R2）不命中
  ✅ DEPT(C2-RD，含下级 C2-RD-BE)：c2_rd / c2_be 命中；c2_sales 不命中
  ✅ DEPT(C2-RD-BE 单节点)：只有 c2_be 命中，父部门 c2_rd **不命中**（证明「含下级」是发布期展开，不是读取期向上递归）
  ✅ USER(c2_be)：只有 c2_be 命中
```

> 「部门公告其他部门看不到」= `c2_sales` 对 `C2-A01-DEPT` / `C2-A01-DEPTSUB` / `C2-ATT-DEPT` 均不可见，已逐条断言。

### A-02 状态过滤（各断言 0 行）

```
  C2-A02-DRAFT     不在列表里 ✓
  C2-A02-OFFLINE   不在列表里 ✓
  C2-A02-EXPIRED   不在列表里 ✓
```

### A-03 到期双保险

```
  DB 事实：status=PUBLISHED expired=t（故意留成 PUBLISHED，模拟定时任务未跑）
  ✅ 查询侧带 expire_at > now() 兜住了定时任务的最多 10 分钟延迟
```

### A-10 未读数口径

```
  未读接口 announcement_unread=8  列表 total=8  only_unread total=8
  ✅ 三者相等（夹具已清空已读回执）
  POST /{id}/read → HTTP 200 code=0 data={'read_count': 1, 'first_read': True}
  重复 POST（幂等）→ HTTP 200 first_read=False read_count=1（首次 1）
  DB 复核：sys_announcement_read 行数=1（幂等 → 恒为 1）
  标已读后：未读接口=7  only_unread total=7（之前 8）
  PUT /read-all → HTTP 200 code=0 affected=7
  重复 read-all（幂等）→ affected=0
  read-all 后未读数=0
```

## 3. `EXPLAIN` 断言：走 `idx_sys_announcement_scope_reverse`

灌到 POC-8 量级（`sys_announcement=2014` 行、`sys_announcement_scope=6007` 行，跑完即清理）后：

```
=== 单条件反向查找（scope_type + target_id）===
 Bitmap Heap Scan on sys_announcement_scope s (actual time=0.124..0.383 rows=2001.00 loops=1)
   Recheck Cond: (((scope_type)::text = 'ROLE'::text) AND (target_id = ANY (('{…}'::cstring)::uuid[])))
   ->  Bitmap Index Scan on idx_sys_announcement_scope_reverse (actual time=0.112..0.112 rows=2001.00)
         Index Searches: 1
 Execution Time: 0.459 ms

=== 完整可见性查询（列表主查询，LIMIT 20）===
 Limit (actual time=0.652..0.662 rows=20.00 loops=1)
   ->  Index Scan using idx_sys_announcement_list on sys_announcement a
         Index Cond: ((status)::text = 'PUBLISHED'::text)
         Filter: (… AND ((scope_type)::text='ALL' OR ANY (id = (hashed SubPlan 2).col1)
                        OR ANY (id = (hashed SubPlan 4).col1) OR ANY (id = (hashed SubPlan 6).col1)))
         SubPlan 2 → Bitmap Index Scan on idx_sys_announcement_scope_reverse (ROLE, rows=2001)
         SubPlan 4 → Bitmap Index Scan on idx_sys_announcement_scope_reverse (DEPT, never executed)
         SubPlan 6 → Bitmap Index Scan on idx_sys_announcement_scope_reverse (USER,  never executed)
 Execution Time: 0.692 ms
```

与架构 §2.9 的 POC-8 计划形态一致：列表走 `idx_sys_announcement_list`（部分索引 + LIMIT 提前停），三个 `EXISTS` 子计划全部走 `idx_sys_announcement_scope_reverse`，**没有** `Seq Scan on sys_announcement_scope`。

## 4. XSS 验证（重点项）

两种攻击面都验：① **Markdown 内嵌原始 HTML**（`ENABLE_HTML=false` 闸 1/2）；② **库里被手工塞了脏快照**（模拟绕过发布接口改库 → 闸 3 出参再净化）。

```
[Markdown 内嵌 HTML，content_md = '<img src=x onerror=alert(1)>']
  content_html = '&lt;img src=x onerror=alert(1)&gt;\n<p>正文段落。</p>\n'
  ✅ 原始 HTML 被转义成文本节点（ENABLE_HTML=false），浏览器不会解析成元素

[脏快照，content_html = '<p>看图</p><img src="x" onerror="alert(1)"><script>alert(2)</script>']
  content_html = '<p>看图</p><img src="x">'
  ✅ 闸 3（出参前再净化）生效：脏快照里的 onerror / script 全部剥离，合法部分保留
```

关键判据：`content_html` 里**不存在**可执行的 `onerror`（渲染器的转义分支里 `onerror=alert(1)` 只是文本节点内容，不含 `<img` 标签；脏快照分支由 ammonia 直接剥离属性与 `<script>`）。单测 `detail_html_sanitizes_dirty_snapshot` / `detail_html_escapes_raw_html_in_markdown` 把这两条固化为回归。

同时验证了详情的三条错误分支：不可见 → `403/62004`、不存在 → `404/62002`、过期（status 仍 PUBLISHED）→ `403/62005`。

## 5. 附件代理下载（不暴露存储后端）

```
详情里的附件对象 = {'id': 'ea4e39bd…', 'file_id': '4a97c186…', 'file_name': '公告附件 说明.pdf',
                    'file_size': 39, 'mime_type': 'application/pdf', 'sort_order': 0,
                    'download_url': '/api/business/announcement/142d7bd7…/attachment/ea4e39bd…'}
GET …/attachment/ea4e39bd… → HTTP 200
  content-type=application/pdf
  content-disposition=attachment; filename="____ __.pdf"; filename*=UTF-8''%E5%85%AC%E5%91%8A%E9%99%84%E4%BB%B6%20%E8%AF%B4%E6%98%8E.pdf
  content-length=39  x-content-type-options=nosniff  cache-control=private, max-age=300
  字节数=39 前 16 字节=b'%PDF-1.4\n% C2 at'
拿 C2-A01-ALL 的 id 拼 C2-ATT 的附件 id → HTTP 404 code=62003   ← 防「拿别的公告的附件 id」
c2_sales 下载「仅研发部」公告的附件 → HTTP 403 code=62004        ← 可见性先行
```

- 出参里**只有** `/api/business/announcement/…` 代理地址：无 bucket / object key / `X-Amz` / 预签名 URL（单测 `download_url_is_proxy_only` 断言 6 个禁用子串）。
- 恒走服务端流式（`Body::from_stream(ReaderStream)`），**不做** 302 预签名跳转——预签名 URL 在有效期内无身份可下载，而公告可见性会变化（架构 §2.5.3）。
- `text/html` / `application/xhtml` / `svg` 的 MIME 一律降级为 `application/octet-stream`（`download_mime` 单测覆盖），叠加 `Content-Disposition: attachment` 双重防存储型 XSS。

## 6. 单测（16 个新增）与全量回归

```
business::announcement::model::tests::array_literal_sorts_and_dedups                ... ok
business::announcement::model::tests::array_literal_empty_is_valid_array            ... ok
business::announcement::model::tests::audience_params_order_matches_predicate       ... ok
business::announcement::model::tests::visible_predicate_covers_state_expire_and_scope ... ok
business::announcement::model::tests::query_placeholders_match_params               ... ok
business::announcement::dto::tests::download_url_is_proxy_only                      ... ok
business::announcement::dto::tests::keyword_blank_is_none                           ... ok
business::announcement::dto::tests::keyword_escapes_like_wildcards                  ... ok
business::announcement::handler::tests::detail_html_renders_markdown                ... ok
business::announcement::handler::tests::detail_html_sanitizes_dirty_snapshot        ... ok
business::announcement::handler::tests::detail_html_escapes_raw_html_in_markdown    ... ok
business::announcement::handler::tests::detail_html_falls_back_to_markdown_when_snapshot_empty ... ok
business::announcement::handler::tests::download_mime_downgrades_active_content     ... ok
business::announcement::handler::tests::content_disposition_is_attachment           ... ok
business::announcement::handler::tests::visibility_errors_map_to_codes              ... ok
business::announcement::handler::tests::current_user_rejects_bad_subject            ... ok
```

正常路径 + 错误/边界分支都覆盖（空集合、空串快照、空白关键词、非法 `sub`、越权错误码映射、通配符转义）。

全量回归（C5 已合入的树上）：

```
running 415 tests
test result: ok. 415 passed; 0 failed; 0 ignored        ← C1 的 375 + 本卡 16 + C5 的 24
test result: ok. 1 passed   (tests/batchB_b2 …)
test result: ok. 1 passed   (tests/batchB_b7_mutex …)
test result: ok. 8 passed   (tests/batchC_c5_changelog …)
test result: ok. 7 passed   (tests/…)
test result: ok. 2 passed   (tests/…)
test result: ok. 0 passed; 7 ignored   （需外部库的集成测试，缺省 SKIP）
```

## 7. 三道检查（`bash scripts/check.sh`，EXIT=0）

```
==> 后端：cargo fmt --check        （通过）
==> 后端：cargo clippy -- -D warnings   Finished `dev` profile … （零告警）
==> 后端：cargo check             Finished `dev` profile …
==> 前端：pnpm typecheck          vue-tsc --noEmit（零错误）
==> 批次B 闸①：分页必须走 scope::paginate   PASS：0 处
==> 批次B 闸②：登记表查询接入 scope:: 或 SCOPE-EXEMPT  PASS：0 处
==> 批次B 门禁：全部通过
==> 全部检查通过
```

- 闸①：新增列表没有直接调 `paginate_with_params`，走 `scope::paginate_unscoped(ScopeTarget::unscoped("sys_announcement"))`（架构 §2.3.3 结论：公告不属数据权限范畴）。
- 闸②：`model.rs` 里查了 `sys_user` / `sys_user_role` / `sys_role`（受众身份解析），命中「登记表查询必须接入 scope:: 或写 SCOPE-EXEMPT」的口径——文件内既有 `scope::` 真实调用，也在需要豁免的查询上写了 `SCOPE-EXEMPT` 理由。

## 8. 实现要点与踩坑记录（可复用）

1. **可见性谓词只写一份**：`model::VISIBLE_PREDICATE` 被 5 条查询共用（列表 / 未读数 / 详情 / 已读校验 / 附件）。四处各写一遍就会出现「列表看不到但未读数算进去了」的静默不一致，A-10 的交叉断言就是为了钉死这一点。
2. **绑定参数顺序 = SQL 文本里 `?` 出现的先后**，不是「语义顺序」：`check_visibility` 的 `scope_ok` 在 **SELECT 投影**里，占位符排在 FROM/JOIN 之前。第一版按语义顺序绑 → 运行期 `error serializing parameter 0`（把 uuid 绑给了 `::text::uuid[]`）。已加单测 `query_placeholders_match_params` 固化正确顺序。
3. **`receiver_count = 0` 时写已读会撞 `ck_sys_announcement_counts`**：已读路径一律 `read_count = LEAST(receiver_count, read_count + 1)`。
4. **`sys_announcement` 的受众部门集合 ≠ `ScopedPrincipal.dept_ids()`**：前者是「我在哪个部门」（组织归属），后者是数据范围（`CUSTOM`/`SELF_ONLY` 会被改写）。用数据范围去匹配公告受众会让「自定义范围只配了 A 部门的主管」看不到发给本部门的公告。故新增轻量 `Audience`（`sys_user.dept_id` + 启用未过期的角色绑定）。
5. **别名路径与 `{id}` 捕获段共存**：axum/matchit 里静态段优先，`/visible`、`/unread-count`、`/mine/*` 与 `/{id}` 同时注册不会 panic（已实测 13 条路由全部可达）。
6. **附件下载不能直接复用 `business/storage/handler.rs` 的私有函数**（`download_response` / `resolve_file_store` / `content_disposition` 都是私有），本卡不动批次 A 的模块，故按同一口径在 announcement 内实现，并保留「`storage_code IS NULL` ⇒ 按本地默认目录读」的兼容语义。

## 9. 未做 / 留给后续卡

- **管理侧 10 个接口**（含 `POST /preview`、发布/下线状态机、扇出与补偿、附件绑定）→ **C3**。本卡不写 `content_html`（写路径在 C3 的发布事务里），但读取侧对「快照缺失」做了从 `content_md` 现渲染的兜底。
- **`GET /business/announcement/mine/badge`**（通知 + 公告聚合角标，架构 §2.7）→ 不在 C2 票面的 7 条接口内，留给前端卡按需补；它需要读 `sys_notice_receiver`，跨模块口径应由 C4/C5 统一。
- **`V0085`（批次 C 20 个权限码全集）/ `V0086`（`announcement` 存储分类策略 + 5 个 `sys_config` 参数）** → 仍按 §5.3 留给 C3/C4/C5。**C3 的公告菜单请从 UUID `…0710` 起排**，避免和本卡 `…0700/…0701` 撞号。
