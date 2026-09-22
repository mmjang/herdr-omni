fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("herdr-omni {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help") {
        println!("Herdr Omni\n\nUsage: herdr-omni [--demo] [--version]\n\nTab: categories; Ctrl+F: transcripts; Ctrl+O: expand preview; Ctrl+Y: toggle preview; F6: next pane; Esc: back/close");
        return;
    }
    if let Err(error) = herdr_omni::ui::run(args.iter().any(|a| a == "--demo")) {
        eprintln!("Herdr Omni: {error}");
        std::process::exit(1);
    }
}
