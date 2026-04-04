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

/// Build an ACPI SSDT that declares an MSFT0101 TPM 2.0 device in the \_SB
/// scope.  Windows requires an ACPI device node with HID "MSFT0101" to load
/// its CRB TPM driver; the TPM2 table alone is not sufficient.
///
/// The device's _CRS exposes the full CRB MMIO window (0xFED40000, 0x5000).
/// _STA returns 0x0F (present, enabled, shown in UI, functioning).
///
/// A `_DSM` stub for the TCG Physical Presence Interface
/// (UUID {3DDDFAA6-361B-4EB4-A424-8D10089D1653}, Revision 0) is included.
/// Without it, Win32_Tpm.GetPhysicalPresenceRequest() returns
/// TBS_E_INTERNAL_ERROR (0x80284001) and Get-Tpm throws TpmWmiException.
///
/// The AML bytes were produced by:
///
/// ```text
/// DefinitionBlock("TpmSsdt.aml", "SSDT", 2, "OXIDE ", "TPMDEV  ", 1) {
///   Scope(\_SB) {
///     Device(TPM) {
///       Name(_HID, "MSFT0101")
///       Name(_CRS, ResourceTemplate() {
///         Memory32Fixed(ReadWrite, 0xFED40000, 0x5000)
///       })
///       Method(_STA, 0, NotSerialized) { Return(0x0F) }
///       Method(_DSM, 4, Serialized) {  // TCG PPI stub
///         If(LEqual(Arg0, ToUUID("3DDDFAA6-361B-4EB4-A424-8D10089D1653"))) {
///           Switch(ToInteger(Arg2)) {
///             Case(0) { Return(Buffer(2){0xFF,0x01}) }  // functions 0-8
///             Case(1) { Return(Zero) }                  // submit v1 -> ok
///             Case(2) { Return(Package(2){0,0}) }       // pending op -> none
///             Case(3) { Return(2) }                     // platform action
///             Case(4) { Return(Package(3){0,0,0}) }     // last response
///             Case(5) { Return(3) }                     // language -> n/a
///             Case(6) { Return(Package(2){0,0}) }       // submit v2 -> ok
///             Case(7) { Return(4) }                     // confirm -> allowed
///             Case(8) { Return(Zero) }                  // action v2 -> none
///           }
///         }
///         Return(Buffer(1){0})
///       }
///     }
///   }
/// }
/// ```
///
/// Compiled with `iasl -tc TpmSsdt.asl` (Intel ACPI CA 20251212).
pub fn build_acpi_tpm2_ssdt() -> Vec<u8> {
    #[rustfmt::skip]
    const AML: &[u8] = &[
        0x53, 0x53, 0x44, 0x54, 0x27, 0x01, 0x00, 0x00, 0x02, 0xcb, 0x4f, 0x58,
        0x49, 0x44, 0x45, 0x20, 0x54, 0x50, 0x4d, 0x44, 0x45, 0x56, 0x20, 0x20,
        0x01, 0x00, 0x00, 0x00, 0x49, 0x4e, 0x54, 0x4c, 0x12, 0x12, 0x25, 0x20,
        0x10, 0x42, 0x10, 0x5c, 0x5f, 0x53, 0x42, 0x5f, 0x5b, 0x82, 0x49, 0x0f,
        0x54, 0x50, 0x4d, 0x5f, 0x08, 0x5f, 0x48, 0x49, 0x44, 0x0d, 0x4d, 0x53,
        0x46, 0x54, 0x30, 0x31, 0x30, 0x31, 0x00, 0x08, 0x5f, 0x43, 0x52, 0x53,
        0x11, 0x11, 0x0a, 0x0e, 0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0xd4, 0xfe,
        0x00, 0x50, 0x00, 0x00, 0x79, 0x00, 0x14, 0x09, 0x5f, 0x53, 0x54, 0x41,
        0x00, 0xa4, 0x0a, 0x0f, 0x14, 0x42, 0x0c, 0x5f, 0x44, 0x53, 0x4d, 0x0c,
        0x08, 0x5f, 0x54, 0x5f, 0x30, 0x00, 0xa0, 0x4f, 0x0a, 0x93, 0x68, 0x11,
        0x13, 0x0a, 0x10, 0xa6, 0xfa, 0xdd, 0x3d, 0x1b, 0x36, 0xb4, 0x4e, 0xa4,
        0x24, 0x8d, 0x10, 0x08, 0x9d, 0x16, 0x53, 0xa2, 0x46, 0x09, 0x01, 0x70,
        0x99, 0x6a, 0x00, 0x5f, 0x54, 0x5f, 0x30, 0xa0, 0x0e, 0x93, 0x5f, 0x54,
        0x5f, 0x30, 0x00, 0xa4, 0x11, 0x05, 0x0a, 0x02, 0xff, 0x01, 0xa1, 0x4a,
        0x07, 0xa0, 0x09, 0x93, 0x5f, 0x54, 0x5f, 0x30, 0x01, 0xa4, 0x00, 0xa1,
        0x4d, 0x06, 0xa0, 0x0e, 0x93, 0x5f, 0x54, 0x5f, 0x30, 0x0a, 0x02, 0xa4,
        0x12, 0x04, 0x02, 0x00, 0x00, 0xa1, 0x4b, 0x05, 0xa0, 0x0b, 0x93, 0x5f,
        0x54, 0x5f, 0x30, 0x0a, 0x03, 0xa4, 0x0a, 0x02, 0xa1, 0x4c, 0x04, 0xa0,
        0x0f, 0x93, 0x5f, 0x54, 0x5f, 0x30, 0x0a, 0x04, 0xa4, 0x12, 0x05, 0x03,
        0x00, 0x00, 0x00, 0xa1, 0x39, 0xa0, 0x0b, 0x93, 0x5f, 0x54, 0x5f, 0x30,
        0x0a, 0x05, 0xa4, 0x0a, 0x03, 0xa1, 0x2b, 0xa0, 0x0e, 0x93, 0x5f, 0x54,
        0x5f, 0x30, 0x0a, 0x06, 0xa4, 0x12, 0x04, 0x02, 0x00, 0x00, 0xa1, 0x1a,
        0xa0, 0x0b, 0x93, 0x5f, 0x54, 0x5f, 0x30, 0x0a, 0x07, 0xa4, 0x0a, 0x04,
        0xa1, 0x0c, 0xa0, 0x0a, 0x93, 0x5f, 0x54, 0x5f, 0x30, 0x0a, 0x08, 0xa4,
        0x00, 0xa5, 0xa4, 0x11, 0x03, 0x01, 0x00,
    ];
    AML.to_vec()
}

/// Build the `etc/table-loader` fw_cfg blob for a set of ACPI tables.
///
/// OVMF requires this blob alongside `etc/acpi/tables`; without it OVMF
/// ignores the tables blob entirely.  The format is a sequence of 128-byte
/// QEMU BIOS linker/loader commands:
///
///   1. One `ALLOCATE` entry — tells OVMF to load `etc/acpi/tables` into
///      high memory at 64-byte alignment.
///   2. One `ADD_CHECKSUM` entry per table — tells OVMF to (re)compute the
///      ACPI header checksum byte (offset 9) over each table's byte range.
///
/// `table_ranges` is a slice of `(start_offset, length)` pairs that describe
/// each ACPI table within the concatenated `etc/acpi/tables` blob.
pub fn build_fw_cfg_table_loader(table_ranges: &[(usize, usize)]) -> Vec<u8> {
    const ENTRY_SIZE: usize = 128;
    const FILESZ: usize = 56; // max fw_cfg filename length including NUL
    const CMD_ALLOCATE: u32 = 1;
    const CMD_ADD_CHECKSUM: u32 = 3;
    const ZONE_HIGH: u8 = 1; // allocate above 4 GiB boundary

    let name = b"etc/acpi/tables";
    assert!(name.len() < FILESZ, "fw_cfg name too long");

    let mut out = Vec::new();

    // ALLOCATE: load the blob into high memory, 64-byte aligned.
    let mut e = [0u8; ENTRY_SIZE];
    e[0..4].copy_from_slice(&CMD_ALLOCATE.to_le_bytes());
    e[4..4 + name.len()].copy_from_slice(name);
    e[60..64].copy_from_slice(&64u32.to_le_bytes()); // alignment
    e[64] = ZONE_HIGH;
    out.extend_from_slice(&e);

    // ADD_CHECKSUM: recompute ACPI checksum byte (header offset 9) for each table.
    for &(start, len) in table_ranges {
        let mut e = [0u8; ENTRY_SIZE];
        e[0..4].copy_from_slice(&CMD_ADD_CHECKSUM.to_le_bytes());
        e[4..4 + name.len()].copy_from_slice(name);
        e[60..64].copy_from_slice(&(start as u32 + 9).to_le_bytes()); // offset: checksum byte pos
        e[64..68].copy_from_slice(&(start as u32).to_le_bytes());     // start: region start
        e[68..72].copy_from_slice(&(len as u32).to_le_bytes());       // length: region length
        out.extend_from_slice(&e);
    }

    out
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
    // MinimumLogAreaLength = 0 (no event log; firmware did not populate one)
    t[64..68].copy_from_slice(&0u32.to_le_bytes());
    // LogAreaStartAddress = 0
    t[68..76].copy_from_slice(&0u64.to_le_bytes());

    // Compute ACPI checksum: byte sum of entire table must equal 0 mod 256.
    let sum: u8 = t.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);

    t
}
