# HR-16 员工薪资标准 CRUD + 生效区间不重叠 — 实测证据

**卡**：`t_09501658`（批 2 员工档案）· **日期**：2026-11-12 · **主机**：99（工作仓 `/opt/cloudpivot`）

设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`

| 章节 | 本卡用到的部分 |
|---|---|
| §3.1 ③ | `biz_employee_salary` 的 DDL（10 张表已在真库跑通，本卡零迁移） |
| §4.9 A3 | 同一 `employee_id + item_code` 的区间不许相交（BLOCKER 巡检断言） |
| §5.2 #16 / #17 | 读（现行 + 历史）/ 写（调薪、收口旧行） |
| §3.1 ③ 注 | 「调薪 = 旧行 `effective_to = 新生效日 - 1` + 插新 `effective_from`，区间不许重叠」 |

## 验收项对照

| 卡面验收 | 证据 |
|---|---|
| **区间重叠被拒（含边界相等）** | `01_unit_and_db_tests.out`：`equal_boundary_is_rejected`（旧止 06-30 撞新起 06-30 → 409+68007，改 07-01 立刻放行）、`overlapping_range_is_rejected_and_rolled_back`（落在已收口区间内部 → 409+68007 且**整笔回滚**）；`06_http_smoke.out` 第 5/6 组（真 HTTP 409 + 报文点名两条区间） |
| **单测覆盖：不重叠 / 重叠 / 相邻** | `01_unit_and_db_tests.out`：`disjoint_ranges_do_not_overlap`、`adjacent_ranges_do_not_overlap`、`nested_ranges_overlap`、`equal_boundaries_overlap`（纯函数 `ranges_overlap`）；真库 `adjacent_ranges_are_accepted`、`disjoint_ranges_are_accepted` |
| 后端改动带 `#[cfg(test)] mod tests` | `salary_model` 11 条、`salary_dto` 10 条、`salary_handler` 4 条（共 25 条纯单测）+ 10 条真库用例 |
| 迁移：幂等 + `.down.sql` + 不改已应用迁移 | **零迁移**（表由 HR-01 的 `V0383__hr_tables` 建；本卡只读不改） |
| 提交前 `cargo fmt` | `05_fmt_check.out`（`cargo fmt --check` 空输出 = 逐字节一致）|
| 前端 `pnpm typecheck && pnpm build` | 本卡未改前端；仍跑了一遍全绿（`TC_EXIT=0` / `BUILD_EXIT=0`） |

## 实测数字

| 项 | 结果 | 日志 |
|---|---|---|
| 纯单测 + 真库用例（`business::hr::salary`） | **36 passed / 0 failed**（含 10 条真库；无库时应 SKIP 不假绿） | `01_unit_and_db_tests.out` |
| 真库用例（`HR_DB_URL` 指向 `hr16_card`） | **10 passed / 0 failed** | `02_db_tests.out` |
| 全量 lib（`cargo test --lib`） | **4993 passed / 0 failed**（HR-15 收尾基线 4957，本卡 +36） | `03_full_lib.out` |
| `cargo clippy --all-targets -- -D warnings` | EXIT=0（零警告） | `04_clippy.out` |
| `cargo fmt --check` | EXIT=0 | `05_fmt_check.out` |
| HTTP 冒烟（真实例 + 真库） | **通过 48 项 / 失败 0 项** | `06_http_smoke.out` |
| 残留自检（夹具 / 库 / 端口） | 全 0；`hr16_card` / `hr16_http` 两个演练库已 `dropdb` | `07_residue.out` |

## 三条写路径（回执里能看出来）

| 意图 | 载荷 | 库里发生什么 |
|---|---|---|
| 现行标准（调薪 / 首录） | `amount` + 无 `effective_to` | 插 `[effective_from, 至今)`；旧的**开放行**收口到 `effective_from - 1`（回执 `closed[]`） |
| 补登历史 / 预置计划 | `amount` + `effective_to` | 插明确区间；**一条历史都不动**，与任何版本相交即 409 |
| 修正错金额 | 同 `effective_from` 再提交 | 原行 `UPDATE amount`（`uk_biz_emp_salary` 决定同一天只有一行）；幂等重放 |
| 取消某项 | `amount` 缺省 | 覆盖生效日的行收口到 `-1 天`，起点 ≥ 生效日的行软删（回执 `canceled[]`） |

## 复现命令

```bash
# 1) 空库首启（自动跑完全部迁移）：造 hr16_card
su - postgres -c "psql -c \"CREATE DATABASE hr16_card OWNER alun\""
cd backend && NIK_PROFILE=test NIK_DATABASE_NAME=hr16_card NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18098 \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET='<44+ 字符>' \
  CLOUDPIVOT_STORAGE_MASTER_KEY='<64 hex>' X_CONFIG_SECRET_KEY='<任意>' ./target/debug/cloudpivot
# 迁移完成后停实例，跑真库用例
HR_DB_URL=postgres://alun:alun@127.0.0.1:5432/hr16_card \
  cargo test --lib business::hr::salary_db -- --test-threads=1 --nocapture

# 2) HTTP 冒烟（实例 + 同一个库；薪资项夹具由脚本用 psql 造，写接口归 HR-31）
BASE_URL=http://127.0.0.1:18098 ADMIN_PASSWORD='Admin@123456' \
  PSQL_URL=postgres://alun:alun@127.0.0.1:5432/hr16_http PGPASSWORD=alun \
  bash ../scripts/verify/hr/salary_http_smoke.sh
```

## 与设计的偏差（已在卡内上报）

1. `GET #16` 的权限码取 `salary:detail` 单一码（设计写 `employee:detail` + `salary:detail`）：设计 §3.2 ⑤ 给
   `FINANCE_MANAGER` 的只有薪资 4 码，两个都要会把财务经理 403；数据范围仍由员工侧探针 404 裁剪。
2. 一个 `POST #17` 覆盖增 / 改 / 删（设计只给两条接口，没有独立的「删除标准」路由）：`amount` 缺省 =
   取消该薪资项自生效日起的标准。多开 `DELETE` 会让「写标准」出现两个入口、两套收口逻辑。
3. `items[].effective_to` 为新增可选入参：设计只写「调薪收口旧行」，但补登历史 / 预置计划必须有明确区间
   （否则「更早的至今」会与更晚的版本重叠、只能拒）。
4. 新增两个业务码 `68007`（区间重叠）/ `68008`（薪资标准业务闸）—— 共享文件 additive 改动。
5. 「已收口但延伸过新生效日的历史行」按冲突处理（409）而不是静默收口：明确区间一旦落库就是审计口径，
   隐式改历史会让「当时定的截止日」凭空变短。
