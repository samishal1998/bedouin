//! `bedouin-ui` — the web UI, served.
//!
//! A sidecar, not a subcommand. It carries an HTTP stack and the built web
//! assets, and the bootstrap binary carries neither: `bedouin ui` finds this
//! on disk (fetching it once if it is absent) and `exec`s it.
//!
//! Exec, not spawn, and that is load-bearing. `apply` runs `sudo -v` with
//! inherited stdin; replacing the process means this one owns the terminal
//! you launched it from, so sudo prompts *there* rather than needing a
//! password to cross an HTTP boundary.

mod api;

use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use bedouin_core::host::OsHost;
use bedouin_core::run;
use std::path::PathBuf;
use std::sync::Arc;

struct Ctx {
    config: Option<PathBuf>,
    cwd: PathBuf,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut config: Option<PathBuf> = None;
    let mut port: u16 = 7777;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!("bedouin-ui {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            "-c" | "--config" => config = args.next().map(PathBuf::from),
            "-p" | "--port" => {
                port = match args.next().and_then(|p| p.parse().ok()) {
                    Some(p) => p,
                    None => {
                        eprintln!("bedouin-ui: --port needs a number");
                        return std::process::ExitCode::FAILURE;
                    }
                }
            }
            other => {
                eprintln!("bedouin-ui: unknown argument `{other}`");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ctx = Arc::new(Ctx { config, cwd });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/state", get(state))
        .route("/api/plan", get(plan))
        .route("/api/facts", get(facts))
        .with_state(ctx);

    // Loopback only. This serves a machine's configuration and can change it;
    // it is not a thing to put on an interface by accident.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bedouin-ui: cannot listen on {addr}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("bedouin-ui {} — http://{addr}", env!("CARGO_PKG_VERSION"));
    println!("sudo will prompt here, in this terminal, not in the browser.");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("bedouin-ui: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Planning is blocking and touches the filesystem and subprocesses, so it
/// goes on the blocking pool rather than stalling the reactor.
async fn outcome(ctx: Arc<Ctx>) -> Result<run::Outcome, String> {
    tokio::task::spawn_blocking(move || {
        let host = OsHost::new();
        run::plan(&host, ctx.config.as_deref(), &ctx.cwd).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Everything the page draws, in one call. See `api::snapshot`.
async fn state(State(ctx): State<Arc<Ctx>>) -> impl IntoResponse {
    let r =
        tokio::task::spawn_blocking(move || api::snapshot(ctx.config.as_deref(), &ctx.cwd)).await;
    match r {
        Ok(Ok(s)) => Json(s).into_response(),
        Ok(Err(e)) => problem(e),
        Err(e) => problem(e.to_string()),
    }
}

async fn plan(State(ctx): State<Arc<Ctx>>) -> impl IntoResponse {
    match outcome(ctx).await {
        Ok(o) => Json(serde_json::json!({
            "items": o.plan.items,
            "warnings": o.plan.warnings,
            "pruned": o.plan.pruned,
        }))
        .into_response(),
        Err(e) => problem(e),
    }
}

async fn facts(State(ctx): State<Arc<Ctx>>) -> impl IntoResponse {
    match outcome(ctx).await {
        Ok(o) => {
            let mut f = o.facts;
            // Names only, the same rule `bedouin facts` follows: this output
            // ends up in screenshots.
            f.env = f.env.keys().map(|k| (k.clone(), "<set>".into())).collect();
            Json(f).into_response()
        }
        Err(e) => problem(e),
    }
}

fn problem(e: String) -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": e })),
    )
        .into_response()
}

/// The Astro build, inlined into one file and embedded here. Served from
/// memory rather than from disk beside the binary: this binary is fetched
/// into a directory of its own, so anything it expected to find next to
/// itself would not be there.
async fn index() -> Html<&'static str> {
    Html(include_str!("../web/dist/index.html"))
}
