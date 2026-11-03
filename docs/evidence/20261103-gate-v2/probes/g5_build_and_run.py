#!/usr/bin/env python3
"""独立复核用：造 G5 样例代码（自写，不引用仓库 fixture），跑 ./scripts/lint/changelog_in_tx.py 的**真实 CLI**。

用法：python3 g5_build_and_run.py
"""
import json
import os
import shutil
import subprocess
import os
import sys
from pathlib import Path

BASE = Path(os.environ.get("PROBE_DIR", "/tmp")) / "g5fix"
SRC = BASE / "backend" / "src"
SCRIPT = "/opt/cloudpivot/scripts/lint/changelog_in_tx.py"

# 陪跑文件：把判定面撑到 >= MIN_CALL_SITES(27)，这样 CLI 的防假绿下限不会掩盖真正的问题
PAD = "\n".join(
    [
        "pub async fn pad_%02d(db: &Db) -> Result<(), ApiError> {" % i
        + " db.transaction(|tx| async move { changelog::record(&tx, ctx, target, input).await }).await }"
        for i in range(30)
    ]
)

CASES = {
    # ① 函数顶层 changelog::record(db(), …) → 期望报红
    "case01_top_level_db.rs": """
pub async fn top_level(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    changelog::record(db(), ctx, target, input).await?;
    Ok(())
}
""",
    # ② tokio::spawn(async { changelog::record(&tx, …) }) → 期望报红（句柄越过事务边界）
    "case02_spawn.rs": """
pub async fn spawn_task(tx: &Transaction, ctx: &ChangeCtx) -> Result<(), ApiError> {
    tokio::spawn(async move {
        changelog::record(&tx, ctx, target, input).await.ok();
    });
    Ok(())
}
""",
    # ③ transaction(|tx| async move { changelog::record(db(), …) }) → 期望报红（第二个事务入口）
    "case03_tx_but_db.rs": """
pub async fn run(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    db.transaction(|tx| async move {
        changelog::record(db(), ctx, target, input).await
    })
    .await
}
""",
    # ④ 本地 fn record 遮蔽的裸调 + 同文件事务外的 changelog::record(ex,…)
    #    期望：裸调不报红、真调用报红（恰好 1 条）
    "case04_shadow.rs": """
fn record(rule: &str, outcome: &str) -> u8 {
    if rule.is_empty() { 0 } else { 1 }
}

pub async fn run(conn: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    record("r1", "ok");
    record("r2", "ok");
    changelog::record(conn, ctx, target, input).await?;
    Ok(())
}
""",
    # ⑤ fn f(ex: &Transaction, …) 里 changelog::record(ex, …) → 期望判绿
    "case05_typed_param.rs": """
pub async fn cancel(ex: &Transaction, ctx: &ChangeCtx) -> Result<(), ApiError> {
    for _victim in &victims {
        changelog::record(ex, ctx, target, input).await?;
    }
    Ok(())
}
""",
    # ---- 以下是我额外加的「放宽是否过度」探针 ----
    # ⑥ 方法调用（缓存闸门）→ 期望不计入判定面
    "case06_method_call.rs": """
pub fn bump(gate: &InvalidationGate) {
    TREE_CACHE_GATE.record(true);
    GATE.record(false);
}
""",
    # ⑦ 别名导入 + 别名裸调（db() 直呼）→ 我怀疑这条能绕过判定面
    "case07_aliased_import.rs": """
use crate::business::changelog::record as audit_write;

pub async fn sneaky(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    audit_write(db(), ctx, target, input).await?;
    Ok(())
}
""",
    # ⑧ 原样导入 changelog::record 后裸调（非 changelog 模块内）→ 我怀疑只提示不判红
    "case08_plain_import.rs": """
use crate::business::changelog::record;

pub async fn sneaky2(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    record(db(), ctx, target, input).await?;
    Ok(())
}
""",
    # ⑨ 泛型实参（turbofish）→ 限定名正则可能漏掉
    "case09_turbofish.rs": """
pub async fn generic(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    changelog::record::<()>(db(), ctx, target, input).await?;
    Ok(())
}
""",
    # ⑩ 限定名带 crate:: 前缀 + 显式事务句柄 begin() → 期望判绿（原有合规形态）
    "case10_crate_begin.rs": """
pub async fn explicit(conn: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    let tx = conn.begin().await?;
    crate::changelog::record(&tx, ctx, target, input).await?;
    Ok(())
}
""",
    # ⑪ 事务闭包内，但首参既不是 tx 也不是 Transaction 类型 → 期望报红
    "case11_tx_but_other_handle.rs": """
pub async fn other(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    db.transaction(|tx| async move {
        changelog::record(other_pool, ctx, target, input).await
    })
    .await
}
""",
}


def main() -> int:
    if BASE.exists():
        shutil.rmtree(BASE)
    SRC.mkdir(parents=True)
    (SRC / "_pad.rs").write_text(PAD + "\n", encoding="utf-8")
    for name, body in CASES.items():
        (SRC / name).write_text(body.lstrip("\n"), encoding="utf-8")

    cmd = [sys.executable, SCRIPT, "--root", str(BASE), "--src", str(SRC), "--list"]
    out = subprocess.run(cmd, capture_output=True, text=True)
    print("$ " + " ".join(cmd))
    print(f"EXIT={out.returncode}")
    print(out.stdout)
    if out.stderr:
        print("--- stderr ---")
        print(out.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
