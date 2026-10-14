fn main() {
    // 前端还没构建时（比如刚克隆下来先跑 cargo test），Tauri 也要求 frontendDist 目录存在：放一个占位页
    let dist = std::path::Path::new("../dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(dist).expect("创建 app/dist");
        std::fs::write(dist.join("index.html"), "<!doctype html><meta charset=utf-8><title>Record</title><p>前端还没有构建：在 app 目录运行 pnpm install && pnpm build。</p>").expect("写占位页");
    }
    tauri_build::build()
}
