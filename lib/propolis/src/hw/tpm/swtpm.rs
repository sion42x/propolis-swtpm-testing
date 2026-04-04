// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! swtpm backend: connects to a running swtpm instance over a Unix domain
//! socket and forwards TPM 2.0 command/response traffic.
//!
//! ## Starting swtpm
//!
//! ```sh
//! mkdir /tmp/mytpm
//! swtpm socket --tpmstate dir=/tmp/mytpm \
//!   --ctrl  type=unixio,path=/tmp/mytpm.ctrl \
//!   --server type=unixio,path=/tmp/mytpm.sock \
//!   --tpm2 --log level=20
//! ```
//!
//! Pass the data socket path (`/tmp/mytpm.sock` above) to [`SwtpmBackend::new`].
//!
//! ## Protocol
//!
//! The swtpm data socket carries raw TPM 2.0 command/response bytes with no
//! additional framing.  A command is written in full; the response is read by
//! first consuming the 6-byte TPM header (which contains the total response
//! length) and then reading the remainder.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::TpmBackend;

// ── TPM 2.0 constants ─────────────────────────────────────────────────────────

/// TPM response header size: tag (2) + size (4) + code (4).
const TPM_HEADER_SIZE: usize = 10;

/// TPM_ST_NO_SESSIONS (big-endian): 0x8001.
const TPM2_ST_NO_SESSIONS: u16 = 0x8001;

/// TPM_RC_FAILURE.
const TPM2_RC_FAILURE: u32 = 0x0000_0101;


// ── SwtpmBackend ──────────────────────────────────────────────────────────────

/// TPM backend that communicates with a running swtpm process via a Unix
/// domain socket.
pub struct SwtpmBackend {
    socket_path: PathBuf,
    /// Cached connection to the swtpm data socket.  Taking the Option out of
    /// the Mutex and returning it at the end of a call lets us reuse the
    /// connection across commands without holding the lock during I/O.
    stream: Mutex<Option<UnixStream>>,
}

impl SwtpmBackend {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            stream: Mutex::new(None),
        }
    }

    /// Return an open connection to swtpm, reusing the cached one if possible.
    fn get_or_connect(&self) -> io::Result<UnixStream> {
        let mut guard = self.stream.lock().unwrap();
        match guard.take() {
            Some(s) => Ok(s),
            None => {
                let mut s = UnixStream::connect(&self.socket_path)?;
                Ok(s)
            }
        }
    }

    /// Send `cmd` to swtpm and return the full response, including header.
    fn send_cmd(&self, cmd: &[u8]) -> io::Result<Vec<u8>> {
        eprintln!("[swtpm] connecting to {:?}", self.socket_path);
        let mut stream = self.get_or_connect()?;
        eprintln!("[swtpm] sending {} bytes: {:02x?}", cmd.len(), &cmd[..cmd.len().min(10)]);

        stream.write_all(cmd)?;
        stream.flush()?;
        eprintln!("[swtpm] sent, waiting for response");

        // Read the first 6 bytes to learn the total response length.
        let mut header = [0u8; 6];
        stream.read_exact(&mut header)?;

        let rsp_size = u32::from_be_bytes([
            header[2], header[3], header[4], header[5],
        ]) as usize;

        if rsp_size < TPM_HEADER_SIZE || rsp_size > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("swtpm: implausible response size {rsp_size}"),
            ));
        }

        let mut response = Vec::with_capacity(rsp_size);
        response.extend_from_slice(&header);
        if rsp_size > 6 {
            response.resize(rsp_size, 0);
            stream.read_exact(&mut response[6..])?;
        }

        eprintln!("[swtpm] response {} bytes: {:02x?}", response.len(), &response[..response.len().min(10)]);
        // Stash the connection for the next command.
        *self.stream.lock().unwrap() = Some(stream);
        Ok(response)
    }
}

impl TpmBackend for SwtpmBackend {
    fn execute_cmd(&self, cmd: &[u8]) -> Vec<u8> {
        if cmd.len() < TPM_HEADER_SIZE {
            return tpm2_error_response(TPM2_RC_FAILURE);
        }
        match self.send_cmd(cmd) {
            Ok(rsp) => rsp,
            Err(e) => {
                // Drop the cached connection so the next command reconnects.
                *self.stream.lock().unwrap() = None;
                eprintln!("swtpm: command error: {e}");
                tpm2_error_response(TPM2_RC_FAILURE)
            }
        }
    }
}

/// Build a TPM_ST_NO_SESSIONS response carrying `rc` as the response code.
fn tpm2_error_response(rc: u32) -> Vec<u8> {
    let mut rsp = vec![0u8; TPM_HEADER_SIZE];
    rsp[0..2].copy_from_slice(&TPM2_ST_NO_SESSIONS.to_be_bytes());
    rsp[2..6].copy_from_slice(&(TPM_HEADER_SIZE as u32).to_be_bytes());
    rsp[6..10].copy_from_slice(&rc.to_be_bytes());
    rsp
}
