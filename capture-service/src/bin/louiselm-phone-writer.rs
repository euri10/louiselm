//! Manager-owned entrypoint for fixed phone receiver registration.

fn main() {
    if louiselm_capture::run_managed_registration_writer().is_err() {
        eprintln!("managed phone registration unavailable");
        std::process::exit(1);
    }
}
