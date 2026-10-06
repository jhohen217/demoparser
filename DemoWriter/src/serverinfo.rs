//! Pull `svc_ServerInfo` out of a demo's signon block.
//!
//! This is the message the version gate reads: CS2 refuses a demo with
//! `NETWORK_DISCONNECT_REPLAY_INCOMPATIBLE (Network version N is incompatible.)` when its
//! `protocol` does not match the running client.

use crate::frame::*;
use crate::index::DemoIndex;
use anyhow::{Context, Result};
use csgoproto::{CDemoPacket, CsvcMsgServerInfo};
use parser::first_pass::read_bits::Bitreader;
use prost::Message;

const SVC_SERVER_INFO: u32 = 40;

pub struct FoundServerInfo {
    pub frame_index: usize,
    pub info: CsvcMsgServerInfo,
}

pub fn find(demo: &[u8], index: &DemoIndex) -> Result<Option<FoundServerInfo>> {
    for frame in &index.frames {
        if frame.cmd != CMD_SIGNON_PACKET && frame.cmd != CMD_PACKET {
            continue;
        }
        let raw = frame.payload(demo);
        let decoded = if frame.compressed {
            match snap::raw::Decoder::new().decompress_vec(raw) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            }
        } else {
            raw.to_vec()
        };
        let packet = match CDemoPacket::decode(&decoded[..]) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let Some(data) = packet.data else { continue };
        let mut bitreader = Bitreader::new(&data);
        while bitreader.bits_remaining().unwrap_or(0) > 8 {
            let msg_type = match bitreader.read_u_bit_var() {
                Ok(t) => t,
                Err(_) => break,
            };
            let size = match bitreader.read_varint() {
                Ok(s) => s,
                Err(_) => break,
            };
            let bytes = match bitreader.read_n_bytes(size as usize) {
                Ok(b) => b,
                Err(_) => break,
            };
            if msg_type == SVC_SERVER_INFO {
                let info = CsvcMsgServerInfo::decode(&bytes[..])
                    .context("could not decode svc_ServerInfo")?;
                return Ok(Some(FoundServerInfo {
                    frame_index: frame.index,
                    info,
                }));
            }
        }
    }
    Ok(None)
}
