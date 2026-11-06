# SO-35 销售员字段 + 佣金规则表 —— 证据包

卡：`t_a47a9fd7`（ARCH-7 §15.4.2）。库：`cloudpivot_so35`（`cloudpivot_so34` 模板克隆 → 迁移器补跑 V0309/V0310），实例端口 `18242`。

## 一、交付内容

| 项 | 位置 | 说明 |
|---|---|---|
| `biz_sales_order.salesperson_id uuid` | `V0309` | 硬外键 → `sys_user(id)` `ON DELETE SET NULL` + 部分索引 `idx_biz_sales_order_salesperson (salesperson_id, order_date DESC) WHERE salesperson_id IS NOT NULL` |
| `biz_commission_rule`（17 列） | `V0309` | scope_type `SALESPERSON/MATERIAL_CATEGORY/ALL`、calc_type `PERCENT/FIXED`、rate、min/max_amount、有效期、status |
| `biz_commission_settlement`（15 列） | `V0309` | 单号存活行唯一 + **(salesperson_id, period_code) 存活行唯一**、status `DRAFT/APPROVED/PAID`、paid_at（`PAID ⇔ paid_at 非空` 由 CHECK 强制） |
| `biz_commission_item`（11 列） | `V0309` | `settlement_id` 硬外键 `ON DELETE CASCADE`；so_id / receivable_id / rule_id 弱引用（ADR-06 单据链零外键） |
| 佣金 7 个权限码 + 2 页 + 5 按钮 + 4 角色授权（13 对，含 2 条历史缺口补丁） | `V0310` | 码名是 SO-36 / SO-37 的契约，逐字复用 |
| 销售员接入订单 API | `sales_order_model.rs` / `sales_order_handler.rs` | 建单 / 改单可挂销售员，详情与分页回显 id + **姓名**（批量补齐，无 N+1） |

## 二、口径决策（写下来免得后人重猜）

1. **迁移号**：卡面 `V0271` 已被占用（销售链既有号段），本卡顺延 `V0309`（DDL）+ `V0310`（授权）。
2. **SALES_MANAGER 不新建**：V0267 第四节已定「本仓销售经理 = `SALES_SUPERVISOR`（销售主管）」；
   再建同义角色会出现「两个销售管理角色、`so_approve` 只认其中一个」。本卡把卡面的 SALES_MANAGER
   一律按 `SALES_SUPERVISOR` 授权，自检 6.8 断言 `SALES_SUPERVISOR` + `SALES_CLERK` 两个角色就位。
3. **金额列 18,4**：ARCH-7 稿面写 `numeric(18,2)`，本仓口径是 `NUMERIC(18,4)`（与订单 / 应收 / 凭证同型），
   否则佣金基数与凭证对账会静默四舍五入。
4. **订单销售员不默认成建单人**：佣金归属必须人明确指定，「谁建的行政单」不等于「谁卖的货」。
5. **销售员校验 = 存在 + 未软删 + 启用**：外键兜「物理清场」，handler 兜「软删 / 停用」（停用即停止接新单计佣）。
   改单沿用原值时**不再校验**（那个人后来停用，不该卡住历史单的编辑）。
6. **同人同期间唯一结算单**（`uk_biz_cmset_period`）：重复计佣 = 重复发钱。需要重算时软删旧行再生成。
7. **顺手补一条历史缺口（不在卡面内，但不补全量真库用例红）**：`so_approve / n_sales` 节点指派给
   `SALES_SUPERVISOR`，而该角色没有 `business:flow:approve`（V0239 早于 V0281）⇒ 审批节点永远点不动（403）。
   V0310 给它补 `business:flow:instance:read`（审批中心页，父页码）+ `business:flow:approve`（与 V0239 里
   FINANCE_MANAGER / WAREHOUSE_MANAGER 等审批角色的组合逐条一致）；down 会精确撤掉这两行。
8. **7 个佣金码登记到 `permission_audit_tests::ROUTE_LEVEL_ONLY`**：本卡只种码不写接口，
   `n14`（菜单码必须有源码落地）会红；按台账纪律登记 + 写清「哪张卡落地、落地即删本例外」，
   `n18` 会持续校验台账是否仍然成立。

## 三、验证证据（全部为本机真实执行输出）

| 证据 | 文件 | 结果 |
|---|---|---|
| 迁移往返（up→up 幂等→down→down 幂等→up） | `migration_probe.txt` | 每轮自检通过；两轮 down 第二轮全 `skipping`；表/列/外键/菜单/授权行数在 up/down 后精确回到 0 / 原值 |
| 迁移器记账 | `db_state.txt` ① | `309|versioned|executed|t`、`310|versioned|executed|t`；全库 `applied_via` 只有 `executed`（238 行） |
| 结构 / 约束 / 索引 | `db_state.txt` ③④⑤ | 列数 17/15/11；外键 `SET NULL` / `CASCADE` 逐条对；11 条 CHECK 全在；两个唯一索引列序正确 |
| 角色 + 授权 | `db_state.txt` ⑤ | `SALES_SUPERVISOR` / `SALES_CLERK` / `FINANCE_MANAGER` / `GENERAL_MANAGER` 在位，佣金 11 对授权逐条可查 |
| 真实 token curl | `curl_transcript.txt` | 建单回显 `salesperson_name`、详情/分页补名、空串解绑、重挂、不存在 400、乱码 400、无 token 401 |
| 常驻闸门 | `gates.txt` | `missing_in_menu = 0`、`孤儿 (角色,码) = 0`、`role_code_gate` 无未知角色码 |
| 单测 | 汇报正文 | `cargo test --lib` 全绿（含本卡新增 3 条：model 往返 1 条、handler 纯逻辑 1 条 + 真库 1 条） |

## 四、复现

```bash
# 1) 隔离库 + 实例（启动时自动补跑 V0309/V0310）
runuser -u postgres -- createdb -O alun -T cloudpivot_so34 cloudpivot_so35
bash scripts/verify/so35_instance.sh up

# 2) 真实 token curl 留证
bash scripts/verify/so35_curl_evidence.sh

# 3) 迁移往返（另开一个空克隆库，psql 直连）
runuser -u postgres -- createdb -O alun -T cloudpivot_so34 cloudpivot_so35p
bash scripts/verify/so35_migration_probe.sh

# 4) 常驻闸门（指到同一个隔离库）
export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
       NIK_DATABASE_NAME=cloudpivot_so35 NIK_DATABASE_PASSWORD=alun
python3 scripts/verify/perm_menu_gate.py && python3 scripts/verify/role_code_gate.py \
  && python3 scripts/verify/orphan_button_gate.py
```

## 五、残留（交给下游卡，别在本卡重做）

1. **佣金规则 CRUD / 计提 / 结算接口**：SO-36（计提 + 结算）与 SO-37 / 前端卡补齐；权限码已在 V0310 就位
   （`perm_menu_gate` 报告 `unused_in_code` 里能看到这 7 个码，未接后端注解前属正常）。
2. **佣金两页 path/component 留 NULL + 隐藏**：SO-37 落地前端路由后按 V0310 头注释里的两条 UPDATE 收尾，
   否则 `system::menu::tests::n08` 会红（现在隐藏 + 无 path，n08 不受影响）。
3. **行级数据范围**：`business:erp:commission-settle:list` 目前全量可见；销售员只看自己的结算单要 SO-36
   在 handler 里按登录人过滤。
4. **销售订单前端销售员选择器**：SO-37 任务项 3（后端已支持 `salesperson_id` 入参与 `salesperson_name` 回显）。
5. 隔离库保留：`cloudpivot_so35`（含 curl 留证订单）、`cloudpivot_so35p`（迁移往返探针库）。实例已停。
