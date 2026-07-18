//! Test fixture standing in for a real java.exe: prints a marker with its argv
//! and its own path (so tests can prove WHICH installed copy ran), then exits
//! with `FAKE_JAVA_EXIT` (default 0). `FAKE_JAVA_SLEEP_MS` keeps the process
//! alive first — tests use it to hold the exe image mapped while a shim
//! replacement happens over it.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("fake-java argv=[{}]", args.join(" "));
    if let Ok(exe) = std::env::current_exe() {
        println!("fake-java exe={}", exe.display());
    }
    if let Some(ms) = std::env::var("FAKE_JAVA_SLEEP_MS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let code = std::env::var("FAKE_JAVA_EXIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    std::process::exit(code);
}
