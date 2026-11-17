# M3-08 报价单转订单：单测（含部分转单）· 证据

- **卡**：`t_8d301195`（M3-08【M3·销售】报价单转订单：单测 / 含部分转单）
- **上游**：M3-01 表（V0432）/ M3-02 CRUD+状态机 / M3-03 转单主链 / M3-04 审批闸 / M3-05 报价单单测 /
  M3-06 前端页 / M3-07 PDF
- **结论**：转单链路单测落地 **21 条全绿**；`cargo fmt` / `cargo clippy --lib --all-targets -D warnings` 干净；
  真库演练 `scripts/verify/m308_quotation_convert_drill.sh` **ALL PASS（20 项断言 0 失败）**，
  回归「报价单整组 83 条 / 销售域整组 205 条」全绿。

## 一、本卡做了什么

### 1）新增 `backend/app/src/business/erp/sales/quotation_convert_tests.rs`（21 条）

挂在 `quotation_handler` 下（`#[path]` + `#[cfg(test)]`），够得着私有转单件：
`parse_convert_body` / `plan_order_items` / `compose_order_remark` / `ensure_convertible` / `CONVERT_ACTION`。

| 组 | 用例 | 钉住什么 |
|:--|:--|:--|
| 入参闸（纯逻辑） | `partial_convert_keys_are_rejected_with_a_way_out` | **卡面点名的部分转单**：`line_nos`/`line_no`/`lines`/`items`/`line_ids`/`convert_lines` 六个键**逐个** 400 + 专用文案（含「新建只留接受行的报价单 / 整单转换后删行」的下一步）；整单参数不被误伤 |
| | `unknown_and_misspelled_params_are_400_not_silently_dropped` | 未知键 / 拼错键（`order_dtae`）400，报错回显拼错键并列出 4 个合法键 |
| | `empty_object_and_null_bodies_default_but_never_partially_parse` | 空 body / `{}` / 四个键全 `null`（前端 form 原样提交）→ 默认值；数组 / 字符串 / 数字 / `null` / 非法 JSON / 非 UTF-8 → 400 |
| 窗口闸 | `date_window_is_validated_before_any_database_work` | 日期格式严格；交期早于点单日 → 400（报文带两个日期）；销售员 id 非法 → 400。三条都**在读库之前**返回 |
| 状态闸 | `convert_guard_names_status_label_and_quote_no_for_every_refusal` | 只有 `ACCEPTED` 能转；DRAFT/SENT/REJECTED/CANCELLED 全 409，报文带状态码 + 中文标签 + 单号 + 目标状态；`SENT` 额外给「先走状态接口标接受」 |
| 行转换 | `plan_copies_every_line_even_with_repeated_material_and_zero_price` | 同一物料两行不同价**不合并**、赠品行（单价 0）合法、行数 = 报价行数、逐字段整列搬 |
| | `plan_keeps_per_line_rounding_and_sums_the_rounded_lines` | 先按行 4 位舍入再汇总（3 × 0.00005 → 0.0003，先加再舍入会得 0.0002） |
| | `plan_rejects_partially_dirty_data_as_a_whole` | **部分脏 = 整笔拒**：只有税额被改脏 / 单头税额差 0.0001 / 空明细 / 少一行（部分转单的数据形态）→ 409，报文点名行号与字段 |
| | `plan_accepts_tax_rate_bounds_zero_and_hundred` | 税率 0 与 100 都合法，越界才拒 |
| 备注 | `remark_keeps_the_real_source_even_when_user_text_mimics_it` | 来源「由报价单 QT-… 转换」永不被顶掉；用户自己写「由报价单 QT-FAKE 转换」也不被当成来源；空白备注等同没填 |
| 真库（`M308-` 夹具） | `partial_conversion_is_refused_and_quote_stays_fully_convertible` | 端到端复现路由顺序：部分转单请求 400 后**零订单 / 未回填 / 号段没动 / 报价明细原样**，随后空 body 整单转 3 行全部到位（不留死结） |
| | `conversion_renumbers_gapped_line_nos_and_copies_every_field` | 报价行号空洞（1/3/7）→ 订单行号 1..3 连续；每个字段（物料/数量/单价/金额/税率/税额）逐行相等；已发货量 0 |
| | `failure_after_order_insert_rolls_back_order_backfill_log_and_seq` | **事务原子性**：注入「写 CONVERT 轨迹必失败」的临时 CHECK 约束 → 订单 0 行、回链为空、**号段回退**、报价明细不动、轨迹 0 行；拆掉约束后同一张单能正常转（约定 11：不留只能改库的中间态） |
| | `header_params_land_on_the_order_including_salesperson_name` | `order_date` / `delivery_date`（相等合法）/ 启用销售员（出参带 `salesperson_name`）/ 用户备注作前缀；订单 `contract_id` 为空、状态 DRAFT、汇率 1、本位币三项 = 原币 |
| | `inactive_or_unknown_salesperson_is_rejected_before_any_write` | 停用账号 / 不存在账号 → 400 且**一次写都不发生**（订单 0、号段不动） |
| | `expired_accepted_quote_still_converts_and_flags_expired` | 已接受但今天已过期仍可转（过期约束「接受时」），`meta.quotation_expired = true` 如实标示 |
| | `material_labels_are_filled_when_present_and_absent_without_failing` | 主数据 join：查得到带编码/名称，查不到留空但**整单照转**（弱引用，不编造名字） |
| | `unique_index_prevents_two_quotes_claiming_the_same_order` | 库层兜底 `uk_biz_quotation_converted_so`：第二张报价单直接 UPDATE 认领同一订单 → `UniqueViolation{constraint}` 命中该索引；第二张仍能转出自己的订单 |
| | `idempotent_replay_returns_the_same_order_without_burning_a_second_number` | 幂等重放回原单 + `already_converted=true`，**不重复吃号**、不建第二张；重放时改交期/备注**不改**已建订单 |
| | `conversion_only_backfills_the_link_and_never_rewrites_the_quote` | 转单只回填 `converted_so_id/converted_at` + 一条 `CONVERT` 轨迹（`biz_ref` 指向订单号）；报价状态/三项合计/备注/明细逐行原样 |
| 清场 | `zz_teardown_purges_fixtures_and_status_log_orphans` | 客户 / 账号 / 报价单 / 订单 / 注入约束全 0 残留，销售链孤儿日志收干 |

### 2）handler 收口「部分转单」（`quotation_handler.rs`）

- `ConvertQuotationReq` 加 `#[serde(deny_unknown_fields)]`：多写/拼错键必须 400，不许 serde 静默丢弃；
- 新增 `PARTIAL_CONVERT_KEYS`（6 个「转哪几行」的键名）+ `parse_convert_body` 先解析成 `Value`
  再判：非对象 → 400；命中部分转单键 → **400 + 专用文案**；其余未知键 → 400（serde 列合法键名）；
- 模块文档补「部分转单」一行 + 一节「为什么是拒绝而不是漏做」。

**为什么不做真正的部分转单**（口径决定，写进代码注释与本文件）：
V0432 的回链是**单值** `converted_so_id` + `uk_biz_quotation_converted_so`（一张订单只被一张报价认领）
⇒ 一张报价单只能换出一张订单，换完即用尽。客户只接受部分明细时的正解是
**新开一张只保留接受行的报价单**再转，或整单转换后在 `DRAFT` 订单里删行。
要做「一张报价单分批转多张订单」得改表结构（行级已转数量 + 转换多对多），不是给接口加参数。

### 3）顺带修掉一个真 bug：销售订单轨迹的词表写错

`fixture_hygiene::ENTITY_TABLES` / `ALL_TYPES` / `SALES` 里登记的是 `SALES_ORDER`，
但销售订单轨迹落库的 `entity_type` 是 **`SALE_ORDER`**（`DocKind::SaleOrder.as_str()`，
145 模板库实测 `SALE_ORDER` 32 行 / `SALES_ORDER` 0 行）——
于是「销售订单孤儿日志」的统计与清理**从来没收到过任何一行**（永远 0 = 假绿），
真孤儿一直在测试库里攒。同错也出现在 M3-05 的清场 SQL（`quotation_state_tests.rs`）。

- `fixture_hygiene.rs`：两处词表改 `SALE_ORDER`（保留注释说明为什么写错会「永远绿」）；
- `quotation_state_tests.rs`：清场 SQL 的 `entity_type` 改 `SALE_ORDER`；
- 实证：本卡演练库跑完 `SALE_ORDER` 孤儿日志 6 → 0（模板自带的历史孤儿被 `zz_` 收尾一次收干）。

## 二、验收（全部真跑）

| 项 | 命令 | 结果 |
|:--|:--|:--|
| 格式 | `cd backend && cargo fmt -p cloudpivot -- --check` | 0 差异 |
| Lint | `cd backend/app && cargo clippy --lib --all-targets -- -D warnings` | 0 warning（1m45s） |
| 本卡单测 | `bash scripts/verify/m308_quotation_convert_drill.sh` | **21 条全绿**，SKIP=0 |
| 回归 | 同上（脚本内） | 报价单整组 **83** 条、销售域整组 **205** 条全绿，SKIP=0 |
| 夹具卫生 | 同上（脚本内） | `M308-CUST` / 夹具账号 / 注入约束 0 残留；孤儿日志 0 ≤ 基线 6 |
| 演练汇总 | `docs/evidence/2026-11-17-m308-quotation-convert-tests/00-drill.out` | **PASS=20 FAIL=0 / ALL PASS** |

演练环境：99 上编译 + `cargo test`，库在 145（`cp_m308_convert_drill_<pid>` 从模板 `cloudpivot_test`
克隆 + 补 V0405…V0439 迁移），跑完脚本自己 DROP。

## 三、下游提示

- 前端无需改动：`buildConvertPayload` 只产出 4 个合法键，`deny_unknown_fields` 不会误伤；
  但**前端以后想加「只转选中行」必须先做表结构**（见上「口径决定」），别在 body 里塞 `line_nos`——
  现在会拿到 400 而不是静默整单转。
- `fixture_hygiene::SALES` 已改为 `["QUOTATION", "SALE_ORDER"]`；其它模块若仍手写
  `entity_type = 'SALES_ORDER'` 请一并核对（本卡只改了跑得通的那两处）。
- 错误码：本卡**零迁移、零新权限码**（转单接口与 `business:erp:quotation:convert` 均为 M3-03 既有）。
