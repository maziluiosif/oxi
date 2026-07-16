#[cfg(target_os = "windows")]
fn main() {
    winresource::WindowsResource::new()
        .set_icon("assets/windows/app-icon.ico")
        .compile()
        .expect("failed to embed Windows resources");
}

#[cfg(not(target_os = "windows"))]
fn main() {}
