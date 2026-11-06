# SO-36 佣金计提（按收款计佣）+ 结算单 —— 证据包

卡：`t_95f4e713`（ARCH-7 §15.4.3）。库：`cloudpivot_so36`（**空库**由迁移器从 V0001 跑到 V0311），实例端口 `18243`。
前置：SO-35 的 `V0309`（三表）/`V0310`（7 个权限码 + 2 页 + 5 按钮）已就位，本卡补 `V0311` 与全部接口。

## 一、交付内容

| 项 | 位置 | 说明 |
|---|---|---|
| `V0311`：贷方科目 `2241.02 其他应付款-佣金`（末级） | `migrations/business/` | 卡面写「贷 2241」，但 V0226 已把 2241 翻成**非末级**，`post_voucher` 拒收非末级科目；与 V0226 种 2241.01、V0229 种 5602.01 同口径 |
| `V0311`：`biz_seq(seq_type='CM')` | 同上 | 结算单号 `CM-yyyyMM-000001`，**按月**滚动（凭证是 PZ 按天，两套节奏） |
| `V0311`：`ck_biz_cmitem_base` 放宽为 `base_amount <> 0` | 同上 | 基数改成**有符号**：正 = 本期新收钱，负 = 退货 / 退款冲回；0 被拒（无意义行） |
| `POST /api/business/erp/commission/settle` | `commission_handler.rs` | 按 `period_code` +（可选）`salesperson_id` 生成结算单；不给销售员 = 月结批跑所有有数据的销售员 |
| `GET /api/business/erp/commission/page` | 同上 | 分页 + 期间 / 销售员 / 状态 / 关键字筛选，**走 `scope::paginate`**（销售员只看自己的） |
| `GET /api/business/erp/commission/{id}` | 同上 | 详情 + 明细（应收单号 / 订单号 / 命中规则名回显） |
| `PATCH /api/business/erp/commission/{id}/status` | 同上 | `DRAFT → APPROVED`（出计提凭证）→ `PAID`（出付款凭证），库层 CHECK 兜 `PAID ⇔ paid_at` |
| 计提算法三件套 | `commission_dto.rs` / `commission_model.rs` / `commission_handler.rs` | 纯逻辑（规则选优 / 基数分摊 / 佣金计算）全部是纯函数，31 个单测覆盖 |

## 二、口径决策（写下来免得后人重猜）

1. **按已收款计佣**：基数取 `biz_receivable.received_amount`（收款单核销 + 收款退款双向维护的那一列），
   不取 `amount`（开单额），也不取 `biz_receipt`（收款单可能是预收，没核销到这张应收）。
2. **防重复发钱的闸**：本期可计佣 = `received_amount − 已认领基数`，
   已认领 = 该应收在**存活**结算单里的 `base_amount` 之和（**草稿也算认领**）。
   重算走「先软删旧草稿 → 重新算」，草稿被弃用时基数自然释放。
3. **退货 / 退款冲回**：`received_amount` 下降（收款退款单）⇒ 差额为负 ⇒ 落负基数、负佣金明细行；
   `total_sales` 按 0 兜底（`ck_biz_cmset_total_sales` 要求非负），冲回只体现在 `total_commission` 的负数上。
4. **规则优先级**：销售员 > 物料 > 全局；同范围多条取生效日最新 → 创建最新 → id 小（**全序**，
   否则同一单两次计提可能算出两个数）。
5. **物料粒度**：基础版**没有** `biz_material_category` 表（`erp/handler.rs` 有登记），
   故 `MATERIAL_CATEGORY` 规则的 `scope_id` 暂存 `biz_material.id`（按物料），
   接入分类表后由数据迁移换 id，匹配逻辑不变。
6. **基数按订单行金额分摊到物料桶**：一张订单跨物料时不会「整单只吃一条规则」；
   末桶吸收全部舍入残差，**桶之和恒等于本期 delta**（这是逐期可复算的前提）。
7. **佣金精度**：基数 4 位（库层 18,4）、佣金按**分**（2 位）四舍五入；凭证金额 = 明细合计，逐分相等。
8. **凭证科目**：卡面「借 6601」→ 落 **5601 销售费用**（本仓科目集没有 66xx，V0226/V0229 同一决策）；
   「贷 2241」→ 落 **2241.02**（2241 是非末级）。发放凭证：借 2241.02 / 贷 1002。
9. **凭证时点**：`APPROVED` 出计提凭证（审批通过即入账，与费用报销卡同口径），`PAID` 出付款凭证；
   纯冲回（合计为负）时整张凭证反向，借贷仍平衡。
10. **状态流转不写 `biz_status_log`**：状态日志的 `entity_type` 取值域 = `DocKind`，
   新增 `DocKind` 要同步前端 `doc-kinds.ts` 镜像与巡检登记 SQL（`doc_kind_tests` 三方逐字钉死），
   属 P2-5 词表收敛的独立改动。本卡用「`LEGAL_EDGES` 白名单 + `WHERE status = 原状态` 乐观锁 +
   库层 `ck_biz_cmset_status`」兜住，审计凭据是两张凭证的 `source_no`（= 结算单号）。
   **新增** `VoucherSourceType::Commission`（`biz_voucher.source_type='COMMISSION'`）已按词表纪律
   同步：Rust 枚举 + `VOUCHER_SOURCE_VALUES` + 前端镜像 + `10_checks.sql` 登记表，`doc_kind` 25 条用例全绿。
11. **数据权限**：`biz_commission_settlement` 登记进 `scope::SCOPED_TABLES`（owner = `salesperson_id`，
   无 dept 列 → 部门类范围安全侧降级为「仅本人」）。

## 三、验证证据（全部为本机真实执行输出）

| 证据 | 文件 | 结果 |
|---|---|---|
| 真实 token curl 全链（35 条断言） | `curl_transcript.txt` | `ALL CHECKS PASSED` |
| 迁移往返（up → up 幂等 → 负基数被拒 → down 被负行挡住 → 清场后 down → down 幂等 → up） | `migration_probe.txt` | 每轮自检通过；`base_check` 在 `<> 0` 与 `>= 0` 之间精确切换 |
| 库内真实状态（迁移记账 / 结算单 / 明细 / 凭证分录） | `db_state.txt` | 三条 versioned|executed；`CM-202611-000002` 400/80 PAID；`-000003` 1000/60 APPROVED；`CM-202612-000004` 0/−40 DRAFT |
| 全量单测 | `full_test.txt` | 见下节 |

### 3.1 验收点逐条对应（curl_transcript.txt）

| 卡面验收 | 实测 |
|---|---|
| 按已收计佣（未收款部分不计） | AR-A 应收 1000、已收 400 → 基数 **400.0000**、佣金 80；AR-C 应收 500、已收 0 → **零明细**（明细只有 2 行，全部挂 AR-A） |
| 规则优先级（销售员覆盖全局） | 三条规则同生效（全局 5% / 物料一 10% / 销售员甲 20%）→ 命中「SO36 销售员甲 20%」，佣金 48 + 32 = 80 |
| 退货扣回佣金 | AR-B 已收 1000 → 600 后，2026-12 计提 −400 基数 / **−40.0000** 佣金，备注「退货 / 退款冲回」 |
| 封顶（max_amount） | 物料一 10% × 1000 = 100 → 封顶 60：`total_commission = 60.0000`，凭证借方 5601 = 60.0000 |
| 凭证生成 | 计提：借 5601 80 / 贷 2241.02 80（借贷平衡 sign = 0）；付款：借 2241.02 80 / 贷 1002 80 |
| 权限 / 负路径 | DRAFT→PAID 409、非法期间 400、非法状态 400、无 token 401（列表 + 计提） |
| 幂等 | 同期间二次计提 → 0 张新单、2 个 skipped，原因「已审批入账不能重算」 |

## 四、复现步骤

```bash
runuser -u postgres -- dropdb --if-exists cloudpivot_so36
runuser -u postgres -- createdb -O alun cloudpivot_so36
bash scripts/verify/so36_instance.sh up                        # 迁移器从 V0001 跑到 V0311
bash scripts/verify/so36_curl_evidence.sh                      # 夹具 + 35 条断言
runuser -u postgres -- dropdb --if-exists cloudpivot_so36p
runuser -u postgres -- createdb -O alun -T cloudpivot_so36 cloudpivot_so36p
SO36_PROBE_DB=cloudpivot_so36p bash scripts/verify/so36_migration_probe.sh
bash scripts/verify/so36_instance.sh down
```

> 实例必须用**空库**首启：`cloudpivot_so35` 那套模板库的 `sales_receipt_menu_page`（V0296）
> 记录校验和与当前文件不一致（别的卡在 so35 克隆之后改过该文件），克隆库会启动失败。
> 换成空库后迁移器从 V0001 顺序跑到底，顺带把 V0311 在「干净库」上的路径也验了。

## 五、遗留

1. 佣金**规则 CRUD** 与前端两页 = SO-37（`permission_audit_tests::ROUTE_LEVEL_ONLY` 里 4 条
   `commission-rule:*` 例外保持原样，等待接口落地时删除）。
2. 结算单状态未接 `biz_status_log`（理由见 §二.10），接入需与 `DocKind` 词表扩展同批。
3. `MATERIAL_CATEGORY` 语义在基础版等价于「按物料」（无分类表），接分类表后需一次数据迁移。
