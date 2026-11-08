# ATT-04 附件上传 / 删除补单据读权 —— 验证证据（2026-11-09）

卡号：t_8eac092d（ATT-04，ATT-02 遗留）。提交：`dae260b`（main → origin/main）。

## 0. 一句话结论

附件「读 / 写 / 删」三处现在是同一把尺子：**看不见这张单据，就不能看它的附件、不能往上挂东西、
也不能删别人的挂件**。上传前置 403 排在「单据不存在 404」之前，无权用户拿任何 `doc_id`
（存在的 / 不存在的 / 随机 UUID）都得到**逐字相同**的 403 —— 单据存在性探测面已截断。

## 1. 代码改动

| 文件 | 改动 |
|------|------|
| `backend/src/business/attachment/dto.rs` | 新增 `preflight_upload`（字段 400 → 读码 403，不查库）、`doc_upload_forbidden`（与 `doc_forbidden` 共用 `denied()` 构造）、`can_delete` 增 `can_view_doc` 入参、`to_item` 增 `can_view_doc` 入参 |
| `backend/src/business/attachment/handler.rs` | `upload` 用 `dto::preflight_upload`（403 前移到 `model::doc_exists` 之前）；`remove` 从绑定行解析 `doc_type` → 判读码，两种拒绝分开说；`list_of` 把 `can_view` 透传给 `to_item`；接口表 / 校验顺序文档同步 |
| `backend/src/business/attachment/mod.rs` | 接口表权限列补「单据读码」；DELETE 行改正为实际路由码 `:upload`（原表写成 `:list`，与宏不符） |
| `backend/src/business/attachment/tests.rs` | +3 条纯逻辑用例（见 §3），`can_delete` / `to_item` 旧用例同步新签名 |

### 1.1 判定顺序（写进 doc comment，评审可直接引用）

```
upload ：400 字段非法 → 403 缺单据读码 → 404 单据不存在 → 400 附件数上限 → 存储策略
list   ：400 → 403 读码 → 404 单据（ATT-02 已就位）
download：400 → 404 绑定行 → 403 读码（doc_type 只能从绑定行读，ATT-02 已说明不可反转）
delete ：400 非法 id → 404 附件不存在 → 400 库内 doc_type 漂移 → 403
```

### 1.2 delete 两个分支的语义（卡面「先按卡面确认语义」的落地）

| 分支 | 判据 | 理由 |
|------|------|------|
| 上传人删**自己的** | 只认上传人本人，**不看**单据读码 | 「传错了要能撤」是上传人自己的事；换岗 / 调部门后仍能撤当年挂的附件 —— 卡面明确可接受（反过来则那条附件只能改库删）。**145 实测见 §2.c-b** |
| 持 `business:attachment:delete` 删**别人的** | delete 码 **且** 看得见该单据 | 升权处置一张看不见的单没有业务含义；与 ATT-02 同一把尺子。**145 实测见 §2.c-c** |

列表出参 `can_delete` 走同一条函数（`dto::can_delete`），按钮与接口不会失配。

## 2. 145 真机矩阵（debug 重建 + `systemctl restart cloudpivot-test`，:8033）

完整输出：`matrix_145.txt`。夹具角色 / 用户：`ATT04_NO_PAYABLE` / `att04_wh`（`attachment:upload` +
`attachment:list` + `erp:contract:list`，**故意不给 payable 码**）、`ATT04_DELETE_NO_PAYABLE` /
`att04_del`（再多一个 `attachment:delete`）。

令牌里的权限快照（JWT payload，登录时装入）：

```
[admin]     ["*:*:*"]
[att04_wh]  ["business:attachment:upload","business:erp:contract:list","business:attachment:list","erp:contract:list"]
[att04_del] ["business:attachment:upload","business:erp:contract:list","business:attachment:delete","erp:contract:list"]
```

| # | 场景 | 期望 | 实测 |
|---|------|------|------|
| a | admin 上传应付单附件（有读码） | 200 | **200**（id `01a1bc87-d6dc-…`） |
| b | admin 上传合同附件 | 200 | **200** |
| c-① | `att04_wh` 盲传应付单附件（**改造前 200**） | 403 + 30001 | **403** `无权向该应付单上传附件（需要以下任一权限：business:erp:payable:list / business:erp:payable:detail）` |
| c-② | 同一账号上传到**不存在的** payable id `d2120999-…`（探测面） | 403（不是 404） | **403，消息与 c-① 逐字相同** |
| c-③ | 同一账号上传合同附件（有读码，非一刀切） | 200 | **200** |
| c-④ | 同一账号 `doc_type=BOGUS_DOC`（字段非法先于权限） | 400 + 10001 | **400**（消息列出 16 类可选值） |
| d | admin 上传到不存在的 payable id（有读码才看得到存在性） | 404 + 10002 | **404** `应付单不存在或已删除，无法挂附件` |
| e-a | `att04_wh` 删自己传的合同附件 | 200 | **200** `deleted=true file_deleted=true` |
| e-b | 历史行模拟：admin 传、`uploaded_by` 改成 `att04_wh` 的应付附件；换岗后无 payable 读码 → 删自己的 | 200 | **200**（§1.2 第一行的口径实测成立） |
| e-c | `att04_del` 持 `:delete` 但无 payable 读码，删 admin 传的应付附件 | 403 + 30001 | **403** `无权查看该附件所属的应付单（…）` |
| e-d | admin 删同一份 | 200 | **200** |
| e-e | 非法 id | 400 + 10001 | **400** `id 格式不正确` |

HTTP 层流水（`operation_logs` 中间件行，POST × 8）与状态码逐条对上：200/200/403/403/200/400/404/200。

夹具清理：`cleanup_145.txt` —— 绑定 / `sys_file` / 留痕 / 用户 / 角色 / 用户角色 / 角色菜单
残留 **0|0|0|0|0|0|0**，4 个物理对象（`uploads/2026/11/08/*.pdf`）已按 `file_key` 删除。

## 3. 单测

| 环境 | 命令 | 结果 |
|------|------|------|
| 99（纯逻辑） | `cargo test --lib business::attachment -- --test-threads=1` | **89 passed / 0 failed**（0.25s） |
| 99 → 145 `cloudpivot_dev`（真库，经 ssh -L 15432 隧道） | 同上 + `ERP_DB_URL` | **89 passed / 0 failed**（39.04s，**无 SKIP**，真库用例实跑） |
| 99 全量 lib（无 `ERP_DB_URL`） | `cargo test --lib -- --test-threads=4` | **3835 passed / 0 failed / 0 ignored**（2.05s；真库用例按开关静默跳过） |

> 145 本机不能跑 `cargo test`：7 GB 内存下 test 二进制链接被 OOM SIGKILL（`TEST_EXIT=101`，
> rustc signal 9）。真库用例放 99 跑、库走隧道 —— 与既有约定一致。
>
> 全量 **带真库** 的一轮（`--test-threads=1` + 隧道）在 2160/3833 处按预算主动中止：
> 单线程真库用例过 ssh 隧道每条约数百毫秒，跑完需再 ~40 分钟。中止前观察到的失败只有
> `business::erp::cash_flow::tests::r3_real_*` 3 条，**单独重跑同样失败**，原因是共享库
> `cloudpivot_dev` 里残留了别的卡（P2D1）建的测试科目 —— 断言原文：`分类配置漏科目：
> ["P2D1A P2D1 测试现金 没有任何分类行…", "P2D1B P2D1 测试应付 没有任何分类行…"]`。
> 与附件模块无关（本卡只改 `business/attachment/*`，且 `cash_flow` 用例不经过附件代码）。

新增 3 条（`tests.rs`）：

1. `upload_preflight_forbids_blind_upload_before_existence_check` —— 400 先于 403；
   **存在的 id / 幽灵 id / 随机 UUID 的 403 消息逐字相同**；有读码的模块放行（非一刀切）；403 消息不回显 id、不含「不存在」。
2. `doc_upload_forbidden_names_the_missing_permission` —— 16 类单据全覆盖、点名读码、与读侧 403 措辞可区分。
3. `can_delete_requires_doc_read_for_the_delete_code_branch` —— 真值表，含**「能看 ≠ 能删」**。

质量门：`cargo clippy --all-targets` 在 `attachment/{dto,mod,handler,tests}.rs` **零告警**
（仓内既有告警属他人文件：`attachment/rule.rs` for-loop-single-element、`pdf/dto.rs` field-reassign、
`sales_order_model.rs` too_many_arguments）；`rustfmt +1.94.1 --edition 2024` 无 diff。

## 4. 残留 / 观察到的问题（写清，别留给后人猜）

1. **上传的结构化审计行（`DOC_ATTACHMENT_UPLOAD`）在本机没落库**：本次 4 次成功上传，
   `operation_logs` 里只有中间件那 8 条 `POST /api/business/attachment/upload` 流水，
   没有一条带 `doc_type/doc_id/file_name` 的业务行。上传仍走 `log::record_audit` → 日志总线
   （500ms 批量），与 ATT-03 修删除时观察到的现象一致（总线在未初始化 / 单实例进程里丢事件）。
   → 已开卡（AUD 口径，直写 `operation_logs` 的同一套做法见 `handler::download_log` / `delete_log`）。
   与本次权限改造无关，不阻塞本卡。
2. **`delete` 的上传人分支不看读码**是有意为之（§1.2），代价是「换岗后的人仍能删自己当年挂的附件」——
   卡面已明确可接受；若要更严，改动点是 `dto::can_delete` 里的一行。
3. 附件可见性仍完全由权限码表达（七 / 十六张单据表都没有 dept / owner 列，`SCOPE-EXEMPT`），
   所以「同部门可见」这类数据范围在附件层依旧是**不可表达的**，与 ATT-02 结论一致。
4. 未动 `/support/file/download`、`business/storage/file/{id}/download`（各有各的既有判据）。
5. 未碰 144、未做任何生产部署；145 只是测试机（`git pull` + `cargo build --bin cloudpivot`(debug) +
   `systemctl restart cloudpivot-test`，顺带应用了 AUD-01 的 V0352 迁移）。
