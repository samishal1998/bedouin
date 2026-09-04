//! Makes sure there is something to `include_str!`.
//!
//! The real page is built by `npm run build` in `web/` and CI does that before
//! cargo. Without this, a clone with no node toolchain fails to compile with a
//! missing-file error that says nothing about why — so a placeholder is
//! written instead, and it says exactly what is missing and how to get it.

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("web/dist/index.html");
    println!("cargo:rerun-if-changed=web/dist/index.html");
    if dist.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(dist.parent().unwrap());
    let _ = std::fs::write(
        &dist,
        "<!doctype html><meta charset=utf-8><title>bedouin</title>\
         <body style=\"font:14px ui-monospace,monospace;padding:2rem\">\
         <p>The web interface was not built into this binary.</p>\
         <p>Build it with <code>npm ci &amp;&amp; npm run build</code> in \
         <code>bedouin-ui/web</code>, then rebuild.</p>\
         <p>The API is live regardless: <a href=\"/api/state\">/api/state</a>.</p>",
    );
}
