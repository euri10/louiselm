//! Guest characterization uses the exact installed probe implementation.

pub use louiselm_skills::conformance::installed::probes::{Attack, Endpoint, Probe};
pub use louiselm_skills::conformance::{Observation, Outcome};

pub fn serve() {
    louiselm_skills::conformance::installed::serve_probe().expect("owned probe exchange succeeds");
}
