fn main() {
    // If vergen can't describe the commit (no tags, shallow clone, no git repo),
    // fall back to the static version in Cargo.toml so the build always succeeds.
    vergen_git2::Emitter::default()
        .default_on_error()
        .emit()
        .expect("Failed to emit build-time version information");
}
