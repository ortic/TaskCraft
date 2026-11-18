use std::io::Read;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
    let config_dir = std::env::var_os("TASKCRAFT_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|d| std::path::PathBuf::from(d).join("taskcraft")))
        .unwrap_or_else(|| home.join(".config").join("taskcraft"));
    let env = taskcraft_cli::Env {
        config_dir,
        cwd: std::env::current_dir().unwrap_or_default(),
        url: std::env::var("TASKCRAFT_URL").ok().filter(|v| !v.is_empty()),
        token: std::env::var("TASKCRAFT_TOKEN").ok().filter(|v| !v.is_empty()),
        browser: true,
        out: Arc::new(|line| println!("{line}")),
        stdin: Arc::new(|| {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }),
    };
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = taskcraft_cli::run(&args, &env).await {
        if e.quiet {
            // clap's own output (help, version, usage errors)
            print!("{}", e.message);
            std::process::exit(e.code);
        }
        eprintln!("tc: {}", e.message);
        std::process::exit(e.code);
    }
}
