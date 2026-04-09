# vTPM Demo: BitLocker-Encrypted Windows VM on illumos

This guide walks through running a Windows Server 2022 VM with a software TPM
(swtpm) and BitLocker encryption. Two deployment paths are covered:

1. **Local helios box** — using `propolis-standalone` or `propolis-server`
   directly; TPM state lives in a local directory.
2. **Oxide rack (r19+)** — using `propolis-server` managed by sled-agent/Nexus;
   TPM state persists across stop/start in a Crucible volume. No recovery key
   prompt, no manual intervention.

The result: a Windows VM whose disk is BitLocker-encrypted and whose TPM key
material survives VM stop/start automatically.

## Architecture

```
Windows guest
  └── tpmci.sys (CRB driver, loaded via MSFT0101 ACPI device node)
        └── CRB MMIO at 0xFED40000
              └── propolis TpmCrb device model
                    └── SwtpmBackend (Unix socket)
                          └── swtpm process (/tmp/swtpm.sock)
                                └── swtpm state dir (/tmp/swtpm-state/)  ← the "key"
                                      └── [on Oxide rack] Crucible volume (tpm-* disk)
```

OVMF firmware provides two ACPI tables that Windows requires:
- `TPM2` table (StartMethod=7 CRB, ControlArea=0xFED40040) — tells tpmci.sys
  where and how to talk to the TPM
- `SSDT` with an `MSFT0101` device node — triggers Windows to load tpmci.sys

On a local helios box, the swtpm state directory is a plain filesystem path that
must be preserved manually between restarts. On an Oxide rack, propolis-server
manages swtpm as a child process and automatically saves/restores the state to a
dedicated Crucible volume on every VM stop/start.

## Prerequisites

### swtpm on illumos

swtpm must be built from source on helios/illumos (the OmniOS IPS package does
not work on helios). Build version 0.10.1 from
[stefanberger/swtpm](https://github.com/stefanberger/swtpm):

```bash
git clone https://github.com/stefanberger/swtpm /root/swtpm
cd /root/swtpm
git checkout v0.10.1   # or the 0.10.1 release commit
./autogen.sh
./configure --prefix=/opt/swtpm --with-tss-user=root --with-tss-group=root MAKE=gmake
gmake -j$(nproc)
gmake install
```

The binary ends up at `/opt/swtpm/bin/swtpm`.

### OVMF firmware

You need an OVMF build from the Oxide edk2 fork
(`oxide-computer/edk2`, branch `oxide`) that includes the TPM ACPI tables.
A prebuilt `OVMF_CODE.fd` should be provided alongside this demo; if you need
to rebuild it:

```bash
cd ~/oxide-edk2
./illumos/build.sh DEBUG
cp Build/OvmfX64/DEBUG_ILLGCC/FV/OVMF_CODE.fd /root/OVMF_CODE.fd
```

### propolis binaries

Build propolis on the illumos host:

```bash
cargo build --release -p propolis-server -p propolis-standalone
```

`propolis-server` is the recommended path — it exposes the full HTTP API and
accepts VM configuration via `propolis-cli`. `propolis-standalone` is simpler
for quick iteration but doesn't go through the API layer.

### Windows Server 2022 disk image

A raw disk image with Windows Server 2022 installed. The image used here is at
`/root/images/SERVER_EVAL_x64FRE_en-us.raw`.

## VM configuration

`/root/win2022.toml`:

```toml
[main]
name = "win2022"
cpus = 4
memory = 32768
bootrom = "/root/OVMF_CODE.fd"

[block_dev.win]
type = "file"
path = "/root/images/SERVER_EVAL_x64FRE_en-us.raw"

[dev.win-disk]
driver = "pci-nvme"
block_dev = "win"
pci-path = "0.4.0"

[dev.tpm0]
driver = "tpm-crb"
socket_path = "/tmp/mytpm.sock"

[dev.net0]
driver = "pci-virtio-viona"
vnic = "vnic0"
pci-path = "0.5.0"
```

## First-time setup

### 1. Initialize the swtpm state directory

```bash
mkdir -p /tmp/mytpm
```

### 2. Start swtpm

```bash
/opt/swtpm/bin/swtpm socket --tpm2 --tpmstate dir=/tmp/mytpm \
  --ctrl type=unixio,path=/tmp/mytpm.ctrl \
  --server type=unixio,path=/tmp/mytpm.sock \
  --flags not-need-init --daemon
```

### 3. Start propolis and boot the VM

**Using propolis-server (recommended):**

```bash
# Terminal 1: start the server
pfexec /root/propolis/target/release/propolis-server run \
  /root/OVMF_CODE.fd 0.0.0.0:12400

# Terminal 2: create and run the instance
./target/release/propolis-cli -s 127.0.0.1 -p 12400 new \
  --config-toml /root/win2022.toml -c 4 -m 32768 win2022
./target/release/propolis-cli -s 127.0.0.1 -p 12400 state run

# Connect to serial console
./target/release/propolis-cli -s 127.0.0.1 -p 12400 serial
```

**Using propolis-standalone:**

```bash
/root/propolis/target/release/propolis-standalone /root/win2022.toml \
  > /tmp/propolis.log 2>&1 &
socat STDIO,raw,echo=0 UNIX-CONNECT:/root/ttya
```

Windows will boot. You should see "Trusted Platform Module 2.0" in Device
Manager under Security Devices.

### 5. Install BitLocker and provision the TPM

BitLocker is not installed by default on Windows Server 2022. From an elevated
PowerShell prompt:

```powershell
Install-WindowsFeature BitLocker -IncludeAllSubFeature -IncludeManagementTools -Restart
```

The VM will reboot. After it comes back up, verify the TPM is visible:

```powershell
Get-Tpm
# TpmPresent, TpmReady, TpmEnabled, TpmActivated should all be True
```

Then provision the TPM. This creates the Storage Root Key (SRK) that BitLocker
will use to seal the volume key:

```powershell
Initialize-Tpm
```

### 6. Enable BitLocker

The standard `Enable-BitLocker` PowerShell cmdlet requires the TBS service,
which is absent on Windows Server 2022. Use the WMI path instead:

```powershell
# 1. Create the TPM protector (empty PCR selection — binds VMK to SRK only,
#    not PCR values, so any propolis restart with the same swtpm state unseals)
$vol = Get-WmiObject -Namespace root\cimv2\Security\MicrosoftVolumeEncryption `
  -Class Win32_EncryptableVolume -Filter "DriveLetter = 'C:'"
$r = $vol.ProtectKeyWithTPM("TPM", [byte[]]@())
"Protector: $($r.ReturnValue) - ID: $($r.VolumeKeyProtectorID)"  # ReturnValue 0 = success

# 2. Start encryption (XTS-AES-256, encrypt used space only)
$r2 = $vol.Encrypt(6, 0)
"Encrypt: 0x$($r2.ReturnValue.ToString('X8'))"  # 0x00000000 = success

# 3. Wait for completion
while ($true) {
    $v = Get-BitLockerVolume C:
    Write-Host "$($v.VolumeStatus) - $($v.EncryptionPercentage)%"
    if ($v.VolumeStatus -eq 'FullyEncrypted') { break }
    Start-Sleep 5
}

# 4. Verify
Get-BitLockerVolume C:
```

### 7. Back up the disk image and swtpm state

Once fully encrypted, shut Windows down cleanly and back everything up:

```powershell
Stop-Computer -Force
```

On helios (after propolis exits):

```bash
cp /root/images/SERVER_EVAL_x64FRE_en-us.raw \
   /root/images/SERVER_EVAL_x64FRE_en-us.raw.bak
cp -r /tmp/mytpm /tmp/mytpm.bak
```

The swtpm state directory is the key material. Keep it safe.

## Normal restart sequence

**The swtpm state directory must not be wiped between restarts.**
`rm -f /tmp/mytpm/*` destroys the SRK and breaks BitLocker.

propolis can reconnect to a running swtpm process — no need to restart swtpm
unless its process died. After a clean Windows shutdown:

**propolis-server:**

```bash
# propolis-server exits when the guest powers off; just restart it and
# re-create the instance
pfexec /root/propolis/target/release/propolis-server run \
  /root/OVMF_CODE.fd 0.0.0.0:12400 &
./target/release/propolis-cli -s 127.0.0.1 -p 12400 new \
  --config-toml /root/win2022.toml -c 4 -m 32768 win2022
./target/release/propolis-cli -s 127.0.0.1 -p 12400 state run
```

**propolis-standalone:**

```bash
/root/propolis/target/release/propolis-standalone /root/win2022.toml \
  > /tmp/propolis.log 2>&1 &
```

If swtpm also needs restarting (process died):

```bash
pkill swtpm
/opt/swtpm/bin/swtpm socket --tpm2 --tpmstate dir=/tmp/mytpm \
  --ctrl type=unixio,path=/tmp/mytpm.ctrl \
  --server type=unixio,path=/tmp/mytpm.sock \
  --flags not-need-init --daemon
```

Windows should boot straight in — no BitLocker recovery key prompt.

## Verifying the security model

To demonstrate that the swtpm state is the actual key material:

```bash
# Break it: replace state with a fresh empty dir
pkill swtpm
mv /tmp/mytpm /tmp/mytpm.good
mkdir /tmp/mytpm
/opt/swtpm/bin/swtpm socket --tpm2 --tpmstate dir=/tmp/mytpm \
  --ctrl type=unixio,path=/tmp/mytpm.ctrl \
  --server type=unixio,path=/tmp/mytpm.sock \
  --flags not-need-init --daemon
# start propolis-server and re-create instance (see Normal restart sequence)
# → BitLocker recovery key screen appears

# Fix it: restore the good state
pkill swtpm
rm -rf /tmp/mytpm
mv /tmp/mytpm.good /tmp/mytpm
/opt/swtpm/bin/swtpm socket --tpm2 --tpmstate dir=/tmp/mytpm \
  --ctrl type=unixio,path=/tmp/mytpm.ctrl \
  --server type=unixio,path=/tmp/mytpm.sock \
  --flags not-need-init --daemon
# start propolis-server and re-create instance (see Normal restart sequence)
# → boots normally, no recovery key prompt
```

## Checking TPM activity

propolis logs TPM CRB traffic to stderr. With propolis-standalone redirected to
a file, or with propolis-server's output captured:

```bash
grep -c '\[CRB\]' /tmp/propolis.log
```

OVMF (Tcg2Pei) accounts for ~11,000 entries on boot; additional entries after
that are from Windows tpmci.sys.

## Troubleshooting

**BitLocker recovery screen on restart**
→ The swtpm state directory was wiped or replaced. Restore from backup.

**`ProtectKeyWithTPM` returns 2150694936 (0x80310018 = FVE_E_TPM_NOT_OWNED)**
→ TPM not provisioned yet. Run `Initialize-Tpm` first.

**`Enable-BitLocker -TpmProtector` fails with "external key or password protector required"**
→ Use the `Win32_EncryptableVolume` WMI path shown above instead.
   The PowerShell cmdlet requires the TBS service, absent on Server 2022.

**Device Manager shows TPM with error (Code 12, resource conflict)**
→ PCI MMIO window is overlapping the TPM MMIO range. Ensure you are using the
   Oxide OVMF build with the PCI ceiling fix in AcpiPlatformDxe.

**0 CRB entries in propolis log after boot**
→ OVMF did not detect the CRB interface. Check that INTF_ID_LO reports
   CapCRB (bit 14) set. Value should be 0x00024011.

---

## Oxide rack deployment (r19+)

This path requires a propolis-server build that includes the Crucible-backed TPM
state persistence changes (see `vtpm-changes.md`) and an swtpm binary bundled
into the propolis-server tarball.

### Prerequisites

- propolis-server with TPM changes built and repacked to the sleds
- `swtpm` binary at `/opt/oxide/propolis-server/bin/swtpm` inside the zone
- A Nexus project with a Windows Server 2022 instance

### 1. Create a TPM state disk

In Nexus (via the CLI or web console), create a small disk named with a `tpm-`
prefix and attach it to the instance **at creation time**:

```bash
oxide --insecure disk create --project <project> \
  --name tpm-<instance> --size 1GiB --disk-source blank=4096

# Then attach at instance creation time, or include it in the instance spec.
# The disk must be attached before the instance is first started.
```

The name `tpm-<anything>` is the only requirement. propolis-server detects it by
NVMe serial number prefix and intercepts it automatically — Windows never sees
the disk.

### 2. Start the instance

Start the instance normally via Nexus. propolis-server will:
1. Find the `tpm-*` disk in the spec and remove it from the guest device list
2. Activate the Crucible volume for direct I/O
3. Restore prior swtpm state if present (first boot: starts fresh)
4. Spawn swtpm as a managed child process
5. Attach the CRB device to the guest MMIO bus

### 3. Verify TPM presence

From an elevated PowerShell prompt inside the VM:

```powershell
Get-Tpm
# TpmPresent, TpmReady, TpmEnabled, TpmActivated should all be True
# ManufacturerIdTxt: IBM  (swtpm identifies as IBM)
```

### 4. Install BitLocker and encrypt

```powershell
Install-WindowsFeature BitLocker -IncludeAllSubFeature -IncludeManagementTools -Restart
```

After the VM reboots, encrypt using the WMI path (the PowerShell cmdlet requires
the TBS service, which is absent on Windows Server 2022):

```powershell
$vol = Get-WmiObject -Namespace root\cimv2\Security\MicrosoftVolumeEncryption `
  -Class Win32_EncryptableVolume -Filter "DriveLetter = 'C:'"
$r = $vol.ProtectKeyWithTPM("TPM", [byte[]]@())
$r2 = $vol.Encrypt(6, 0)

while ($true) {
    $v = Get-BitLockerVolume C:
    Write-Host "$($v.VolumeStatus) - $($v.EncryptionPercentage)%"
    if ($v.VolumeStatus -eq 'FullyEncrypted') { break }
    Start-Sleep 5
}
```

### 5. Stop and start — automatic unseal

Stop the instance via Nexus. propolis-server will kill swtpm, read
`tpm2-00.permall` from `/tmp/swtpm-state/`, and write it to the Crucible volume
with a magic header before the zone is torn down.

Start the instance again. propolis-server restores the permall file before
spawning swtpm. By the time the guest's CRB driver initializes, the SRK is
already present. Windows boots straight to the desktop — no BitLocker recovery
key prompt.

### Confirming state was saved and restored

From the propolis zone (while the instance is running):

```bash
# Confirm swtpm is managed by propolis-server
pgrep -fl swtpm

# Confirm state dir has the permall file
ls -la /tmp/swtpm-state/

# Check propolis log for TPM lifecycle messages
grep -i "tpm\|swtpm" /var/svc/log/system-illumos-propolis-server:default.log
```

Expected log messages on start:
- `"claiming disk as TPM state disk"` — disk interception fired
- `"No valid TPM state in Crucible, starting fresh"` (first boot only)
- `"swtpm started"` — socket appeared within 5 seconds
- `"TPM CRB device attached (managed swtpm)"`

Expected log message on stop:
- `"saving TPM state to Crucible"`

### Verifying the security model (Oxide rack)

To demonstrate that the Crucible volume is the actual key material, stop the
instance, delete and recreate the `tpm-*` disk (wiping its contents), then start
the instance. BitLocker will show the recovery key screen — the SRK is gone.
Restore from the original volume and it boots normally.

### Notes

- The `tpm-*` disk naming convention is a demo hack. A production implementation
  would have Nexus provision the TPM state volume automatically and pass it to
  sled-agent as a first-class part of the instance spec.
- No sled-agent or Nexus changes are required for this demo path.
- TPM initialization is non-fatal: if the Crucible volume fails to activate or
  swtpm times out, the VM starts without a TPM rather than failing entirely.
