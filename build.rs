// The skills tree is compiled in (src/skills.rs). include_dir! expands to
// one include_bytes! per file, which rustc lists in its dep-info, so an
// edit to a file it read rebuilds. A file added or removed under skills/
// is not a change to any listed file and would leave the binary stale.
// Naming the directory makes cargo watch its shape as well.
fn main() {
    println!("cargo:rerun-if-changed=skills");
}
