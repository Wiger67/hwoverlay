//! Embeds the exe icon (assets/hwoverlay.ico) as a Windows resource.
//! Resources are only read when Explorer asks for them, so this costs no runtime memory or CPU.

use embed_resource::CompilationResult;

fn main() {
    println!("cargo:rerun-if-changed=assets/hwoverlay.rc");
    println!("cargo:rerun-if-changed=assets/hwoverlay.ico");
    match embed_resource::compile("assets/hwoverlay.rc", embed_resource::NONE) {
        // No resource compiler (rc.exe / windres) found: still build, just without an icon.
        CompilationResult::NotAttempted(why) => println!("cargo:warning=exe icon not embedded: {why}"),
        result => result.manifest_optional().expect("compiling the icon resource failed"),
    }
}
