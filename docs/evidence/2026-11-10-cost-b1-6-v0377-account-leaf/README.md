# COST-B1-6（卡 t_486e652d）：V0377 成本科目翻末级 + 种 9 个末级科目

依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
§3.3 ③（科目 DDL，逐字）、§1 D-14（科目补种表）、§7.2（存量处理：翻非末级前先查历史分录）、
§4.7（三张结转凭证的借贷科目）、拆卡清单 §8.1 的 B1-6。

## 0. 号段：卡面写 V0375，实际落 V0377

`ls backend/migrations/business | sed 's/_.*//' | sort -u | tail` 现核（开工时）：

```
V0369 V0370 V0371 V0372 V0373 V0374 V0375 V0376   ← 最大号 V0376
```

V0375 = B1-5 出库加 `cost_object_id`、V0376 = B1-5 入库 `PRODUCTION` + 两列（commit `c7038665`）；
所以本卡落 **V0377**。nik 迁移加载器对版本号做跨目录全局唯一校验，重号会让服务启动直接失败
（AGENTS 约定 6）。后续 B1-7 → V0378 起（菜单权限 / 号段字典 / 批号回填）。

## 1. 改了哪些文件

| 文件 | 说明 |
|---|---|
| `backend/migrations/business/V0377__cost_accounts.up.sql` | 迁移主体（1 条 UPDATE + 1 条 INSERT + 3 个 DO 块：前置护栏 / 形态自检 / 口径自检） |
| `backend/migrations/business/V0377__cost_accounts.down.sql` | 回滚（2 个数据护栏 + DELETE 9 个科目 + 父级翻回末级 + 反向自检） |
| `backend/tests/cost_b1_6_account_leaf_split_migration.rs` | 11 条静态不变量（纯文件解析 + 引擎拆分器，不连库） |
| `docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/01_replay_fresh.out` | 空库全链重放 **297/297 PASS**（B1-5 时 296，+1） |
| `docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/02_account_leaf_drill.sh` | 真库往返演练脚本（八段，可复跑） |
| `docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/02_account_leaf_drill.out` | 演练输出（60 条断言全绿） |
| `docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/03_cargo_test_summary.out` | `cargo test` 结果 |
| `docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/04_isolated_gate.out` | 隔离工作树上的完整 L1（fmt / clippy -D warnings / check） |

**没动的东西**：任何既有迁移（V0137/V0149/V0183/V0199/V0375/V0376 一个字节都没改）、
任何业务代码、任何前端代码、`docs/architecture/**`。

## 2. 迁移做三件事

```sql
-- ① 三科目翻非末级（幂等：AND is_leaf = 1；只改 is_leaf / updated_at）
UPDATE biz_account SET is_leaf = 0, updated_at = CURRENT_TIMESTAMP
 WHERE account_code IN ('4001','4101','2211') AND deleted_at IS NULL AND is_leaf = 1;

-- ② 种 9 个末级科目（级次 = 父级+1；类别 / 余额方向**继承父级**，不硬编码）
INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status, remark)
SELECT v.code, v.name, p.id, p.level + 1, p.account_category, p.balance_direction, 1, 1,
       'ARCH-9 成本核算升级 D-14 播种：成本末级科目（V0377）'
  FROM (VALUES …9 行…) AS v(code, name, parent_code)
  JOIN biz_account p ON p.account_code = v.parent_code AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;
```

9 个末级科目（与 §3.3 ③ / D-14 逐字一致）：

| 编码 | 名称 | 父级 | 级次 | 类别/方向 |
|---|---|---|---|---|
| 4001.01 | 生产成本-直接材料 | 4001 生产成本 | 2 | COST / DEBIT |
| 4001.02 | 生产成本-直接人工 | 4001 | 2 | COST / DEBIT |
| 4001.03 | 生产成本-制造费用 | 4001 | 2 | COST / DEBIT |
| 4101.01 | 制造费用-折旧费 | 4101 制造费用 | 2 | COST / DEBIT |
| 4101.02 | 制造费用-水电费 | 4101 | 2 | COST / DEBIT |
| 4101.03 | 制造费用-修理费 | 4101 | 2 | COST / DEBIT |
| 4101.04 | 制造费用-车间管理费 | 4101 | 2 | COST / DEBIT |
| 4101.09 | 制造费用-其他 | 4101 | 2 | COST / DEBIT |
| 2211.01 | 应付职工薪酬-工资 | 2211 应付职工薪酬 | 2 | LIABILITY / CREDIT |

## 3. 卡面验收逐条对照

### ① 迁移内置检查：三科目历史分录非 0 → RAISE EXCEPTION（不许翻）

迁移里的前置护栏（`V0377 ... up.sql` 第 1 个 DO 块，跑在 UPDATE/INSERT **之前**）：

```sql
FOR rec IN SELECT t.code, a.is_leaf FROM (VALUES ('4001'),('4101'),('2211')) t(code)
             JOIN biz_account a ON a.account_code = t.code AND a.deleted_at IS NULL LOOP
    CONTINUE WHEN rec.leaf_flag <> 1;             -- 已翻过就不再拦（保证幂等）
    SELECT count(*) INTO v_entries FROM biz_voucher_entry e WHERE e.account_code = rec.code;
    IF v_entries > 0 THEN … RAISE EXCEPTION 'V0377 拒绝执行：% 有 % 条分录 …'
END LOOP;
```

真库实测抛出的原文（`02_account_leaf_drill.out` S1）：

```
ERROR:  V0377 拒绝执行：4001 有 1 条分录 —— 这些汇总科目还有历史分录，翻成非末级后这些金额会记在
汇总科目上（报表按末级取数、年结按末级结转，会被漏结 ⇒ 跨年试算不平）。请先把这些分录改记到新的
末级科目（4001.01~03 / 4101.01~04,09 / 2211.01）或把它们红冲/作废后再执行本迁移
```

补充口径：**软删凭证的分录也计入**（红冲/反审核后分录还在表里，年结重算会再看到）。
迁移未声明 `-- +no-transaction`（Rust 测试 ② 断言），所以护栏抛错时整份迁移由引擎单事务回滚
（`backend/vendor/nik-db/src/migrate/exec.rs::is_transactional` 默认 true）—— 不会出现
「翻了一半」的中间态。

### ② 实测：干净库能翻，有分录的库被拦

`02_account_leaf_drill.out`（八段，60 条断言全绿）：

| 段 | 场景 | 结果 |
|---|---|---|
| S1 | 4001 / 4101 / 2211 **逐个**造一条历史分录 → up | 三次全部 `exit=3` 被拦，拒绝信息点名科目与条数；**失败后库零变化**（三科目仍 `is_leaf=1`、9 个末级编码仍未种下） |
| S2 | 干净库（0 分录）→ up | 成功；形态 / 口径两个自检都过；9 个科目形态逐条打印 |
| S3 | 再 up 一次 | 成功；科目形态指纹（md5）与 S2 **逐字节相等**；9 个编码无重复行；全表裂树节点 0 |
| S4 | 往 4001.01 记一条分录 → down | `exit=3` 被拦并点名「4001.01 有 1 条」；9 个末级科目仍在 |
| S5 | 清掉分录 → down | 成功；9 个编码全删、三科目翻回末级、裂树 0、孤立分录 0 |
| S6 | 再 down 一次 + 回滚后重放 up | 都成功；重放后形态指纹与 S2 一致 |
| S7 | 客户已自建「形态正确」的 4001.01 → up | 成功；`ON CONFLICT` 复用它，**不种重复行、不覆盖客户数据**（remark 仍是客户的） |
| S8 | 客户自建的同码科目**挂错父级**（4001.01 ← 4101）→ up | `exit=3` 被拦并点名「4001.01(父级=4101（应为 4001）)」；库零变化、客户那行原样保留；清掉反例后重放，形态指纹与 S2 一致 |

S2 打印出来的实际科目树（摘自演练输出）：

```
2211.01  应付职工薪酬-工资   ← 2211  level=2  LIABILITY/CREDIT  is_leaf=1
4001.01  生产成本-直接材料   ← 4001  level=2  COST/DEBIT  is_leaf=1
4001.02  生产成本-直接人工   ← 4001  level=2  COST/DEBIT  is_leaf=1
4001.03  生产成本-制造费用   ← 4001  level=2  COST/DEBIT  is_leaf=1
4101.01  制造费用-折旧费     ← 4101  level=2  COST/DEBIT  is_leaf=1
4101.02  制造费用-水电费     ← 4101  level=2  COST/DEBIT  is_leaf=1
4101.03  制造费用-修理费     ← 4101  level=2  COST/DEBIT  is_leaf=1
4101.04  制造费用-车间管理费 ← 4101  level=2  COST/DEBIT  is_leaf=1
4101.09  制造费用-其他       ← 4101  level=2  COST/DEBIT  is_leaf=1
```

空库全链重放：**297/297 PASS**（`01_replay_fresh.out`；B1-5 收口时 296，本卡 +1）。

### ③ 9 个末级科目编码与 §3 一致

演练 S2 用三元组比对（编码, 名称, 父级）与设计稿 `not in (values …)`，不符条数 **0**；
Rust 测试 ④ 也把 9 个三元组逐条钉在源码里（设计稿改了而迁移没跟上 → 测试红）。

## 4. 设计依据之外我自己做的判断（都在代码注释里写了理由）

1. **子科目类别/余额方向从父级继承**（`p.account_category` / `p.balance_direction`），
   而不是硬编码 `'COST'/'DEBIT'`。设计 §3.3 ③ 的 INSERT 本来就是这样写的，我把它变成断言（测试④），
   防止后来人「优化」成硬编码 —— 客户把 4101 的类别改过之后，硬编码会让制造费用挂到资产下。
2. **科目名与设计不符只 `RAISE NOTICE`，不 `RAISE EXCEPTION`**。编码/父级/级次/类别/方向/末级位
   是结构（错就得拦），科目名是显示（客户可能叫「生产成本-原材料」），不该挡住升级，但必须看得见。
3. **「9 个编码已被占用且形态不符」单列一条前置护栏**（软删 / 非末级 / 父级或级次不对 → 拒绝并点名）。
   默认情况下 `ON CONFLICT DO NOTHING` 会静默保留客户自己建的 `4001.01`；那会让自检红在一个
   看不懂的地方。现在提前拦住，并告诉运维「本迁移不改已有科目行，请在会计科目里修正后重跑」。
4. **down 只删 biz_account 里那 9 行**（按编码删，含客户手工建的同码行 —— 这几个编码属设计
   命名空间），且两道护栏在前：末级科目有分录 → 拒；三个父级下还有本迁移之外的子科目 → 拒
   （否则出现「末级科目挂子科目」的裂树）。回滚后再 up 的形态指纹与首次完全一致（S6）。
   S7/S8 实证了「客户已自建同码科目」的两种情况：形态正确 → 复用不覆盖；形态不对 → 拒绝并点名。
5. **down 的护栏能识别「分录表已不存在」**（`to_regclass('public.biz_voucher_entry') IS NULL` → 跳过），
   否则凭证模块先回滚时本 down 会直接抛错、不幂等。

## 5. 已知偏差 / 需要下游注意的点

1. **号段偏差**：卡面 V0375 → 实际 V0377（见 §0）。
2. **`2211.01` 与人力资源卡（ARCH9-2）同键**：设计 D-14 明说幂等共存。迁移按版本号升序执行
   ⇒ V0377 一定先跑、行归本卡；后跑的 ARCH9-2 会 `ON CONFLICT DO NOTHING` 跳过。
   本迁移的自检**只断言结果形态**（存在 / 挂 2211 / level=2 / 末级 / 类别方向继承），
   不断言「是本卡刚种的那一行」，避免并行上线时后跑的卡误报红。
3. **迁移给科目行加了 `remark`**（`'ARCH-9 成本核算升级 D-14 播种：成本末级科目（V0377）'`）。
   设计 §3.3 ③ 的 INSERT 没写 remark；house style（V0149/V0158/V0160 的补种科目）都带播种来源。
   回滚按编码删（带了分录护栏），不按 remark 删 —— remark 只做人眼追溯。
4. **`biz_cost_overhead.debit_account_code` / `biz_cost_alloc_rule.account_code` 里的 `'4101.xx' |
   '4001.xx'` 是字符串弱引用**，没有 FK。这些行在 V0377 之前就能写（V0371/V0372 的自检就写了
   `'4101.01'`），科目行是否存在不影响约束。**下游成本模块（B2/B3）落库前应解析到真实科目 id**，
   别继续拿裸字符串当科目存在性证明。
5. **B3-4 结算前的既有开放问题（B1-5 提出，未在本卡范围内）**：V0199 `ck_biz_si_sourceless_pair`
   与 §3.3 的 `ck_biz_si_production` 冲突 ⇒ 无采购单的完工入库插不进去。与本卡无关，仍需一张放宽迁移。
6. **没有改动报表映射**：`report.rs::match_account` 是前缀匹配
   （`code == token || code.starts_with("token.")`），`4001` 的 token 会自动吃到 `4001.01~03`，
   所以 V0183 的「存货」项（`'1401,…,4001,4101'`）一行不改就能把新叶子并进去。
   迁移的口径自检里断言了这条映射**必须还在**（有人删了就会掉出存货报表）。

## 6. 复现命令

```bash
# 静态不变量（11 条，不连库）
cd backend && cargo test --test cost_b1_6_account_leaf_split_migration

# 空库全链重放（应输出 297/297 PASS）
bash scripts/verify/mig_replay_fresh.sh costb16

# 真库六段往返演练（自建临时库，跑完自动删；KEEP_DB=1 可留）
bash docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/02_account_leaf_drill.sh
```
