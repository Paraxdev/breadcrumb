fn main() {
    // Note: the debug build's cli::run has a frame near the 1 MB main thread stack on Windows, give it room.
    let code = std::thread::Builder::new().stack_size(8 * 1024 * 1024).spawn(breadcrumb::cli::main_entry).expect("starting the main thread").join().unwrap_or(101);
    std::process::exit(code);
}
