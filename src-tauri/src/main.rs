#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;

/// AginxBrain — Agent 的 AI 大脑
#[derive(Parser, Debug)]
#[command(name = "aginxbrain", version, about)]
struct Args {
    /// 服务器模式：不创建桌面窗口，前台运行 HTTP 服务器
    #[arg(long)]
    server: bool,

    /// 监听端口（覆盖 config.yaml 中的 port）
    #[arg(short, long)]
    port: Option<u16>,

    /// 监听地址（覆盖 config.yaml 中的 host）
    #[arg(short = 'H', long)]
    host: Option<String>,
}

fn main() {
    let args = Args::parse();

    if args.server {
        aginxbrain_lib::run_server(args.port, args.host);
    } else {
        #[cfg(feature = "desktop")]
        aginxbrain_lib::run_desktop();
        #[cfg(not(feature = "desktop"))]
        {
            eprintln!("This binary was built without desktop support. Use --server mode.");
            std::process::exit(1);
        }
    }
}
