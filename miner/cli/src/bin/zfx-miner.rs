//! `zfx-miner`: the public name of the ZFX mining client. Same program as
//! `zpu-cli`. ("ZPU" is reserved for a planned inference server; see docs/REPO_AUDIT.md.)
#[path = "../app.rs"]
mod app;

fn main() {
    app::main()
}
