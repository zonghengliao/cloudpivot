# COST-B1-7 证据：成本菜单/权限/授权 + 号段字典 + 批号回填

卡：`t_191cf205`（COST-B1-7【成本】迁移 V0376/0377/0379）
设计：`docs/architecture/ARCH-9-成本核算升级设计.md` §6.1 / §5.3 / §7.1 / §7.2

## 1. 落地了什么（三个迁移，号段整体顺延 3 位）

| 本卡实际号 | 设计稿号 | 文件 | 内容 |
|---|---|---|---|
| V0378 | V0376 `cost_menu_and_perm` | `V0378__cost_menu_and_perm.{up,down}.sql` | 6 页面菜单 + 20 按钮权限点 + 5 角色授权矩阵 |
| V0379 | V0377 `cost_seq_and_dict` | `V0379__cost_seq_and_dict.{up,down}.sql` | `biz_seq` 两条（CB/CS）+ 费用项目字典 1 类 7 项 |
| V0380 | V0379 `cost_lot_backfill` | `V0380__cost_lot_backfill.{up,down}.sql` | 三处来源存量批号 → `biz_lot` 回填 + 不合规清单 |

号段顺延原因：`V0375/V0376` = B1-5（出库加列 / 入库 PRODUCTION）、`V0377` = B1-6（科目翻末级），
`V0372~V0374` = B1-3/B1-4。nik 加载器对版本号做跨目录全局唯一校验，重号会让服务启动直接失败
（AGENTS 约定 6）。⇒ 设计稿里排 V0378 的 `cost_flow_def`（B3-2 审批流）需从 **V0381** 起。

## 2. 卡面验收逐条对照

| 卡面验收 | 结论 | 证据 |
|---|---|---|
| 菜单挂到财务管理下，sort 不冲突 | ✅ 5 个成本页挂在财务管理（e02）sort 60/62/64/66/68（该目录下 60 起原本是空档）；批次管理挂仓库管理（e01）sort **95**（设计写 80，已被出库单占用 → 见偏差一）。同父 sort 撞车 0 组 | `02_drill.out` S1；`menu_invariants.sh` ⑤⑦ 全绿 |
| 号段表 `biz_seq` 加成本相关编码 | ✅ `COST_OBJECT` → `CB-yyyyMMdd-000001`（天滚）、`COST_PERIOD` → `CS-yyyyMM-0001`（月滚），`current_no=0`、`step=1`、`separator='-'`、`status=1` | `02_drill.out` S4；`01_replay_fresh.out` |
| 批号回填幂等（重复跑不重复插，`ON CONFLICT DO NOTHING`） | ✅ 冲突目标是 `(material_id, lot_no) WHERE deleted_at IS NULL`（对应 `uk_biz_lot_material_no` 局部唯一索引）；第二次 up `本次回填 0 行`，`biz_lot` 行数不变 | `02_drill.out` S6 |
| 不合规批号清单输出到日志（不阻断迁移） | ✅ 逐条 `RAISE NOTICE 'V0380 待清理批号 #n：物料=… 批号=[…] 来源=… 问题=…'`（明细上限 200 行）+ 合计行（含空格/超长/物料失联分项）+ 明确写「不阻断迁移」；清单段**没有** `RAISE EXCEPTION` | `02_drill.out` S6 |

## 3. 三个脚本 / 三份原始输出

| 文件 | 做什么 | 结果 |
|---|---|---|
| `01_replay_fresh.sh` / `.out` | 空库全链重放（全部迁移，含本卡三件） | **300/300 迁移通过**（B1-6 时 297，+3）；菜单 26 条 / 总数 411 / 号段 2 / 字典 7 / 13 张成本表 |
| `02_drill.sh` / `.out` | 真库八段往返演练（S0 基线 → S1 up → S2 幂等 → S3 down → S4 号段字典 → S5 down 护栏 → S6 回填 → S7 down → S8 全链） | **70 条断言全绿，0 失败** |
| `03_lot_seed.sql` | S6 的源数据（入库/出库/序列号各若干，含含空格批号、软删物料、客户自建批次） | — |

`02_drill.sh` 覆盖的场景清单：

- **S0 基线**（跑到 V0377）：成本块 0 菜单 / 0 号段 / 无字典 / `biz_lot` 0 行；e02 下 sort≥60 为空档；e01 下 80/90 已被占用。
- **S1 V0378 up**：6 页 + 20 按钮 + 48 条授权（财务经理 22 / 财务总监 23 / 仓管 3；空库无 SUPER_ADMIN/ADMIN ⇒ 迁移只 NOTICE 不阻断）；红冲 `cost-period:reverse` 只授财务总监；**无孤儿**（每个被授按钮的父页也授了）；`menu_invariants.sh` ②③④⑤⑥⑦⑧ 全绿。
- **S2 V0378 幂等**：再 up 一次，菜单指纹与授权指纹逐字节相同、0 行插入。
- **S3 V0378 down**：26 菜单 + 授权删净、总数回 385；down 再跑一次 0 行。
- **S4 V0379 up**：`COST_OBJECT=CB/yyyyMMdd/0`、`COST_PERIOD=CS/yyyyMM/0`、字典 7 项、默认项唯一=`DEPRECIATION`；再 up 0 行插入。
- **S5 V0379 down 护栏**：把 `current_no` 改成 3 后 down **exit≠0** 且点名「号段已发过号」、库零变化（号段 2 条 / 字典 7 项仍在）；清零后 down 成功删净。
- **S6 V0380 up**：三处来源 4 个不同 (物料,批号) → 回填 4 行；客户自建的 `B17-LOT-B`（PRODUCTION/COST_PERIOD）**不被覆盖**；含空格批号落库已 `btrim`；软删序列号的批号 `B17-LOT-GONE` 不进批次；不合规清单 2 条（含空格 1 + 物料软删 1）进日志且不阻断；第二次 up 0 行。
- **S7 V0380 down**：只删 4 条回填行、客户自建批次保留；被成本对象引用的回填批次打 **WARNING**（1 条）但仍删；down 再跑 0 行。
- **S8 全链**：三件按序 up（= 真实升级路径）→ 菜单 26 条 / 号段 2 / 字典 7 / 总数 411 → `menu_invariants.sh` 全绿 → 逆序 down 回基线（385 菜单、号段/字典空、客户批次仍在）。

## 4. `menu_invariants.sh` 的 ① 孤儿 parent = 9：**基线既有**，与本卡无关

迁移-only 的空库（未跑 seed）里，`10000000-…-0001` / `…-0050` 这两个**系统根目录**尚不存在
（由 seed 建），于是 9 条 `system.*` / `support.*` 菜单成了孤儿。证据：
S3 down 之后（本卡菜单全删）跑同一个脚本，① 仍是 **9** 条、且被点名的 9 条全是非成本路径；
S1/S8 加上本卡 26 条菜单后 ① 依旧是 9。②③④⑤⑥⑦⑧ 全绿。

## 5. 已知偏差（都在卡内说明过，不是偷偷改）

1. **批次管理 sort 80 → 95**：设计写「仓库管理 sort 80」，但 e01 下 80 = 出库单、90 = 盘点单。按 AGENTS 约定 12「同父 sort 唯一」改取 95（末尾）。纯顺序差异。
2. **号段冲突目标 `ON CONFLICT (seq_type) WHERE deleted_at IS NULL`**（设计稿写 `ON CONFLICT (id)`）：真正的唯一键是 `uk_biz_seq_type`；客户库里若已有一行 `COST_OBJECT`（应用先跑过 `voucher::ensure_seq_row` 自动补的，id 不同），按 id 冲突会撞唯一索引让升级失败。与 `voucher.rs` / `transfer_base.rs` 的 `ensure_seq_row` 同口径。
3. **新增两道前置护栏（设计无）**：① 6 个 path / 26 个 auth_code 已被**别的 id** 占用 → 拒绝（否则会插出重复 path，`menu_invariants` ④ 红在迁移之外）；② `biz_seq` 两个固定主键被别的 seq_type 复用 → 拒绝。
4. **授权矩阵是本卡补的口径**：设计只给了 5 个角色名，没给逐码矩阵。本卡按「谁的活」落矩阵，红冲结算只给财务总监（与设计 §4「反结账（财务总监）」同口径），并断言无孤儿。
5. **不合规清单口径改成查源表**：设计 §7.2 的清单 SQL 查 `biz_lot`，但回填时批号已 `btrim`，那里永远查不出「含空格」。本迁移改为查三张源表并汇报原始值（更贴近「去源单据修数据」的用法）。
6. **`biz_seq.date_pattern` 只是展示列**：取号逻辑 `voucher::next_doc_no` 一律用北京时间 `yyyyMMdd` 当周期键（`serial_model.rs:432`），不读 `date_pattern`。所以 CS「月滚」要等 B3 的取号自带月度周期键；CB 是天滚，与实现一致。

## 6. 给下游卡的提示

- **B3-2（审批流 cost_flow_def）→ 从 V0381 起**（V0378/0379/0380 已被本卡占用）。
- B3 实现 CS 号段时注意偏差 6；`COST_PERIOD` 行已存在，别再插。
- 前端 6 个页面（B1-11/B1-12/B2-13）落地前，侧栏点进去是 404（路由未注册），菜单先种是刻意的。
- `biz_cost_object.lot_no` 引用回填批次时无外键，`V0380.down` 只打 WARNING 不阻断——回滚前先确认有没有在用。
