// The build script only invokes `napi_build::setup()` when the `napi`
// feature is enabled — that way pure-Rust CI doesn't need a Node
// toolchain.
fn main() {
    #[cfg(feature = "napi")]
    {
        napi_build::setup();
    }
}
