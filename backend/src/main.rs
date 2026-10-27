//! 程序入口：`cargo run` 的起点，仅负责转发到 [`cailu::run`]。

/// 服务入口。
///
/// 启动期错误无法依赖日志组件输出（日志初始化本身也可能失败），因此直接写入标准错误，
/// 并以非零退出码结束进程，便于容器编排与进程守护识别失败。
#[tokio::main]
async fn main() {
    if let Err(err) = cailu::run().await {
        // `FATAL:` 前缀是约定的运维检索锚点（journalctl / docker logs 里 grep 即可），
        // 启动期错误一律以它开头，避免被业务日志淹没。
        eprintln!("FATAL: 服务启动失败: {err}");
        std::process::exit(1);
    }
}
