# P1-4 业务单据附件（合同扫描件 / 签收单 / 发票影像）— 交付证据

- **卡号**：`t_fd2209e2`（【P1-4】业务单据附件）
- **依据**：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §6 R-P1-3（断链 **DC-17**）+ §3.11 维度⑩
- **落点**：`biz_doc_attachment` 桥接表（迁移 **V0212**）+ `business/attachment` 四件套（4 个端点）+ 四个单据详情页的「附件」区
- **日期**：2026-11-03

---

## 1. 改造前 / 改造后

| | 改造前 | 改造后 |
|---|---|---|
| 合同扫描件 | 用户存自己电脑；出事找不到 | `POST /api/business/attachment/upload`（doc_type=CONTRACT）+ 合同详情「附件」区 |
| 收货签收单 | 同上 | 收货单详情「附件」区（默认角色 `SIGN_RECEIPT`） |
| 付款/发票影像 | 同上 | 应付（付款）详情「附件」区（默认角色 `INVOICE`） |
| 文件本体 | —— | **完全复用** `business::storage`（策略 / 秒传 / 自建后端路由 / 流式下载 / 7 天物理回收） |

ARCH-3 的判据是「ERP 30 张表对 `sys_file` 的引用数 = 0」；本次之后 `biz_doc_attachment.file_id`
是 ERP 侧第一条指向 `sys_file` 的引用（`file_id → sys_file.id`，`ON DELETE RESTRICT`）。

---

## 2. 数据模型

```sql
CREATE TABLE biz_doc_attachment (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_type    varchar(32) NOT NULL,     -- CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT
    doc_id      uuid        NOT NULL,     -- 单据主键（**应用层**校验存在性，见下）
    file_id     uuid        NOT NULL REFERENCES sys_file(id) ON DELETE RESTRICT,
    file_role   varchar(32) NOT NULL DEFAULT 'OTHER',  -- CONTRACT_SCAN / SIGN_RECEIPT / INVOICE / OTHER
    remark      varchar(255),
    uploaded_by uuid REFERENCES sys_user(id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    deleted_at  timestamptz
);
-- 索引：idx_biz_doc_attachment_doc (doc_type, doc_id) WHERE deleted_at IS NULL
--       idx_biz_doc_attachment_file (file_id)
--       uk_biz_doc_attachment UNIQUE (doc_type, doc_id, file_id) WHERE deleted_at IS NULL
```

**`doc_id` 为什么没有外键**：五类单据在五张不同的表（`biz_contract` / `biz_purchase_order` /
`biz_stock_in` / `biz_payable` / `biz_payment`），单列无法同时指向；也避免「加一类单据就要
改表结构」。存在性由 handler 在写库前用 `model::doc_exists()` 逐类校验（**含 `deleted_at IS NULL`**），
找不到 → `404`，不会写出悬空附件。表名只来自 Rust 常量白名单（`DocKind::table()`），不拼外部字符串。

---

## 3. 接口契约（4 个端点）

| 方法 | 路径（前端写全 `/api`） | 路由权限码 | 行级判据 |
|------|--------------------------|------------|----------|
| POST | `/business/attachment/upload` | `business:attachment:upload` | —— |
| GET | `/business/attachment/list?doc_type=&doc_id=` | `business:attachment:list` | —— |
| DELETE | `/business/attachment/{id}` | `business:attachment:upload` | 上传人本人 ∪ `business:attachment:delete` |
| GET | `/business/attachment/download/{id}` | `business:attachment:list` | —— |

- **upload** 是 multipart：`doc_type` / `doc_id` / `file_role`（可缺省=OTHER）/ `remark`（≤200 字）/ `file`。
  校验顺序：`Content-Length` 硬顶（读流前）→ 字段白名单 → 单据存在性（404）→ 单张单据 50 个上限 →
  storage 策略（分类 `attachment`：100 MB、办公文档/图片/压缩包白名单）→ 秒传 → 落 `sys_file` → 插绑定行。
- **list** 出参：`doc_type` / `doc_id` 回显 + `list` + `total` + `max_count`(50)；
  每条带 `file_role_label`（中文，后端下发）与 `can_delete`（前端按钮只认它）。
- **delete** 只软删绑定行；**仅当这份文件没有别的单据在用时**才顺带软删 `sys_file`
  （秒传会让多单据共用一条 `sys_file`，无脑删文件记录会让别人的附件 404）。
- **download** 走 `storage::handler::download_response`（内部可信路径）：`LOCAL` 流式、
  `S3` 302 预签名、`Content-Disposition: attachment`、`X-Content-Type-Options: nosniff`。

### 路由权限码为什么 delete 挂 `:upload` 而不是 `:list`

`system::authz_tests::o13_every_write_route_requires_permission` 明令**写接口不许挂 `:list`/`:detail`**
（「有权查看即可改数据」是明确漏洞）。本卡第一版把 DELETE 挂在 `:list` 上，全量套件直接红（真实抓到）：

```
写接口只要求了读权限（有权查看即可改数据）：
DELETE /business/attachment/{id} → business:attachment:list
```

改法：路由层挂写侧码 `:upload`（「能上传的人应当能撤销自己的上传」），删**别人**的附件再由
行级码 `:delete` 判 —— 两个码各管一件事，且都不可绕过。

---

## 4. 前端（四个详情页）

组件 `frontend/src/views/business/erp/shared/DocAttachmentPanel.vue`（拖拽 / 点击选择 / 角色下拉 /
备注 / 下载 / 删除，交互照任务详情页的协作区附件）：

| 页面 | 抽屉 testid | doc_type | 默认角色 |
|------|-------------|----------|----------|
| 合同详情 | `contract-detail` | `CONTRACT` | `CONTRACT_SCAN` |
| 采购单详情 | `purchase-detail-drawer` | `PURCHASE_ORDER` | `OTHER` |
| 收货单详情 | `stockin-detail-drawer` | `STOCK_IN` | `SIGN_RECEIPT` |
| 应付（付款）详情 | `payable-detail` | `PAYABLE` | `INVOICE` |

API 层：`frontend/src/api/business/erp/attachment.ts`（路径写全 `/api/...`；下载走 `responseType: 'blob'`）。

---

## 5. 验证记录

| 验收标准 | 证据文件 | 结果 |
|----------|----------|------|
| 迁移 up/down 通过 | `raw/06-migration-up-down-up.txt` | 3 轮往返全过，down 后 表=0 / 权限=0 / 授权=0 |
| 表/约束/索引/权限落库 | `raw/01-seed-and-structure.txt` | 1 表 / 3 索引 / 2 CHECK / 2 FK / 3 权限节点 |
| 单测 ≥8 条（上传/列表/删除/超权限/文件不存在/doc 不存在） | `raw/04-attachment-unit-tests.txt` | **15 条全过**（8 条纯逻辑 + 7 条真库） |
| 真机：合同上传→列表→下载→删除 | `raw/02-real-machine-main.txt` | **38 项断言全过** |
| 真机：采购/收货/付款各传一个都能看到 | 同上（④） | 3 类各 1 条，文件名/角色逐个核对 |
| 附件不丢：重启后还能下载 | `raw/03-real-machine-after-restart.txt` | 7 项全过（列表仍在 + 字节一致 + 已删的不复活） |
| 不变量全绿 | `raw/07-invariants.txt` | **40/40**，BLOCKER 0 / WARN 0 |
| 全量后端用例 | `raw/05-full-lib-suite.txt` | `test result: FAILED. 2202 passed; 2 failed` —— 2 条失败是 `contract_approval_tests::large_amount_requires_two_levels_in_sequence` / `small_amount_single_level_approval_signs_contract`，即**真库用例并发互踩**（整期 UPDATE + `zz_teardown` 抢行）的已知缺陷；同一套件在 `-j 1 --test-threads=1` 下两条各自单跑均通过（见 DEPLOY-C 回归记录），已单独开卡 `t_0d7c51f8` 修复。本卡 15 条附件用例全在内且全过 |
| 前端类型检查 | `raw/08-frontend-typecheck.txt` | `pnpm typecheck` 无输出（通过） |
| 截图 ≥4 张 | `01-contract-attachments.png` … `05-payable-attachments.png` + `ui-drawers.txt` | 5 张，四类单据各一张 + 界面上传前后各一张 |

### 复验（2026-11-03 22:19–22:32，用**当次重新编译的** `target/debug/cloudpivot` 重跑）

前一轮 20:37 的证据在 `handler.rs` 最后一次修改（21:02）之前，因此本轮把「格式/静态检查 → 编译 → 起隔离实例 → 真机全流程 → 重启 → 迁移往返 → 不变量 → 截图」整条链重跑了一遍（`handler.rs` 在 clippy 修 `redundant_guards` 之后又整链复跑了一次，下面是最末尾一轮的数字）：

```
22:2x  cargo fmt -- --check / cargo clippy --lib   → 均退出 0（附件模块零告警）
22:30  cargo build                                → Finished dev profile (38.28s)
22:30  cargo test --lib business::attachment      → 15 passed / 0 failed (8.17s)   raw/04
22:30  p14_instance.sh start（cp_p14_e2e, :18098） → /health 200，「应用 0 个迁移」
22:30  p14_attachment_e2e.py（主流程）             → 通过 38 / 失败 0             raw/02
22:30  stop && start（同一库）→ after-restart      → 通过 7 / 失败 0              raw/03
22:27  p14_migration_roundtrip.sh                 → 3 轮 up/down 全过，零残留      raw/06
22:31  erp_invariants.py (cp_p14_e2e)             → 40/40，BLOCKER 0 / WARN 0      raw/07
22:31  p14_screenshots.cjs（真实浏览器 5174→18098） → 失败 0，5 张 PNG + ui-drawers.txt 重出
22:20  pnpm typecheck                             → EXIT=0                         raw/08
```

### 真机关键断言（节选，原文见 `raw/02-real-machine-main.txt`）

```
[PASS] 上传合同扫描件 200
[PASS] 合同列表 1 条
[PASS] 列表角色中文名正确
[PASS] 下载字节与原文件一致
[PASS] Content-Disposition 为 attachment
[PASS] X-Content-Type-Options: nosniff
[PASS] PURCHASE_ORDER / STOCK_IN / PAYMENT 上传 + 列表各 1 条
[PASS] doc_type 非法 → 400 + 10001（消息列出可选类型）
[PASS] 缺 file 字段 → 400
[PASS] 不存在的合同 → 404（且没有落下任何绑定行）
[PASS] 无权限账号 列表/上传/下载 全部 403
[PASS] 删除 200 + deleted=true → 列表归零 → 再下载 404 → 重复删除 404
```

### 前端截图（真实浏览器，dev server 5174 → 隔离实例 18098）

```
01-contract-attachments.png   合同详情：2 条（合同扫描件 / 补充协议，带备注）
02-contract-upload.png        同一抽屉：用界面上传第 3 个文件后（2 → 3 条）
03-purchase-attachments.png   采购单详情：1 条（签收单）
04-stockin-attachments.png    收货单详情：1 条（签收单）
05-payable-attachments.png    应付（付款）详情：1 条（发票影像）
```

`ui-drawers.txt` 是四个抽屉附件区的纯文本 dump（可 diff）：

```
附件 / 已上传 2 / 50
合同扫描件 | 合同扫描件.png | 84 B | 超级管理员 | 2026-11-03 20:37:24 | 下载 删除
其他 | 合同补充协议.png | 86 B | 超级管理员 | 2026-11-03 20:37:24 | 补充条款第 3 页 | 下载 删除
```

---

## 6. 与卡面的差异（逐条说明）

| 卡面 | 实际 | 原因 |
|------|------|------|
| 迁移 `V0209` | **`V0212`** | `V0209`（合同审批）/`V0210`（业务提醒）/`V0211`（盘点单）都已被占；按「最大号 + 1」取 V0212（迁移号是全仓共享资源，不能抢号） |
| DELETE 端点权限 `business:attachment:delete` | 路由挂 `business:attachment:upload`，`:delete` 落行级 | `authz_tests::o13` 禁止写接口挂读码（第一版挂 `:list` 被全量套件抓红）；`:delete` 仍存在且是「删他人附件」的唯一判据 |
| 前端「付款单详情」 | 应付单详情（`doc_type=PAYABLE`） | 页面上用户看的单据本体就是应付单（`/business/erp/payable`）；单笔付款登记行 `biz_payment` 也支持（`doc_type=PAYMENT`），接口两侧都在真机验证过 |
| 表字段 `uploaded_by` / `created_at` / `deleted_at` | 照做，另加 `remark` | 备注真实需求（「第二页缺章」这类），列可空不影响卡面契约 |

---

## 7. 已知行为 / 遗留

1. **内容秒传会复用首次的文件名**：storage 按内容 SHA-256 命中去重，同一份字节第二次上传时
   列表显示的是**首次**上传的 `original_name`（任务附件 E14 同款语义，本次打 INFO 日志不阻断）。
   真机夹具因此给四类单据用**内容不同**的字节；真实业务里四张单据的文件本来就不同。
2. **`/api/business/notice/stream` 在隔离实例上 401**（截图脚本的 console 里会出现一条）：
   SSE 端点与本次改动无关（同一实例上登录后即复现，请求只会话流），不影响附件任何功能。
3. **物理文件回收**依赖 storage 既有清理任务（软删 7 天后），本卡不另造通道；`raw/02` 里
   `file_deleted=true` 表示该文件已无其他绑定，`sys_file` 同时软删。
4. **前端只做了「查看 + 上传 + 下载 + 删除」**：没有做「按角色筛选 / 批量下载 / 图片缩略图」，
   卡面未要求；需要时在同一个组件里加，后端不用改。

---

## 8. 复现命令

```bash
cd /opt/cloudpivot

# ① 空库 + 启动链迁移（含 V0212）
su - postgres -c 'createdb -O alun cp_p14_e2e'
bash scripts/verify/p14_instance.sh start          # 18098

# ② 真机主流程（38 断言：上传/列表/下载/负例/超权限/删除）
python3 scripts/verify/p14_attachment_e2e.py

# ③ 重启后再验（7 断言：附件不丢）
bash scripts/verify/p14_instance.sh stop && bash scripts/verify/p14_instance.sh start
python3 scripts/verify/p14_attachment_e2e.py --phase after-restart

# ④ 迁移三轮往返
bash scripts/verify/p14_migration_roundtrip.sh

# ⑤ 不变量
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME=cp_p14_e2e \
  python3 scripts/verify/erp_invariants.py

# ⑥ 单测（真库）
cd backend && ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cp_p14_clean" \
  cargo test --lib business::attachment -j 1 -- --test-threads=1

# ⑦ 截图（先起前端 dev server：18098 → 5174）
bash scripts/verify/p14_web.sh start
cd frontend && node ../scripts/verify/p14_screenshots.cjs
```
