//! `lanshare whoami`: this device's name, id and certificate fingerprint, to compare with what other devices show.

use super::Context;
use crate::output;

/// Prints this device's identity.
pub fn run(context: &Context) {
    output::print_identity(&context.device_info());
}
