use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use taskcraft_runner::Runner;

#[derive(Parser)]
#[command(
    name = "taskcraft-runner",
    version,
    about = "Runs TaskCraft's AI coding agents: claims a run, works on a checkout of the repository, hands the result back"
)]
struct Cli {
    /// Where the runner keeps its server and token (default: ~/.config/taskcraft/runner.json)
    #[arg(long, global = true, env = "TASKCRAFT_RUNNER_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register with a TaskCraft server, using the one-time token from Administration → Agents
    Register {
        /// The server's address, e.g. https://tasks.example.com
        #[arg(long)]
        server: String,
        /// The registration token (shown once when the runner was added)
        #[arg(long, env = "TASKCRAFT_RUNNER_REGISTRATION", hide_env_values = true)]
        token: String,
        /// How the runner is called in Administration
        #[arg(long)]
        name: Option<String>,
    },
    /// Wait for runs and work on them
    Run {
        /// Work on one run, then exit (start the runner in a fresh container per run)
        #[arg(long)]
        once: bool,
        /// Let the agent run commands in the checkout (tests, builds). Only inside a container that holds nothing else
        #[arg(long, env = "TASKCRAFT_RUNNER_ALLOW_COMMANDS")]
        allow_commands: bool,
        /// Hand the work to this command instead of the built-in agent: it runs in the checkout with
        /// TASKCRAFT_TASK_FILE (what to do) and TASKCRAFT_RESULT_FILE (where to write the summary or answer)
        #[arg(long, env = "TASKCRAFT_RUNNER_COMMAND")]
        command: Option<String>,
        /// Seconds between looking for work
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// How many steps the built-in agent takes at most
        #[arg(long, default_value_t = 40)]
        max_steps: usize,
        /// The server's address (instead of the registered one)
        #[arg(long, env = "TASKCRAFT_RUNNER_SERVER")]
        server: Option<String>,
        /// The runner's token (instead of the registered one)
        #[arg(long, env = "TASKCRAFT_RUNNER_TOKEN", hide_env_values = true)]
        token: Option<String>,
    },
}

fn config_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("taskcraft").join("runner.json")
    })
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let path = config_path(cli.config);
    let result: Result<(), String> = async {
        match cli.command {
            Command::Register { server, token, name } => {
                let server = server.trim_end_matches('/').to_string();
                let (name, token) = taskcraft_runner::register(&server, &token, name.as_deref()).await?;
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(&path, serde_json::json!({"server": server, "token": token}).to_string())
                    .map_err(|e| format!("can't write {}: {e}", path.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                }
                println!("Registered as {name}. Start it with: taskcraft-runner run");
                Ok(())
            }
            Command::Run { once, allow_commands, command, interval, max_steps, server, token } => {
                let saved: serde_json::Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                let server = server
                    .or_else(|| saved["server"].as_str().map(str::to_string))
                    .ok_or("not registered: run `taskcraft-runner register --server … --token …` first")?;
                let token = token.or_else(|| saved["token"].as_str().map(str::to_string)).ok_or("not registered: no runner token")?;
                let mut runner = Runner::new(&server, &token);
                runner.allow_commands = allow_commands;
                runner.command = command;
                runner.max_steps = max_steps;
                runner.out = Arc::new(|line| println!("{line}"));
                println!("Waiting for runs from {server} …");
                loop {
                    match runner.work_once().await {
                        Ok(true) if once => return Ok(()),
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(e) => eprintln!("taskcraft-runner: {e}"),
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(interval.clamp(1, 300))).await;
                }
            }
        }
    }
    .await;
    if let Err(e) = result {
        eprintln!("taskcraft-runner: {e}");
        std::process::exit(1);
    }
}
