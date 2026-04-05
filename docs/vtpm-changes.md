# vTPM Implementation: Summary of Changes

## propolis (this repo)

### `lib/propolis/src/hw/tpm/mod.rs`
New module. Defines:
- `TpmBackend` trait — interface for TPM command executors
- `TpmCrb` re-export and constants (`TPM_CRB_BASE_ADDR`, `TPM_CRB_SIZE`, etc.)
- `build_acpi_tpm2_table()` — builds a minimal ACPI TPM2 table (unused in current
  demo; OVMF provides the table statically)
- `build_fw_cfg_table_loader()` — builds the `etc/table-loader` fw_cfg blob
  (unused in current demo)

### `lib/propolis/src/hw/tpm/crb.rs`
New file. Implements the TCG PC Client Platform CRB register set at
`0xFED40000`. Key details:

- Only locality 0 is implemented; localities 1–4 return zeros.
- `INTF_ID_LO` reports `0x00024011`: InterfaceType=CRB, CapCRB (bit 14) set,
  InterfaceSelector=CRB (bit 17). These bit positions match the TCG PTP spec
  and are what edk2's `Tcg2ConfigPeim` checks to select the CRB driver path.
- `CTRL_STS` initializes with `tpmIdle` set and the fatal-error bit clear.
- After a `cmdReady` or `goIdle` write to `CTRL_REQ`, the bit is cleared
  immediately (hardware signals transition complete by clearing it).
- `CTRL_START` dispatch: reads the command size from the TPM header, calls the
  backend synchronously, writes the response back to the data buffer, then
  clears `CTRL_START` to signal completion.

### `lib/propolis/src/hw/tpm/swtpm.rs`
New file. `SwtpmBackend` connects to a running swtpm process over a Unix domain
socket and forwards raw TPM 2.0 command/response traffic.

- Reconnects automatically if the socket drops.
- Sends `TPM2_Startup(Clear)` on the first connection of each propolis session
  so the TPM is in Ready state before the guest issues any command. Subsequent
  connections within the same session skip this (tracked by `startup_done`).

### `bin/propolis-standalone/`
Wired `tpm-crb` as a recognized device driver in the standalone config parser,
accepting a `socket_path` field pointing at the swtpm data socket.

### `bin/propolis-server/` — TPM wired into the server API path

**`crates/propolis-api-types-versions/src/add_vsock/components/devices.rs`**
Added `TpmCrb { socket_path: String }` struct.

**`crates/propolis-api-types-versions/src/add_vsock/instance_spec.rs`**
Added `Component::TpmCrb(TpmCrb)` variant. Like `VirtioSocket`, it is
extracted before the v3→v2→v1 conversion chain and filtered out of v1
specs (which predate TPM support).

**`crates/propolis-api-types-versions/src/latest.rs`**
Re-exports `TpmCrb` from v3, making it available as
`propolis_client::instance_spec::TpmCrb`.

**`crates/propolis-config-toml/src/spec.rs`**
Added `"tpm-crb"` match arm so `propolis-cli --config-toml` can parse the
same TOML format used by propolis-standalone. Reads `socket_path` from the
device options.

**`bin/propolis-server/src/lib/spec/mod.rs`**
Added internal `TpmCrb { id, spec }` struct and `tpm_crb: Option<TpmCrb>`
field to `Spec`. Updated `From<Spec> for InstanceSpec` and
`TryFrom<InstanceSpec> for Spec` to extract/insert the TPM component around
the versioned conversion chain.

**`bin/propolis-server/src/lib/spec/builder.rs`**
Added `TpmCrbInUse` error variant and `add_tpm_crb_device()` method.

**`bin/propolis-server/src/lib/spec/api_spec_v0.rs`**
Added `tpm_crb: _` to the exhaustive `Spec` destructure (TPM has no v1
representation).

**`bin/propolis-server/src/lib/initializer.rs`**
Added `initialize_tpm_crb()`: creates `SwtpmBackend` from the socket path,
wraps it in `TpmCrb`, and attaches it to the MMIO bus.

**`bin/propolis-server/src/lib/vm/ensure.rs`**
Calls `initialize_tpm_crb()` during instance initialization, after vsock.

---

## oxide-edk2 (OvmfPkg)

### `OvmfPkg/AcpiTables/Tpm2.aslc` (new file)
Static ACPI TPM2 table:
- Signature `TPM2`, Revision 4
- `StartMethod = 7` (CRB)
- `AddressOfControlArea = 0xFED40040` (locality 0 base + CRB control area offset)
- `Laml = 0` (no event log)

### `OvmfPkg/AcpiTables/TpmSsdt.asl` (new file)
ACPI SSDT declaring `\_SB.TPM` with `_HID = "MSFT0101"`. Required for Windows
to load its CRB TPM driver (`tpmci.sys`); the TPM2 table alone is not
sufficient. Includes:
- `_CRS`: Memory32Fixed covering the full 5-locality window (`0xFED40000`, `0x5000`)
- `_STA`: returns `0x0F` (present, enabled, shown in UI, functioning)
- `_DSM`: stub for the TCG Physical Presence Interface
  (UUID `3DDDFAA6-361B-4EB4-A424-8D10089D1653`) satisfying Windows's requirement
  to evaluate PPI methods without a firmware-level PPI handler

### `OvmfPkg/AcpiTables/AcpiTables.inf`
Added `Tpm2.aslc` and `TpmSsdt.asl` to `[Sources]`.

### `OvmfPkg/AcpiPlatformDxe/Qemu.c`
Capped `Mmio32MaxExclTop` at the PCI MMIO ceiling (`PcdPciMmio32Base +
PcdPciMmio32Size`, i.e. `0xFC000000`) when computing the 32-bit PCI window.
Without this, the TPM's MMIO region at `0xFED40000` inflated the PCI window to
include it, causing a resource conflict (Device Manager Code 12) with the TPM
device.

### `OvmfPkg/AcpiPlatformDxe/AcpiPlatformDxe.inf`
Added `PcdPciMmio32Base` and `PcdPciMmio32Size` to `[Pcd]` to support the
ceiling fix above.
