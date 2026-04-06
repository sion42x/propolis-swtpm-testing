// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::v1::instance_spec::PciPath;

/// A socket device that presents a virtio-socket interface to the guest.
#[derive(
    Clone, Copy, Deserialize, Serialize, Debug, PartialEq, Eq, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct VirtioSocket {
    /// The guest's Context ID.
    pub guest_cid: u64,

    /// The PCI path at which to attach this device.
    pub pci_path: PciPath,
}

/// A TPM 2.0 CRB device backed by a running swtpm instance.
#[derive(Clone, Deserialize, Serialize, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TpmCrb {
    /// Path to the swtpm Unix domain socket.
    pub socket_path: String,
}

/// A Crucible-backed volume used to persist swtpm state across VM restarts.
///
/// The volume is accessed directly by propolis (not presented to the guest).
/// On startup, propolis restores swtpm state from this volume into a tmpfs
/// directory, then starts swtpm pointing at that directory. On shutdown,
/// propolis checkpoints the state back to the volume.
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TpmStateDisk {
    /// A serialized `crucible_client_types::VolumeConstructionRequest` for the
    /// TPM state volume. Stored in serialized form for the same reason as
    /// `CrucibleStorageBackend::request_json`.
    pub request_json: String,
}

impl std::fmt::Debug for TpmStateDisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TpmStateDisk")
            .field("request_json", &"<redacted>")
            .finish()
    }
}
