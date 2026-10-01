//! Fixed launcher verbs and opaque internal bootstrap arguments.

use clap::{Parser, Subcommand};
use std::ffi::OsString;

#[derive(Parser)]
#[command(
    name = "louiselm-launch",
    about = "Run, prepare or certify the installed Session launcher"
)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(super) enum Command {
    /// Launch one authorized Session.
    Run,
    /// Prepare installed launcher authority.
    Prepare,
    /// Certify the installed host profile.
    Certify,
    /// Clean expired retained storage; root authority required.
    Cleanup,
    #[command(
        name = "__sandbox_bootstrap",
        hide = true,
        disable_help_flag = true,
        trailing_var_arg = true
    )]
    Bootstrap {
        #[arg(num_args = 0.., allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
    #[command(name = "__sender-guard-object", hide = true, disable_help_flag = true)]
    SenderGuardObject,
    #[command(name = "__conformance-worker", hide = true, disable_help_flag = true)]
    ConformanceWorker,
    #[command(name = "__conformance-probe", hide = true, disable_help_flag = true)]
    ConformanceProbe,
    #[command(
        name = "__conformance-guard-probe",
        hide = true,
        disable_help_flag = true
    )]
    ConformanceGuardProbe,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_bootstrap_arguments_keep_separators_and_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let payload = vec![
            OsString::from("/bin/bwrap"),
            OsString::from("--unshare-all"),
            OsString::from("--"),
            OsString::from("/bin/worker"),
            OsString::from("--help"),
            OsString::from_vec(vec![0xff]),
        ];
        let args = [
            OsString::from("louiselm-launch"),
            OsString::from("__sandbox_bootstrap"),
        ]
        .into_iter()
        .chain(payload.clone());
        let parsed = Cli::try_parse_from(args);
        assert!(
            matches!(parsed, Ok(Cli {command: Command::Bootstrap {arguments}}) if arguments == payload)
        );
    }
}
