// The app icon (Explorer, taskbar, Alt+Tab, Task Manager) comes from the exe's resources.
fn main() {
    println!("cargo:rerun-if-changed=assets/brisklark.ico");
    embed_resource::compile("assets/brisklark.rc", embed_resource::NONE).manifest_optional().unwrap();
}
