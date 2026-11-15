# M1-06 汇兑损益科目配置 · 证据

卡：`t_dc8892cb`（M1-06 · 多币种收口第 6 步）。迁移：`V0411__fx_gain_loss_account`。

## 一、做了什么

| 项 | 内容 |
|---|---|
| 参数 | `sys_config.erp.fx.gain_loss_account`（`STRING`，缺省 `5603` 财务费用）—— 唯一的真相来源，值只存**科目编码** |
| 接口 | `GET /business/erp/fx-settings`（读当前配置 + 可选科目）；`PUT /business/erp/fx-settings`（改，body `{ "account_code": "5603" }`） |
| 权限码 | 新增 `business:erp:fx-settings:update`（V0411：菜单按钮行 父=汇率管理 / `sort=60` / title=汇兑损益科目；授 SUPER_ADMIN + FINANCE_MANAGER）；读接口复用 `business:erp:fx-rate:list`，不新增码 |
| 错误码 | 新增 `66083 ERP_FX_ACCOUNT_INVALID`（配置指向不可用科目 → 400，点名编码 / 名称 / 参数键 / 两条出路） |
| 消费方 | `period_fx_reval::revalue`（M1-05 期末调汇）、`fx_revaluation::revalue_period`（SO-41 应收重估）—— 两处此前的硬编码 `5603` 全部改为读参数 |
| 前端 | 汇率管理页顶部「汇兑损益 <编码 名称>」标签 + 工具栏「汇兑损益科目」按钮 → 配置抽屉（当前值 / 来源 / 影响面 / 可选科目下拉 / 保存） |

## 二、口径（唯一真相）

```text
参数键 erp.fx.gain_loss_account（sys_config，STRING，分类 erp_fx，可维护）
  ├ 有值  → 用它（每次调汇 / 重估前都校验，不可用即 400 + 66083）
  └ 无值  → 缺省 5603（= 本卡之前两处的硬编码值 ⇒ 存量行为零变化）
```

**「配置坏了」与「没配置」是两件事**（这是本卡最核心的一条）：

| 状态 | 行为 | 理由 |
|---|---|---|
| 参数缺失（行不存在 / 值为空 / 参数被停用） | 用缺省 5603 继续算 | 确实没有任何配置意图；且与上线前行为等价 |
| 参数有值但科目不可用（不存在 / 停用 / 非末级 / 非损益类 / 已标外币核算） | **硬拒绝 400 + 66083**，不回落 5603 | 静默回落 ⇒「改了配置没生效」永远查不出来；汇兑损益记错科目是**两张报表同时错**（该科目虚增 + 利润表少一块） |

四条入口校验（`fx_settings::check_target`，纯函数）：

1. 科目存在且未软删；
2. 启用（`status = 1`）；
3. 末级（`is_leaf = 1`）—— 汇总科目自身没有余额，「差额记到汇总科目」= 记到不存在的账；
4. 损益类（`account_category = 'PROFIT_LOSS'`）—— 汇兑损益是当期损益；
5. 未标「外币核算」（`is_foreign_currency = 0`）—— **标了就会自己调自己**：M1-05 按这个标记扫调汇范围，
   它既出调整行又承接净额，上期净额下期再被重估 ⇒ 差额递归放大、永不收敛。

校验顺序即上表顺序（多种问题叠加时报第一个能修的，一次改一处）。

## 三、为什么是 sys_config 而不是新表

与库存期初对方科目（`sys_config.inventory_opening_equity_account`，FIX-6 / V0328）同一套取舍：
一行 KV 参数不值一张表 —— `sys_config` 已带类型 / 级别 / 可维护标记 / 软删 / 部分唯一索引（`uk_sys_config_key`），
系统参数页已能查改与审计，写操作自带运行时快照 reload。代价是「编码不是外键」，由接口校验 + 运行期 66083 兜住。

参数值存**编码**不存 `biz_account.id`：id 在克隆库 / 重建环境里会变，编码是业务可读、跨库稳定的键。

## 四、迁移 `V0411__fx_gain_loss_account`

- up：参数种子（`WHERE NOT EXISTS`，**不覆盖已存在的值**）+ 一个权限码菜单按钮行（`ON CONFLICT (id) DO NOTHING`）
  + 两个角色授权（`NOT EXISTS` 去重）+ 自检块（参数行可用性 / 节点在位 / 一码一节点 / title 规范 / 同父 sort 唯一 / 授权到位）。
- 自检块**不断言值等于 5603**：重放时运维可能已经改成 5601（那是真实意图），自检只断言「这一行是可用参数」
  （非空 / ≤30 字 / 只含 `[A-Za-z0-9._-]` / STRING / 可维护 / 启用）。这一条是演练自己抓出来的坑（见下）。
- down：撤授权 + 删节点；参数行**只在值仍是 5603 时删**，已人工改过则保留并打 NOTICE。

## 五、实测（全部真跑，命令与脚本入库）

| 项 | 结果 | 命令 |
|---|---|---|
| 迁移 psql 往返（幂等 / 结构 / 反证 / down 护栏） | **PASS=24 FAIL=0** | `bash scripts/verify/m106_fx_settings_drill.sh` |
| 真引擎 + 真 HTTP（克隆库升级 7 个迁移 / 空库首启 331 条 / 重启 0 迁移 / 接口 24 项） | **PASS=45 FAIL=0** | `bash scripts/verify/m106_fx_settings_engine_and_api.sh` |
| 真库单测（`fx_settings` 全套 + `period_fx_reval` 全套 + `fx_revaluation` 全套） | **41/41 pass**（空库首启库 `cp_m106_t2`） | `ERP_DB_URL=… cargo test --lib -p cloudpivot -- business::erp::fx_settings business::erp::period::period_fx_reval business::erp::fx_revaluation --test-threads=1` |
| `cargo fmt` / `clippy --lib -D warnings` / `clippy --all-targets -D warnings` | 全绿（0 warning） | `cd backend && …` |
| 前端 `typecheck` / `typecheck:unit` / `test:unit` / `build` | 全绿，单测 **1880 pass**（含本卡 9 条） | `cd frontend && …` |
| `menu_invariants.sh`（8 条断言） | **全绿**（菜单总数 504 ∈ 400–520） | `bash scripts/verify/menu_invariants.sh cp_m106_t2` |
| `perm_menu_gate.py` | **PASS**（`missing_in_menu = 0`，404 个注解码全部有菜单承载） | 连 `cp_m106_t2` |
| 演练库残留 | 0（`cp_m106_*` 全部 DROP） | — |

### 真 HTTP 覆盖的 24 项（脚本 C 段）

① GET 配置（`5603` / `财务费用` / `source=CONFIG` / `problem=null` / 候选含 5603 与 5601、不含资产类）；
② 四条校验各拦一次：资产类 `1002` → 400+66083（报文含「损益」）、汇总 `5602` → 400+66083（含「末级」）、
不存在 `M106.99` → 400+66083（点名编码）、空串 → 400+10001；且四次被拒后参数值仍是 5603；
③ PUT `5601` → 200 + 库里 `config_value` 真的变成 5601 + 再 GET 读回 5601（证明**运行时快照已刷新**，不是内存默认）；
④ 同值再 PUT → 200 且 `updated_at` 不变（幂等不写库）；
⑤ 只授 `fx-rate:list` 的账号：GET 200 / PUT 403（读不另开码、写单独授权）；
⑥ 把 `5601` 停用后：GET 仍 200 + `account_id=null` + `problem` 点名「5601 已停用」，PUT 该科目 → 400+66083；
⑦ 恢复 `5603`。

### 真库单测覆盖的行为（41 条里的关键 8 条）

- `get_returns_seeded_5603_with_candidates`：迁移种子读得到科目名与候选，`problem = null`；
- `missing_config_falls_back_to_default_5603`：删掉参数行后 `source = DEFAULT`、编码回落 5603；
- `save_writes_config_and_business_path_reads_it`：改 5601 后**参数行真变**，两条业务出口（`DbResult` / `ApiError`）都取到 5601；
- `saving_the_same_value_twice_is_idempotent`：同值不写库（`updated_at` 不变）；
- `invalid_targets_are_rejected_and_config_stays_untouched`：接口层也真拦四种坏科目，且拒绝时参数一个字节不动；
- `broken_config_is_reported_by_get_and_rejected_by_reval_path`：配置坏 → GET 200+problem、调汇链路 400/66083（`DbError::InvalidValue` / `ApiError` 两条出口）；
- `revalue_uses_configured_gain_loss_account`：参数改 5601 后，调汇凭证的净额行真的落在 **5601**、凭证里再没有 5603；
- `revalue_rejects_unavailable_configured_account`：参数指向不存在科目 → 调汇 400+66083，且**一张凭证都没落**（不静默回落）。

## 六、踩到的坑（实测，已修）

| 坑 | 现象 | 处置 |
|---|---|---|
| 迁移自检断言「值 = 5603」 | 演练 C 段（改值后重放 up）直接 `RAISE EXCEPTION`：`应恰好 1 行且值 5603，实际命中 0 行` —— 自检把「运维改过的合法配置」判成失败 | 自检改断「这一行是可用参数」（非空 / ≤30 字 / 只含编码字符 / STRING / 可维护 / 启用），值等于 5603 只由**插入侧**保证 |
| 幂等判据取错基准时间 | 引擎脚本一开始把 `updated_at` 基准取在第一次 PUT **之前**，比的是「种子时间 vs 两次写入后」，恒不相等 | 基准取在第一次 PUT **之后**，证的是「第二次没写库」 |
| `Uuid` 未开 serde | `FxSettingsRes.account_id: Option<Uuid>` 编译失败 | 出参统一用字符串（与全仓 JSON 主键口径一致） |
| `ACCOUNT_FX_GAIN_LOSS` 变成「只在测试里用」 | 改完消费方后 lib 构建报 `unused import`（clippy `-D warnings` 会红） | 主代码不再 import，测试模块内单独 `use` 并注释「按缺省口径断言」 |
| 模板库 `cloudpivot_test` 带脏数据 | 克隆库跑 `business::erp::fx_revaluation` 有 **6 条红**（差额 958.2 vs 800、应收条数 3 vs 2）：模板里遗留一条 `FXR-USD-0001`（2050-01 / USD 400 未核销）与若干他模块应收夹具，重估按期间扫「全部未核销外币应收」→ 被污染 | 与本卡无关（改动只替换了科目解析一行）。**空库首启库 `cp_m106_t2` 上 41/41 全绿**即证明；与 M1-04/M1-05 记录的「模板库缺种子 ⇒ 同组用例必红」同一类现象 |

## 七、给下游（M1-07 / M1-08 / M1-09）的接口

- 汇兑损益科目**别再写 5603**：读 `fx_settings::gain_loss_account_code()`（编码）或
  `require_gain_loss_account{,_api}`（带校验的科目）。
- 报表映射（`V0183` 资产负债表 / `V0184` 利润表 `FINANCE_EXPENSE`）仍按 `5603` 前缀取数：
  本卡只改「记账时写哪个科目」，不改报表取数口径。若把配置改到 5603 之外（例如 5601 销售费用），
  报表里的「财务费用」就不会包含这笔汇兑损益 —— 这是**会计口径决策**，要在改配置前想清楚（页面 `影响面` 文案已提示）。
- 配置读的是 `sys_config` + 进程内快照（TTL 60s）；写接口自带 `reload_runtime_snapshot`，
  **直接改库**的变更最多 60s 后生效（与本仓其它参数同口径）。

## 八、提交

`git commit`（本卡只 add 自己改的文件；工作区里其它 agent 的改动不碰）。`origin/main` 推送见卡内汇报。
