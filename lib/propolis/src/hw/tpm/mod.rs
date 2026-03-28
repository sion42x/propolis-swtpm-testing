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
pub fn build_acpi_tpm2_ssdt() -> Vec<u8> {
    // Compiled AML for:
    //
    //   DefinitionBlock("", "SSDT", 2, "OXIDE ", "TPMDEV  ", 1) {
    //     Scope(\_SB) {
    //       Device(TPM) {
    //         Name(_HID, "MSFT0101")
    //         Name(_CRS, ResourceTemplate() {
    //           Memory32Fixed(ReadWrite, 0xFED40000, 0x5000)
    //         })
    //         Method(_STA, 0, NotSerialized) { Return(0x0F) }
    //       }
    //     }
    //   }

    // Memory32Fixed(ReadWrite, 0xFED40000, 0x5000) resource descriptor + End Tag
    let resource: &[u8] = &[
        0x86, 0x09, 0x00,             // Large Item type 6 (Memory32Fixed), length=9
        0x01,                         // read-write
        0x00, 0x00, 0xD4, 0xFE,       // base  = 0xFED4_0000 (LE)
        0x00, 0x50, 0x00, 0x00,       // length= 0x0000_5000 (LE)
        0x79, 0x00,                   // End Tag
    ]; // 14 bytes

    // Buffer { BytePrefix 14; resource[14] }
    // PkgLength = 1(self) + 2(size expr) + 14(data) = 17 = 0x11
    let mut crs_buf: Vec<u8> =
        vec![0x11, 0x11, 0x0A, 0x0E]; // BufferOp, PkgLen=17, BytePfx, 14
    crs_buf.extend_from_slice(resource); // 4 + 14 = 18 bytes

    // Name(_HID, "MSFT0101")  — 1+4+1+9 = 15 bytes
    let mut name_hid: Vec<u8> =
        vec![0x08, 0x5F, 0x48, 0x49, 0x44, 0x0D]; // NameOp "_HID" StringOp
    name_hid.extend_from_slice(b"MSFT0101\0");

    // Name(_CRS, <buffer>)  — 1+4+18 = 23 bytes
    let mut name_crs: Vec<u8> =
        vec![0x08, 0x5F, 0x43, 0x52, 0x53]; // NameOp "_CRS"
    name_crs.extend_from_slice(&crs_buf);

    // Method(_STA, 0, NotSerialized) { Return(0x0F) }
    // PkgLength = 1(self)+4("_STA")+1(flags)+1(RetOp)+1(BytePfx)+1(0x0F) = 9
    let method_sta: Vec<u8> = vec![
        0x14, 0x09,                   // MethodOp PkgLen=9
        0x5F, 0x53, 0x54, 0x41,       // "_STA"
        0x00,                         // flags: 0 args, not serialized
        0xA4, 0x0A, 0x0F,             // ReturnOp BytePrefix 0x0F
    ]; // 10 bytes

    // Device(TPM) { _HID _CRS _STA }
    // content = "TPM_"(4) + name_hid(15) + name_crs(23) + method_sta(10) = 52
    // PkgLength = 1 + 52 = 53 = 0x35
    let mut device_tpm: Vec<u8> =
        vec![0x5B, 0x82, 0x35,        // DeviceOp PkgLen=53
             0x54, 0x50, 0x4D, 0x5F]; // "TPM_"
    device_tpm.extend_from_slice(&name_hid);
    device_tpm.extend_from_slice(&name_crs);
    device_tpm.extend_from_slice(&method_sta);
    // 3 + 4 + 15 + 23 + 10 = 55 bytes

    // Scope(\_SB) { Device(TPM) }
    // content = "\\_SB_"(5) + device_tpm(55) = 60
    // PkgLength = 1 + 60 = 61 = 0x3D
    let mut scope_sb: Vec<u8> =
        vec![0x10, 0x3D,              // ScopeOp PkgLen=61
             0x5C, 0x5F, 0x53, 0x42, 0x5F]; // "\\_SB_"
    scope_sb.extend_from_slice(&device_tpm);
    // 2 + 5 + 55 = 62 bytes

    const TABLE_LEN: usize = 36 + 62; // header + AML = 98
    let mut t = vec![0u8; TABLE_LEN];

    t[0..4].copy_from_slice(b"SSDT");
    t[4..8].copy_from_slice(&(TABLE_LEN as u32).to_le_bytes());
    t[8] = 2; // Revision
    // t[9] = checksum — filled in at the end
    t[10..16].copy_from_slice(b"OXIDE ");
    t[16..24].copy_from_slice(b"TPMDEV  ");
    t[24..28].copy_from_slice(&1u32.to_le_bytes());
    t[28..32].copy_from_slice(b"PRPL");
    t[32..36].copy_from_slice(&1u32.to_le_bytes());
    t[36..].copy_from_slice(&scope_sb);

    let sum: u8 = t.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);

    t
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
