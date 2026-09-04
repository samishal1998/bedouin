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
    let mut hostname = String::from("127.0.0.1");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!("bedouin-ui {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            "-c" | "--config" => config = args.next().map(PathBuf::from),
            "-H" | "--hostname" | "--host" => match args.next() {
                Some(h) => hostname = h,
                None => {
                    eprintln!("bedouin-ui: --hostname needs an address");
                    return std::process::ExitCode::FAILURE;
                }
            },
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

    // Loopback by default: this serves a machine's configuration, and that is
    // not a thing to put on an interface by accident. `--hostname` is how you
    // say you meant it.
    let listener = match tokio::net::TcpListener::bind((hostname.as_str(), port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bedouin-ui: cannot listen on {hostname}:{port}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let bound = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| format!("{hostname}:{port}"));

    println!("bedouin-ui {} — http://{bound}", env!("CARGO_PKG_VERSION"));
    if !is_loopback(&listener) {
        // Said plainly, because it is true and easy to not think about: there
        // is no authentication here. Anyone who can reach this port can read
        // this machine's configuration, its package list, its paths and the
        // NAMES of the environment variables it reads.
        println!();
        println!("  Reachable from the network, and there is no authentication.");
        println!("  Anyone who can reach {bound} can read this config and this");
        println!("  machine's facts. Bind it to something only you can reach,");
        println!("  or put it behind something that asks who you are.");
        println!();
    }
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

/// Whether what we actually bound is loopback -- asked of the socket rather
/// than of the string, because `localhost`, `::1` and `127.0.0.2` are all
/// loopback and none of them is spelled `127.0.0.1`.
fn is_loopback(l: &tokio::net::TcpListener) -> bool {
    l.local_addr()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn bound(host: &str) -> tokio::net::TcpListener {
        tokio::net::TcpListener::bind((host, 0))
            .await
            .unwrap_or_else(|e| panic!("bind {host}: {e}"))
    }

    #[tokio::test]
    async fn loopback_is_decided_by_the_socket_not_the_spelling() {
        // `localhost` resolves to ::1, `127.0.0.2` is loopback too, and
        // neither is spelled `127.0.0.1`. Matching the string would warn on
        // one and stay silent on the other.
        for h in ["127.0.0.1", "localhost", "127.0.0.2"] {
            assert!(is_loopback(&bound(h).await), "{h} is loopback");
        }
    }

    #[tokio::test]
    async fn binding_every_interface_is_not_loopback() {
        // The case the warning exists for: 0.0.0.0 reaches whatever the
        // machine is reachable on, which on a rented box is the internet.
        assert!(!is_loopback(&bound("0.0.0.0").await));
    }
}
