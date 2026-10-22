//! Fixed launcher verbs and opaque internal bootstrap arguments.

use clap::{Parser, Subcommand};
use std::ffi::OsString;

#[derive(Parser)]
#[command(
    name = "louiselm-launch",
    about = "Run or administer the installed Session launcher"
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
    /// Inspect durable recovery evidence; never infer live supervisor state.
    RecoverInspect {
        /// Exact original consumed authorization identity.
        authorization_id: String,
    },
    /// Explicitly fence and arm a consumed launch for recovery after reboot.
    RecoverArm {
        /// Exact original consumed authorization identity.
        authorization_id: String,
    },
    /// Complete an armed recovery after a different kernel boot.
    RecoverComplete {
        /// Same original authorization identity used when arming.
        authorization_id: String,
    },
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
    fn recovery_inspection_has_an_explicit_read_only_verb() {
        assert!(Cli::try_parse_from(["louiselm-launch", "recover-inspect", "approval"]).is_ok());
    }

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
