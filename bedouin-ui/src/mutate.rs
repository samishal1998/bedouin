//! Editing the config from the browser.
//!
//! Three things stand between a page and this machine's config file, and none
//! of them is optional.
//!
//! **Loopback only.** These routes are registered when, and only when, the
//! server bound a loopback socket. `--hostname 0.0.0.0` stays a read-only
//! view. Remote editing is meant to go through an ssh tunnel, which presents
//! as loopback anyway, so nothing is lost by refusing to serve writes to a
//! network that never proved who it was.
//!
//! **A header a form cannot set.** Binding to 127.0.0.1 is not a security
//! boundary against a browser: any page you visit can POST to it. A plain
//! `<form>` can send `application/x-www-form-urlencoded` cross-origin with no
//! preflight, so a drive-by page could add a package with a `script:` field
//! and the next `bedouin apply` would run it under sudo. Requiring
//! `X-Bedouin: 1` defeats that -- a form cannot set a header, and a `fetch`
//! that sets one turns the request into a preflight this server never
//! answers.
//!
//! **The config has to still load.** Every write goes through
//! `run::write_verified`, which backs the file up, writes, re-plans, and puts
//! the original back if the result no longer loads.

// Both arms of these results ARE the response -- a rejection is a 403 body
// exactly as a success is a 200 body. Boxing to satisfy `result_large_err`
// would add an allocation per request to hide a shape that is the point.
#![allow(clippy::result_large_err)]

use crate::api;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bedouin_core::edit::{self, Section};
use bedouin_core::host::{Host, OsHost};
use bedouin_core::run;
use serde::Deserialize;
use std::sync::Arc;

use crate::Ctx;

#[derive(Deserialize)]
pub struct Create {
    pub section: String,
    /// Ordered, so the entry reads the way a person would have typed it.
    pub fields: Vec<[String; 2]>,
}

#[derive(Deserialize)]
pub struct Update {
    pub section: String,
    pub name: String,
    pub key: String,
    /// Raw YAML, not a quoted scalar: this is what lets a conditional stay a
    /// conditional. `{ macos: brew, default: apt }` goes through as written.
    pub value: String,
}

#[derive(Deserialize)]
pub struct Delete {
    pub section: String,
    pub name: String,
    /// For an alias, the package it is scoped to, if any.
    #[serde(default)]
    pub package: Option<String>,
}

/// The check every mutating handler runs first.
fn allowed(ctx: &Ctx, headers: &HeaderMap) -> Result<(), Response> {
    if !ctx.writable {
        return Err(problem(
            StatusCode::FORBIDDEN,
            "This view is read-only: bedouin-ui is not bound to loopback.\n  \
             Editing over a network with no authentication would let anyone \
             who can reach this port change what `bedouin apply` runs.\n  \
             Reach it through an ssh tunnel instead.",
        ));
    }
    // A `<form>` cannot set this, and a cross-origin `fetch` that does turns
    // the request into a preflight nothing here answers.
    if headers.get("x-bedouin").is_none() {
        return Err(problem(
            StatusCode::FORBIDDEN,
            "missing X-Bedouin header -- this request did not come from the \
             bedouin page",
        ));
    }
    // Defence in depth: a browser always sends Origin on a non-GET. When it
    // is there it has to be us.
    if let Some(o) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        // We are never on port 80, so a browser's Origin for us always carries
        // the port. Prefix matching keeps this something anyone can check by
        // eye, which a hand-rolled host parser would not be.
        let ours = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
            .iter()
            .any(|p| o.starts_with(p));
        if !ours {
            return Err(problem(
                StatusCode::FORBIDDEN,
                "this request came from another origin",
            ));
        }
    }
    Ok(())
}

fn section_of(s: &str) -> Result<Section, Response> {
    Section::parse(s).ok_or_else(|| {
        problem(
            StatusCode::BAD_REQUEST,
            &format!("`{s}` is not a section this can edit"),
        )
    })
}

/// Read the config, apply one edit to its text, write it back verified.
///
/// The lock is held across the whole read-modify-write, so two tabs cannot
/// each read the same text and write over one another.
fn apply_edit(
    ctx: &Ctx,
    edit: impl FnOnce(&str) -> Result<String, bedouin_core::schema::ConfigError>,
) -> Result<Response, Response> {
    // ponytail: one lock for the whole config; it is one file.
    let _held = ctx.write.lock().map_err(|_| {
        problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "a previous edit panicked; restart bedouin ui",
        )
    })?;

    let host = OsHost::new();
    let loaded = run::load_only(&host, ctx.config.as_deref(), &ctx.cwd)
        .map_err(|e| problem(StatusCode::BAD_REQUEST, &e.to_string()))?
        .0;
    let entry = loaded.entry.clone();
    let text = host
        .read(&entry)
        .ok()
        .flatten()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .ok_or_else(|| {
            problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("cannot read {}", entry.display()),
            )
        })?;

    let after = edit(&text).map_err(|e| problem(StatusCode::CONFLICT, &e.to_string()))?;
    run::write_verified(&host, &entry, &after, ctx.config.as_deref(), &ctx.cwd)
        .map_err(|e| problem(StatusCode::CONFLICT, &e.to_string()))?;

    // The page redraws from the snapshot, so hand it back rather than making
    // it ask again and race the next edit.
    match api::snapshot(ctx.config.as_deref(), &ctx.cwd, ctx.writable) {
        Ok(s) => Ok(Json(s).into_response()),
        Err(e) => Err(problem(StatusCode::INTERNAL_SERVER_ERROR, &e)),
    }
}

/// Whatever `apply_edit` returns, on the blocking pool: it reads files and
/// re-plans, which means subprocesses.
async fn blocking(
    ctx: Arc<Ctx>,
    f: impl FnOnce(&Ctx) -> Result<Response, Response> + Send + 'static,
) -> Response {
    match tokio::task::spawn_blocking(move || f(&ctx)).await {
        Ok(Ok(r)) | Ok(Err(r)) => r,
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub async fn create(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Create>,
) -> Response {
    if let Err(r) = allowed(&ctx, &headers) {
        return r;
    }
    blocking(ctx, move |ctx| {
        let section = section_of(&body.section)?;
        apply_edit(ctx, move |text| {
            let fields: Vec<(&str, &str)> = body
                .fields
                .iter()
                .map(|kv| (kv[0].as_str(), kv[1].as_str()))
                .collect();
            edit::add_entry(text, section, &fields)
        })
    })
    .await
}

pub async fn update(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Update>,
) -> Response {
    if let Err(r) = allowed(&ctx, &headers) {
        return r;
    }
    blocking(ctx, move |ctx| {
        // Cleared, not set to nothing. `version:` with an empty value is
        // null, and null is a value -- an absent `version:` means "latest",
        // a null one is a config saying something nobody meant.
        let cleared = body.value.trim().is_empty();
        if body.section == "aliases" {
            return apply_edit(ctx, move |text| {
                if cleared {
                    edit::remove_alias(text, None, &body.name)
                } else {
                    edit::set_alias(text, None, &body.name, &body.value)
                }
            });
        }
        let section = section_of(&body.section)?;
        apply_edit(ctx, move |text| {
            if cleared {
                edit::unset_field(text, section, &body.name, &body.key)
            } else {
                edit::set_field(text, section, &body.name, &body.key, &body.value)
            }
        })
    })
    .await
}

pub async fn delete(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Delete>,
) -> Response {
    if let Err(r) = allowed(&ctx, &headers) {
        return r;
    }
    blocking(ctx, move |ctx| {
        if body.section == "aliases" {
            return apply_edit(ctx, move |text| {
                edit::remove_alias(text, body.package.as_deref(), &body.name)
            });
        }
        let section = section_of(&body.section)?;
        apply_edit(ctx, move |text| {
            edit::remove_entry(text, section, &body.name)
        })
    })
    .await
}

fn problem(code: StatusCode, e: &str) -> Response {
    (code, Json(serde_json::json!({ "error": e }))).into_response()
}
