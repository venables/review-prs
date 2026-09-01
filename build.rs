// The skills tree is compiled in (src/skills.rs). include_dir! records each
// file it read, so an edit rebuilds, but a file added or removed under
// skills/ is not a change to any recorded file and would leave the binary
// stale. Naming the directory makes cargo watch its shape as well.
fn main() {
    println!("cargo:rerun-if-changed=skills");
}
