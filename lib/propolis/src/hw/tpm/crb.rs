// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TPM CRB (Command Response Buffer) device model.
//!
//! Implements the guest-facing MMIO register set defined in the TCG PC Client
//! Platform TPM Profile Specification, section 5.  Only locality 0 is fully
//! implemented; localities 1–4 return zeros.

use std::sync::{Arc, Mutex};

use crate::common::{Lifecycle, RWOp};
use crate::mmio::{MmioFn, MmioBus};

use super::{
    TpmBackend, TPM_CRB_BASE_ADDR, TPM_CRB_SIZE, TPM_DATA_BUFFER_OFFSET,
    TPM_DATA_BUFFER_SIZE, TPM_LOCALITY_SIZE,
};

// ── Per-locality register offsets ────────────────────────────────────────────

const REG_LOC_STATE: usize = 0x000; // RO
const REG_LOC_CTRL: usize = 0x008;  // WO
const REG_LOC_STS: usize = 0x00C;   // RO
const REG_INTF_ID_LO: usize = 0x030; // RO (lower 32 bits of 64-bit field)
const REG_INTF_ID_HI: usize = 0x034; // RO (upper 32 bits)
const REG_CTRL_REQ: usize = 0x040;  // RW
const REG_CTRL_STS: usize = 0x044;  // RO
const REG_CTRL_CANCEL: usize = 0x048; // WO
const REG_CTRL_START: usize = 0x04C; // RW
const REG_INT_ENABLE: usize = 0x050; // RW
const REG_INT_STS: usize = 0x054;   // RW1C
const REG_CTRL_CMD_SIZE: usize = 0x058; // RO
const REG_CTRL_CMD_LADDR: usize = 0x05C; // RO
const REG_CTRL_CMD_HADDR: usize = 0x060; // RO
const REG_CTRL_RSP_SIZE: usize = 0x064;  // RO
const REG_CTRL_RSP_ADDR_LO: usize = 0x068; // RO
const REG_CTRL_RSP_ADDR_HI: usize = 0x06C; // RO

// ── LOC_STATE bits ────────────────────────────────────────────────────────────

const LOC_STATE_REG_VALID: u32 = 1 << 7;
const LOC_STATE_LOC_ASSIGNED: u32 = 1 << 1;

// ── LOC_CTRL bits ─────────────────────────────────────────────────────────────

const LOC_CTRL_REQUEST_ACCESS: u32 = 1 << 0;

// ── LOC_STS bits ──────────────────────────────────────────────────────────────

const LOC_STS_GRANTED: u32 = 1 << 0;

// ── INTF_ID value ─────────────────────────────────────────────────────────────
//
// bits[3:0]   InterfaceType     = 1 (CRB)
// bits[7:4]   InterfaceVersion  = 1
// bits[19:18] CapCRB            = 1 (CRB supported)
// bits[23:22] InterfaceSelector = 1 (CRB active)

const INTF_ID_LO: u32 =
    (1 << 0)  // InterfaceType = CRB
    | (1 << 4)  // InterfaceVersion = 1
    | (1 << 18) // CapCRB
    | (1 << 22); // InterfaceSelector = CRB
const INTF_ID_HI: u32 = 0; // RID = 0

// ── CTRL_STS bits ─────────────────────────────────────────────────────────────
//
// tpmSts  (bit 0): 1 = idle/not-ready, 0 = ready
// tpmIdle (bit 1): 1 = idle state

const CTRL_STS_NOT_READY: u32 = 1 << 0;
const CTRL_STS_IDLE: u32 = 1 << 1;

// ── CTRL_REQ bits ─────────────────────────────────────────────────────────────

const CTRL_REQ_CMD_READY: u32 = 1 << 0;
const CTRL_REQ_GO_IDLE: u32 = 1 << 1;

// ── CTRL_START bit ────────────────────────────────────────────────────────────

const CTRL_START_START: u32 = 1 << 0;

// ── Data buffer base physical address ─────────────────────────────────────────

const DATA_BUF_ADDR: usize = TPM_CRB_BASE_ADDR + TPM_DATA_BUFFER_OFFSET;

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum TpmState {
    Idle,
    Ready,
}

// ── Device state ─────────────────────────────────────────────────────────────

struct CrbRegs {
    state: TpmState,
    loc_state: u32,
    loc_sts: u32,
    ctrl_req: u32,
    ctrl_sts: u32,
    ctrl_start: u32,
    int_enable: u32,
    int_sts: u32,
    data_buf: Vec<u8>,
}

impl CrbRegs {
    fn new() -> Self {
        Self {
            state: TpmState::Idle,
            // Locality 0: register-valid, assigned, not established
            loc_state: LOC_STATE_REG_VALID | LOC_STATE_LOC_ASSIGNED,
            loc_sts: LOC_STS_GRANTED,
            ctrl_req: 0,
            // Idle state: both tpmIdle and tpmSts set
            ctrl_sts: CTRL_STS_IDLE | CTRL_STS_NOT_READY,
            ctrl_start: 0,
            int_enable: 0,
            int_sts: 0,
            data_buf: vec![0u8; TPM_DATA_BUFFER_SIZE],
        }
    }
}

// ── TpmCrb ───────────────────────────────────────────────────────────────────

pub const DEVICE_NAME: &str = "tpm-crb";

pub struct TpmCrb {
    regs: Mutex<CrbRegs>,
    backend: Arc<dyn TpmBackend>,
}

impl TpmCrb {
    pub fn create(backend: Arc<dyn TpmBackend>) -> Arc<Self> {
        Arc::new(Self { regs: Mutex::new(CrbRegs::new()), backend })
    }

    /// Register the CRB MMIO region with the provided [`MmioBus`].
    pub fn attach(self: &Arc<Self>, mmio: &MmioBus) {
        let dev = Arc::clone(self);
        let handler =
            Arc::new(move |_start: usize, rwo: RWOp| dev.mmio_rw(rwo))
                as Arc<MmioFn>;
        mmio.register(TPM_CRB_BASE_ADDR, TPM_CRB_SIZE, handler)
            .expect("TPM CRB MMIO registration failed");
    }

    // ── Top-level MMIO dispatcher ─────────────────────────────────────────────

    fn mmio_rw(&self, rwo: RWOp) {
        let offset = rwo.offset();
        let locality = offset / TPM_LOCALITY_SIZE;
        let reg = offset % TPM_LOCALITY_SIZE;

        if locality != 0 {
            // Localities 1–4: reads return 0, writes are ignored.
            if let RWOp::Read(ro) = rwo {
                ro.fill(0);
            }
            return;
        }

        if reg >= TPM_DATA_BUFFER_OFFSET {
            self.data_buf_rw(reg - TPM_DATA_BUFFER_OFFSET, rwo);
        } else {
            self.control_reg_rw(reg, rwo);
        }
    }

    // ── Control register read/write ───────────────────────────────────────────

    fn control_reg_rw(&self, reg: usize, rwo: RWOp) {
        match rwo {
            RWOp::Read(ro) => {
                // Align to the nearest 4-byte slot for lookup; sub-word reads
                // return the appropriate bytes from that slot.
                let slot = reg & !3;
                let val = self.reg_read(slot);
                match ro.len() {
                    1 => ro.write_u8(val as u8),
                    2 => ro.write_u16(val as u16),
                    4 => ro.write_u32(val),
                    8 => {
                        let hi = self.reg_read(slot + 4);
                        ro.write_u64((val as u64) | ((hi as u64) << 32));
                    }
                    _ => ro.fill(0),
                }
            }
            RWOp::Write(wo) => {
                let val = match wo.len() {
                    1 => wo.read_u8() as u32,
                    2 => wo.read_u16() as u32,
                    4 => wo.read_u32(),
                    8 => wo.read_u64() as u32,
                    _ => return,
                };
                self.reg_write(reg, val);
            }
        }
    }

    fn reg_read(&self, reg: usize) -> u32 {
        let regs = self.regs.lock().unwrap();
        match reg {
            REG_LOC_STATE => regs.loc_state,
            REG_LOC_STS => regs.loc_sts,
            REG_INTF_ID_LO => INTF_ID_LO,
            REG_INTF_ID_HI => INTF_ID_HI,
            REG_CTRL_REQ => regs.ctrl_req,
            REG_CTRL_STS => regs.ctrl_sts,
            REG_CTRL_START => regs.ctrl_start,
            REG_INT_ENABLE => regs.int_enable,
            REG_INT_STS => regs.int_sts,
            REG_CTRL_CMD_SIZE => TPM_DATA_BUFFER_SIZE as u32,
            REG_CTRL_CMD_LADDR => DATA_BUF_ADDR as u32,
            REG_CTRL_CMD_HADDR => (DATA_BUF_ADDR >> 32) as u32,
            REG_CTRL_RSP_SIZE => TPM_DATA_BUFFER_SIZE as u32,
            REG_CTRL_RSP_ADDR_LO => DATA_BUF_ADDR as u32,
            REG_CTRL_RSP_ADDR_HI => (DATA_BUF_ADDR >> 32) as u32,
            _ => 0,
        }
    }

    fn reg_write(&self, reg: usize, val: u32) {
        let mut regs = self.regs.lock().unwrap();
        match reg {
            REG_LOC_CTRL => {
                // requestAccess: grant locality 0 unconditionally.
                if val & LOC_CTRL_REQUEST_ACCESS != 0 {
                    regs.loc_state |= LOC_STATE_LOC_ASSIGNED;
                    regs.loc_sts |= LOC_STS_GRANTED;
                }
                // relinquish and seize are no-ops for a single-locality model.
            }
            REG_CTRL_REQ => {
                if val & CTRL_REQ_CMD_READY != 0 {
                    regs.state = TpmState::Ready;
                    regs.ctrl_sts &= !(CTRL_STS_IDLE | CTRL_STS_NOT_READY);
                    regs.ctrl_req = CTRL_REQ_CMD_READY;
                }
                if val & CTRL_REQ_GO_IDLE != 0 {
                    regs.state = TpmState::Idle;
                    regs.ctrl_sts = CTRL_STS_IDLE | CTRL_STS_NOT_READY;
                    regs.ctrl_req = 0;
                }
            }
            REG_CTRL_CANCEL => {
                // TODO: propagate cancellation to backend
            }
            REG_CTRL_START => {
                if val & CTRL_START_START == 0 || regs.state != TpmState::Ready
                {
                    return;
                }
                regs.ctrl_start = CTRL_START_START;

                // Parse command size from TPM header bytes [2..6] (big-endian).
                let cmd_size = if regs.data_buf.len() >= 6 {
                    u32::from_be_bytes([
                        regs.data_buf[2],
                        regs.data_buf[3],
                        regs.data_buf[4],
                        regs.data_buf[5],
                    ]) as usize
                } else {
                    0
                };
                let cmd_len = cmd_size.min(TPM_DATA_BUFFER_SIZE);
                let cmd: Vec<u8> = regs.data_buf[..cmd_len].to_vec();

                // Release the lock before blocking on the backend.  The guest
                // vCPU is suspended during this MMIO exit so there is no
                // concurrent access to worry about, but holding a lock across
                // a blocking I/O call is bad practice regardless.
                drop(regs);

                let response = self.backend.execute_cmd(&cmd);

                let mut regs = self.regs.lock().unwrap();
                let rsp_len = response.len().min(TPM_DATA_BUFFER_SIZE);
                regs.data_buf[..rsp_len].copy_from_slice(&response[..rsp_len]);
                // Clear CTRL_START — signals to the guest that the command
                // has completed and the response is ready in the data buffer.
                regs.ctrl_start = 0;
            }
            REG_INT_ENABLE => regs.int_enable = val,
            REG_INT_STS => regs.int_sts &= !val, // W1C
            _ => {}
        }
    }

    // ── Data buffer read/write ────────────────────────────────────────────────

    fn data_buf_rw(&self, buf_offset: usize, rwo: RWOp) {
        let mut regs = self.regs.lock().unwrap();
        match rwo {
            RWOp::Read(ro) => {
                if buf_offset + ro.len() > TPM_DATA_BUFFER_SIZE {
                    ro.fill(0);
                } else {
                    ro.write_bytes(
                        &regs.data_buf[buf_offset..buf_offset + ro.len()],
                    );
                }
            }
            RWOp::Write(wo) => {
                if buf_offset + wo.len() <= TPM_DATA_BUFFER_SIZE {
                    wo.read_bytes(
                        &mut regs.data_buf[buf_offset..buf_offset + wo.len()],
                    );
                }
            }
        }
    }
}

impl Lifecycle for TpmCrb {
    fn type_name(&self) -> &'static str {
        DEVICE_NAME
    }

    fn reset(&self) {
        *self.regs.lock().unwrap() = CrbRegs::new();
    }
}
