fn main() {
    if let Err(err) = multivoice_tauri_lib::run_transcription_host() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
