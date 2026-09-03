fn main() {
    match louiselm_skills::cli::run() {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("louiselm-skills: {error}");
            std::process::exit(1);
        }
    }
}
