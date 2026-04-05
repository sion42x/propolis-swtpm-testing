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
