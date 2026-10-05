fn main() {
    if let Err(err) = fairspoken_lib::run_transcription_host() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
