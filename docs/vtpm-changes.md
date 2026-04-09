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

## Control plane integration (Oxide rack, r19)

The changes above were sufficient to prove the concept on `propolis-standalone`.
The following additional work was done to make vTPM operational on a real Oxide
racklet running rack software r19, using `propolis-server` and Nexus-provisioned
instances.

### TPM state persistence via Crucible

**`lib/propolis/src/block/crucible.rs`**
Added three new methods to `CrucibleBackend` for direct host-side I/O, bypassing
the guest block stack:
- `open_for_raw_io()` — activates a Crucible volume and returns a backend
  suitable for direct reads/writes (not attached to any guest device)
- `read_raw(len)` — reads block-aligned bytes from offset 0
- `write_raw(data)` — writes block-aligned bytes to offset 0, then flushes

**`bin/propolis-server/src/lib/initializer.rs`**
Added `TpmStatePersister` struct, held alive in `VmObjects` for the duration of
the VM's life. On `save()`:
1. Kills the swtpm child process (swtpm flushes `tpm2-00.permall` on every
   command completion, so the file is current at kill time)
2. Reads `tpm2-00.permall` from the state directory
3. Writes it to the Crucible volume with the wire format:
   `TPMST\x00\x01\x00` (8-byte magic) + u64 LE length + raw permall bytes

Added `initialize_tpm_with_state_disk()`: activates the Crucible volume,
restores prior state if the magic header is present (fresh disk is handled
gracefully — starts swtpm with an empty state dir), creates the state directory,
removes stale sockets, spawns swtpm as a managed child process (no `--daemon`),
waits up to 5 seconds for the Unix socket to appear, then attaches the CRB device.

Added `restore_tpm_state_from_crucible()`: reads the blob, validates the magic
header, extracts the permall bytes, and writes them to the state directory so
swtpm can restore from them on startup.

**`bin/propolis-server/src/lib/vm/objects.rs`**
Added `tpm_state: Option<TpmStatePersister>` to `InputVmObjects` and
`VmObjectsLocked`. `halt_devices()` calls `tpm_state.save().await` before
tearing down other devices.

### TpmStateDisk spec type

**`crates/propolis-api-types-versions/src/add_vsock/components/devices.rs`**
Added `TpmStateDisk { request_json: String }` — carries the Crucible VCR for
the TPM state volume.

**`crates/propolis-api-types-versions/src/add_vsock/instance_spec.rs`**
Added `Component::TpmStateDisk(TpmStateDisk)` variant.

**`bin/propolis-server/src/lib/spec/mod.rs`**
Added internal `TpmStateDisk { id, spec }` struct and `tpm_state_disk:
Option<TpmStateDisk>` field to `Spec`.

**`bin/propolis-server/src/lib/spec/builder.rs`**
Added `TpmStateDiskInUse` error variant and `add_tpm_state_disk()` method.

**`bin/propolis-server/src/lib/spec/api_spec_v0.rs`**
Added `tpm_state_disk: _` to the exhaustive `Spec` destructure (no v0
representation; the disk is intercepted before the guest sees it).

### Disk interception by NVMe serial number

**`bin/propolis-server/src/lib/vm/ensure.rs`**
At the top of `initialize_vm_objects()`, if `--swtpm-binary` is set and no
`tpm_state_disk` is already in the spec, the code searches `spec.disks` for an
NVMe disk whose serial number starts with `b"tpm-"`. If found:
- The disk is removed from `spec.disks` (Windows never enumerates it)
- Its Crucible VCR is moved into `spec.tpm_state_disk`

This relies on the convention that a disk named `tpm-*` in Nexus gets an NVMe
serial equal to its name (padded to 20 bytes by sled-agent). No sled-agent
changes are required.

TPM initialization is non-fatal: if anything fails (Crucible activation error,
swtpm timeout, etc.), a warning is logged and the VM continues without a TPM
rather than failing the entire ensure.

### `--swtpm-binary` CLI argument

**`bin/propolis-server/src/main.rs`**
Added `--swtpm-binary <PATH>` to the `run` subcommand.

**`bin/propolis-server/src/lib/server.rs`**
Added `swtpm_binary: Option<PathBuf>` to `StaticConfig` and threaded it through
`DropshotEndpointContext` and `EnsureOptions`.

**`bin/propolis-server/src/lib/vm/mod.rs`**
Added `swtpm_binary: Option<PathBuf>` to `EnsureOptions`.

### `packaging/smf/method_script.sh`
If `/opt/oxide/propolis-server/bin/swtpm` exists and is executable, passes
`--swtpm-binary` to propolis-server and sets `LD_LIBRARY_PATH` for swtpm's
shared libraries. No changes to sled-agent or Nexus required.

### Deployment

The swtpm binary and its libraries are bundled into the propolis-server tarball
at repack time alongside the propolis-server binary and OVMF firmware. The
customer creates a 1 GiB disk in Nexus named `tpm-<anything>`, attaches it to
the instance at creation time, and propolis-server handles everything else.

### Verified on

- Oxide racklet running rack software **r19**
- Windows Server 2022 guest (16 vCPU, 64 GiB)
- `Get-Tpm`: TpmPresent/Ready/Enabled/Activated all True (ManufacturerIdTxt: IBM / swtpm)
- BitLocker full-volume encryption + automatic unseal across Nexus stop/start

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
