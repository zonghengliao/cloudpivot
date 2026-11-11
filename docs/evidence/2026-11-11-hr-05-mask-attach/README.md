# HR-05 · 迁移 V0388（脱敏 + 3 个 hr 开关）与 V0389（附件单据类型 17 → 21 + 2 条规则）实测证据

卡：`t_b85f402e`（HR-05【HR】迁移 V0382 + V0383）
设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ⑥⑦⑩ / §7.1 / §8.1 / §8.3 R10

## 一句话结论

卡面写的 `V0382 + V0383` 都已被实占（成本核算 `V0382__stock_in_production_sourceless`；`V0383` 现为
HR-01 的 `hr_tables`），本卡按设计 §7.1 的**实际号**落 **`V0388__hr_mask_config` + `V0389__hr_attachment_doc_types`**。
空库首启由迁移加载器一次跑通 **320 个迁移**（`statements=7` / `6`，`success=t`），二次启动 `应用 0 个迁移`；
存量 17 类附件在放宽后**逐类仍在、0 行违规**；`up×2` / `down×2` 幂等；**3 条护栏负向反证全红且拒绝后库未被改动**；
48 条真库断言 `PASS=48 FAIL=0`；10 条静态测试 + `fmt` / `clippy -D warnings` / 迁移号唯一性全绿。

## 环境

| 项 | 值 |
|---|---|
| 存量演练库 | `hr0389_t1`（`TEMPLATE hr0387_t1` = 318 个迁移、17 值 CHECK、9 条既有规则） |
| 空库首启库 | `hr0389_t4`（全新空库，`su - postgres` 建，owner `alun`） |
| 后端 | `127.0.0.1:18391`（`NIK_DATABASE_NAME=hr0389_t4`，三把密钥显式注入；二进制为 `target/debug/cloudpivot` 副本，迁移从 `backend/migrations/` 运行时读取） |
| 演练脚本 | `hr05_drill.sh`（在 scratch；逻辑与断言清单见下，日志全量留档） |

## 文件

| 文件 | 内容 |
|---|---|
| `01-drill.log` | 存量演练全量日志：48 条断言逐条 PASS、`up×2`/`down×2`、3 条护栏反证（含 NOTICE 原文） |
| `02-boot-fresh.log` | 空库首启：`migration=hr_mask_config statements=7` / `hr_attachment_doc_types statements=6` / `应用 320 个迁移` / `启动 -> http://127.0.0.1:18391` |
| `03-boot-second.log` | 同库二次启动：`应用 0 个迁移`（幂等，指纹校验通过） |
| `04-db-snapshot.txt` | 迁移行、2 条脱敏、3 条配置、21 值 CHECK 原文、规则表 11 行、role CHECK 仍 15 值 |

## 验收点逐条对账

### ① 敏感字段登记到 `sys_field_mask`（设计 §3.2 ⑥）

| 列 | mask_type | mask_param | role_id | 实际输出（代码实测） |
|---|--:|---|:--:|---|
| `biz_employee.id_card` | 2（身份证） | `6,4` | NULL（全局） | `110101****1234` |
| `biz_employee.bank_account` | 5（银行卡） | `4,4` | NULL（全局） | `6222 **** 7890` |

- 参数口径由 `seeded_mask_params_match_masker_output` 这条单测把**库里的参数**与
  `business/masking/masker.rs` 的**实际输出**钉在一起（参数写错 → `MaskError` → 出参整值被抹掉）。
- 身份证中间星号是**固定 4 个**（`nik-utils::Mask::id_card` 的实现口径），不是「长度 − 前后保留」——
  设计稿的示例星号个数与代码不一致，已回写设计文档 §3.2 ⑥。
- 落库主键用**固定 UUID**（`…b8800000-…0001/0002`）：设计稿的 `gen_random_uuid()` 没有幂等性
  （重放插第二条同 `(表,列,NULL)`，撞 `uk_sys_field_mask_rule` 的 `NULLS NOT DISTINCT`），且 `down` 无处下手。
- 3 条遗留的 `sys_user.*` 脱敏规则原样在位（`04-db-snapshot.txt` ② 段），本迁移对 `sys_field_mask` **只有 INSERT**。

### ② 附件 CHECK 放宽后不影响存量（设计 §3.2 ⑦ / §7.2）

演练做法：先在有 17 值 CHECK 的 318 迁移库上插**17 类老 doc_type 附件各 1 条**（含 `RECEIPT_REFUND`）
+ 1 条客户自建规则，再跑迁移。

| 断言 | 结果 |
|---|---|
| 约束取值数 17 → **21**（两个表都改） | PASS |
| `RECEIPT_REFUND` 仍在（照抄设计稿 20 值列表会摘掉它） | PASS |
| 17 类存量附件**逐类仍在**（`CONTRACT:1 … SUPPLIER:1`，与基线逐字节一致） | PASS |
| 存量附件 `doc_type NOT IN (21 值)` = **0 行** | PASS |
| `ck_biz_doc_attachment_role` 仍 15 值（本迁移不该动它） | PASS |
| 客户自建规则未被改动（down 只删本迁移的两条） | PASS |
| 新类型（`EMPLOYEE`）附件能写入（放宽真的生效，不是只改了个定义） | PASS |

### ③ 幂等

- `up` 重放：V0388 五条 `INSERT` 全 `INSERT 0 0`；V0389 两个约束 `已含 4 个新值，跳过`（不 DROP 再 ADD），
  规则插入 0 行 → 脱敏仍 2 条、配置仍 3 条、规则仍 12 条（演练态）。
- `down` 重放：`删除 HR 附件规则 0 条`、`DELETE 0` ×2。
- 空库首启 → 二次启动 `应用 0 个迁移`。

### ④ 3 条护栏负向反证（全部 rc≠0，且**拒绝后库未被改动**）

1. `down` 时还有 `EMPLOYEE` 附件 → 拒绝，报文本点名「还有 1 条 HR 新类型附件」，约束仍 21 值；
2. `down` 时还有客户自建的 `LEAVE_REQUEST` 规则 → 拒绝，报文本区分「不是本迁移种的两条」；
3. `V0388 down` 时未删员工仍有身份证/工资卡 → 拒绝，报文本明说「会让这些明文直接出现在列表/详情/导出里」。

### ⑤ 空库首启（迁移链，不是 psql 手工跑）

```
迁移已应用 migration=hr_tables statements=59
迁移已应用 migration=hr_accounts statements=7
迁移已应用 migration=hr_seq_dict statements=4
迁移已应用 migration=hr_menu_perms statements=6
NOTICE: V0388 前置条件通过：sys_field_mask / sys_config / biz_employee(id_card,bank_account) 与两个唯一索引在位
迁移已应用 migration=hr_mask_config statements=7
NOTICE: V0389：ck_biz_doc_attachment_type 已放宽到 21 值（旧定义 … 17 值 …）
NOTICE: V0389：ck_biz_attachment_rule_doc_type 已放宽到 21 值
NOTICE: V0389：EMPLOYEE_CONTRACT 必传规则插入 1 行（0 = 已存在，幂等跳过）
NOTICE: V0389：SALARY_SHEET 建议规则插入 1 行（0 = 已存在，幂等跳过）
迁移已应用 migration=hr_attachment_doc_types statements=6
数据库迁移完成: 应用 320 个迁移
```

## 两处设计稿纠偏（真库实测，不纠就是事故）

| 设计稿写的 | 照抄的后果 | 落地版 |
|---|---|---|
| 附件 CHECK `16 → 20`（列表里没有 `RECEIPT_REFUND`） | V0368 早已放宽到 **17** 值；按 20 值重建会把 `RECEIPT_REFUND` 摘掉 → 存量退款凭据附件在 `ADD CONSTRAINT` 校验阶段整笔迁移回滚 | `17 → 21`，自检断言「17 个老值一个不少」 |
| `SALARY_SHEET` 规则 `file_role='OTHER'`、`enabled` 未定 | `'OTHER'` 是上传接口的**入参老别名**，落库归一成 `'DEFAULT'`；规则侧按字面值比 ⇒ 永远匹配不到附件，「传了也说没传」。设计又明写「建议传、不硬拦」，`enabled=true` 会把它变成**永久硬闸**（工资表过账卡死） | `file_role = NULL`（任意角色）、`enabled = false`（规则在附件规则页可见，运维要硬闸一键打开） |

两处均已回写设计文档 §3.2 ⑦，并在迁移头注释里写明理由。

## 本卡**没做**、要别的卡接的（设计 §8.3 R10 的另外两处）

4 个新 `doc_type` 要同步 **4 处**，本卡只做了库侧两处：

| 处 | 文件 | 归属 |
|---|---|---|
| ① `ck_biz_doc_attachment_type`（21 值） | 本卡 `V0389` | ✅ |
| ② `ck_biz_attachment_rule_doc_type`（21 值） | 本卡 `V0389` | ✅ |
| ③ `DocKind` / `ATTACHMENT_DOC_KINDS`（17 → 21） | `backend/src/business/shared/doc_kind.rs` | **HR-06** |
| ④ 前端词表 | `frontend/src/api/business/erp/doc-kinds.ts` / `attachment.ts` | **HR-07** |

③④ 未落地前，界面上传这 4 类附件仍会被 DTO 白名单拦（400）—— 这是分批落地的预期顺序；
反过来，缺 ①② 的话 ③④ 落地后上传会撞 `23514` 变 500。

## 残留

- 库 `hr0387_t1`（HR-04 遗留）、`hr0389_t1`（演练）、`hr0389_t2` / `hr0389_t3`（中间调试）、`hr0389_t4`（首启证据）保留；
  headless 下 `DROP DATABASE` 被安全策略拦截，未强删。
- 后端 `127.0.0.1:18389/18390/18391` 均已停；二进制度本在 scratch（24h 自动清理）。
