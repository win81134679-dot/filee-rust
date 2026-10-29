//! Embeds assets/filee-rust.rc (the icon and version information) into filee-rust.exe with the Windows SDK's rc.exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/filee-rust.rc");
    println!("cargo:rerun-if-changed=assets/filee.ico");
    embed_resource::compile("assets/filee-rust.rc", embed_resource::NONE).manifest_optional().unwrap();
}
