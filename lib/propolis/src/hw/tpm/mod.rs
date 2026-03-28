// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TPM 2.0 emulation via the CRB (Command Response Buffer) interface.
//!
//! The CRB interface is the standard guest-facing register set defined by the
//! TCG PC Client Platform TPM Profile spec. It lives at a fixed MMIO base
//! address (0xFED40000) and is divided into per-locality 4 KiB pages.

pub mod crb;
pub mod swtpm;

pub use crb::TpmCrb;
pub use swtpm::SwtpmBackend;

/// Standard TPM CRB base address (TCG PC Client Platform spec, section 5).
pub const TPM_CRB_BASE_ADDR: usize = 0xFED4_0000;

/// Total MMIO size: 5 localities × 4 KiB each.
pub const TPM_CRB_SIZE: usize = 0x5000;

/// Size of one locality window.
pub const TPM_LOCALITY_SIZE: usize = 0x1000;

/// Offset of the command/response data buffer within each locality page.
pub const TPM_DATA_BUFFER_OFFSET: usize = 0x80;

/// Size of the command/response data buffer per locality.
pub const TPM_DATA_BUFFER_SIZE: usize = TPM_LOCALITY_SIZE - TPM_DATA_BUFFER_OFFSET;

/// Backend trait implemented by concrete TPM command executors (e.g. swtpm).
pub trait TpmBackend: Send + Sync + 'static {
    /// Execute a marshaled TPM 2.0 command and return the marshaled response.
    ///
    /// The command bytes begin with the standard TPM header (tag, size, code).
    /// On failure the implementation should return a well-formed TPM error
    /// response rather than panicking.
    fn execute_cmd(&self, cmd: &[u8]) -> Vec<u8>;
}

/// Build a minimal ACPI TPM2 table for the CRB interface.
///
/// The returned bytes can be injected via the fw_cfg "etc/acpi/tables" key so
/// that OVMF-based firmware will expose a TPM2 ACPI device to the guest OS.
///
/// The table uses StartMethod = 7 (CRB) and points the control area at
/// offset 0x40 within locality 0 (0xFED40040).
pub fn build_acpi_tpm2_table() -> Vec<u8> {
    // ACPI table layout (TCG ACPI spec for TPM2, revision 4):
    //   [0..4]   Signature  "TPM2"
    //   [4..8]   Length     76
    //   [8]      Revision   4
    //   [9]      Checksum   (computed below)
    //   [10..16] OEMID
    //   [16..24] OEM Table ID
    //   [24..28] OEM Revision
    //   [28..32] Creator ID
    //   [32..36] Creator Revision
    //   [36..38] PlatformClass   0 (client)
    //   [38..40] Reserved        0
    //   [40..48] ControlAddress  0xFED40040
    //   [48..52] StartMethod     7 (CRB)
    //   [52..64] StartMethodSpecificParameters (12 bytes, zeros for CRB)
    //   [64..68] MinimumLogAreaLength
    //   [68..76] LogAreaStartAddress
    const TABLE_LEN: usize = 76;
    let mut t = vec![0u8; TABLE_LEN];

    t[0..4].copy_from_slice(b"TPM2");
    t[4..8].copy_from_slice(&(TABLE_LEN as u32).to_le_bytes());
    t[8] = 4; // Revision
    // t[9] = checksum — filled in at the end
    t[10..16].copy_from_slice(b"OXIDE ");
    t[16..24].copy_from_slice(b"PROPIVM ");
    t[24..28].copy_from_slice(&1u32.to_le_bytes()); // OEM revision
    t[28..32].copy_from_slice(b"PRPL");             // Creator ID
    t[32..36].copy_from_slice(&1u32.to_le_bytes()); // Creator revision

    // PlatformClass = 0 (client platform)
    t[36..38].copy_from_slice(&0u16.to_le_bytes());
    // Reserved
    t[38..40].copy_from_slice(&0u16.to_le_bytes());
    // ControlAddress: base of locality 0 + CRB control area offset (0x40)
    let ctrl_addr = (TPM_CRB_BASE_ADDR + 0x40) as u64;
    t[40..48].copy_from_slice(&ctrl_addr.to_le_bytes());
    // StartMethod = 7 = TPM2_START_METHOD_CRB
    t[48..52].copy_from_slice(&7u32.to_le_bytes());
    // StartMethodSpecificParameters[12] — all zeros for CRB (offset 52..64)
    // MinimumLogAreaLength = 64 KiB (conventional minimum)
    t[64..68].copy_from_slice(&0x1_0000u32.to_le_bytes());
    // LogAreaStartAddress = 0 (no event log provisioned)
    t[68..76].copy_from_slice(&0u64.to_le_bytes());

    // Compute ACPI checksum: byte sum of entire table must equal 0 mod 256.
    let sum: u8 = t.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);

    t
}
