//! Outer-frame codec: the three-varint header that wraps every record in a PBDEMS2 demo.
//!
//! `command | tick | payload size | payload`, where bit 64 of `command` means the payload
//! is Snappy-compressed. Nothing here decodes payloads — the writer copies them verbatim.

use anyhow::{bail, Result};

pub const HEADER_LEN: usize = 16;
pub const MAGIC: &[u8; 8] = b"PBDEMS2\0";
pub const COMPRESSED_BIT: u32 = 64;

pub const CMD_STOP: u32 = 0;
pub const CMD_FILE_HEADER: u32 = 1;
pub const CMD_FILE_INFO: u32 = 2;
pub const CMD_SYNC_TICK: u32 = 3;
pub const CMD_SEND_TABLES: u32 = 4;
pub const CMD_CLASS_INFO: u32 = 5;
pub const CMD_STRING_TABLES: u32 = 6;
pub const CMD_PACKET: u32 = 7;
pub const CMD_SIGNON_PACKET: u32 = 8;
pub const CMD_CONSOLE_CMD: u32 = 9;
pub const CMD_CUSTOM_DATA: u32 = 10;
pub const CMD_CUSTOM_DATA_CALLBACKS: u32 = 11;
pub const CMD_USER_CMD: u32 = 12;
pub const CMD_FULL_PACKET: u32 = 13;
pub const CMD_SAVE_GAME: u32 = 14;
pub const CMD_SPAWN_GROUPS: u32 = 15;
pub const CMD_ANIMATION_DATA: u32 = 16;
pub const CMD_ANIMATION_HEADER: u32 = 17;
pub const CMD_RECOVERY: u32 = 18;
pub const CMD_MAX: u32 = 18;

/// Init frames encode tick -1 as 0xffffffff.
pub const TICK_INIT: u32 = u32::MAX;

pub fn cmd_name(cmd: u32) -> &'static str {
    match cmd {
        CMD_STOP => "DEM_Stop",
        CMD_FILE_HEADER => "DEM_FileHeader",
        CMD_FILE_INFO => "DEM_FileInfo",
        CMD_SYNC_TICK => "DEM_SyncTick",
        CMD_SEND_TABLES => "DEM_SendTables",
        CMD_CLASS_INFO => "DEM_ClassInfo",
        CMD_STRING_TABLES => "DEM_StringTables",
        CMD_PACKET => "DEM_Packet",
        CMD_SIGNON_PACKET => "DEM_SignonPacket",
        CMD_CONSOLE_CMD => "DEM_ConsoleCmd",
        CMD_CUSTOM_DATA => "DEM_CustomData",
        CMD_CUSTOM_DATA_CALLBACKS => "DEM_CustomDataCallbacks",
        CMD_USER_CMD => "DEM_UserCmd",
        CMD_FULL_PACKET => "DEM_FullPacket",
        CMD_SAVE_GAME => "DEM_SaveGame",
        CMD_SPAWN_GROUPS => "DEM_SpawnGroups",
        CMD_ANIMATION_DATA => "DEM_AnimationData",
        CMD_ANIMATION_HEADER => "DEM_AnimationHeader",
        CMD_RECOVERY => "DEM_Recovery",
        _ => "DEM_Unknown",
    }
}

pub fn is_animation(cmd: u32) -> bool {
    cmd == CMD_ANIMATION_DATA || cmd == CMD_ANIMATION_HEADER
}

/// Frames that only ever appear after `DEM_Stop`, plus stop itself. Never copied from the
/// source body — the writer synthesises its own.
pub fn is_trailer_cmd(cmd: u32) -> bool {
    cmd == CMD_STOP || cmd == CMD_SPAWN_GROUPS || cmd == CMD_FILE_INFO
}

#[derive(Debug, Clone, Copy)]
pub struct FrameRef {
    pub index: usize,
    pub frame_offset: u64,
    pub payload_offset: u64,
    pub end_offset: u64,
    /// Command with the compression bit already stripped.
    pub cmd: u32,
    pub compressed: bool,
    pub tick_raw: u32,
    pub payload_len: u32,
}

impl FrameRef {
    /// Tick as the demo means it: 0xffffffff is -1, not four billion.
    pub fn tick(&self) -> i32 {
        self.tick_raw as i32
    }
    pub fn is_init_tick(&self) -> bool {
        self.tick_raw == TICK_INIT
    }
    pub fn total_len(&self) -> u64 {
        self.end_offset - self.frame_offset
    }
    pub fn name(&self) -> &'static str {
        cmd_name(self.cmd)
    }
    pub fn bytes<'a>(&self, demo: &'a [u8]) -> &'a [u8] {
        &demo[self.frame_offset as usize..self.end_offset as usize]
    }
    pub fn payload<'a>(&self, demo: &'a [u8]) -> &'a [u8] {
        &demo[self.payload_offset as usize..self.end_offset as usize]
    }
}

pub fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u32> {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= buf.len() {
            bail!("truncated varint at offset {}", pos);
        }
        let byte = buf[*pos];
        *pos += 1;
        result |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift > 35 {
            bail!("varint longer than 5 bytes at offset {}", pos);
        }
    }
}

pub fn write_varint(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

pub fn varint_len(value: u32) -> usize {
    match value {
        0..=0x7f => 1,
        0x80..=0x3fff => 2,
        0x4000..=0x1f_ffff => 3,
        0x20_0000..=0xfff_ffff => 4,
        _ => 5,
    }
}

/// Encode a frame header (no payload).
pub fn frame_header(cmd: u32, compressed: bool, tick_raw: u32, payload_len: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    let raw_cmd = if compressed {
        cmd | COMPRESSED_BIT
    } else {
        cmd
    };
    write_varint(&mut out, raw_cmd);
    write_varint(&mut out, tick_raw);
    write_varint(&mut out, payload_len);
    out
}

pub fn frame_header_len(cmd: u32, compressed: bool, tick_raw: u32, payload_len: u32) -> usize {
    let raw_cmd = if compressed {
        cmd | COMPRESSED_BIT
    } else {
        cmd
    };
    varint_len(raw_cmd) + varint_len(tick_raw) + varint_len(payload_len)
}

/// Scan every outer frame from `HEADER_LEN` to EOF.
///
/// Strict on purpose: a demo we cannot walk exactly is a demo we must not splice. Returns
/// the frames and the offset scanning stopped at, which must equal the file length.
pub fn scan(demo: &[u8]) -> Result<(Vec<FrameRef>, usize)> {
    if demo.len() < HEADER_LEN {
        bail!("file is shorter than the 16-byte short header");
    }
    if &demo[..8] != MAGIC {
        if &demo[..8] == b"HL2DEMO\0" {
            bail!("this is a Source 1 (HL2DEMO) demo; only PBDEMS2/CS2 demos are supported");
        }
        bail!("bad magic: expected PBDEMS2\\0");
    }

    let mut frames = Vec::new();
    let mut pos = HEADER_LEN;
    let mut index = 0usize;

    while pos + 3 <= demo.len() {
        let frame_offset = pos;
        let raw_cmd = read_varint(demo, &mut pos)?;
        let tick_raw = read_varint(demo, &mut pos)?;
        let payload_len = read_varint(demo, &mut pos)?;

        let compressed = raw_cmd & COMPRESSED_BIT == COMPRESSED_BIT;
        let cmd = raw_cmd & !COMPRESSED_BIT;
        if cmd > CMD_MAX {
            bail!(
                "unknown command {} in frame {} at offset {}",
                cmd,
                index,
                frame_offset
            );
        }
        let payload_offset = pos;
        let end_offset = payload_offset
            .checked_add(payload_len as usize)
            .filter(|end| *end <= demo.len())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "frame {} at offset {} claims {} payload bytes, past end of file",
                    index,
                    frame_offset,
                    payload_len
                )
            })?;

        frames.push(FrameRef {
            index,
            frame_offset: frame_offset as u64,
            payload_offset: payload_offset as u64,
            end_offset: end_offset as u64,
            cmd,
            compressed,
            tick_raw,
            payload_len,
        });
        pos = end_offset;
        index += 1;
    }

    Ok((frames, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for value in [
            0u32,
            1,
            63,
            64,
            127,
            128,
            255,
            16_383,
            16_384,
            57_985,
            96_317,
            u32::MAX - 1,
            u32::MAX,
        ] {
            let mut buf = Vec::new();
            write_varint(&mut buf, value);
            assert_eq!(buf.len(), varint_len(value), "length mismatch for {value}");
            let mut pos = 0;
            assert_eq!(read_varint(&buf, &mut pos).unwrap(), value);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn init_tick_reads_as_minus_one() {
        let frame = FrameRef {
            index: 0,
            frame_offset: 0,
            payload_offset: 0,
            end_offset: 0,
            cmd: CMD_FILE_HEADER,
            compressed: false,
            tick_raw: TICK_INIT,
            payload_len: 0,
        };
        assert_eq!(frame.tick(), -1);
        assert!(frame.is_init_tick());
    }

    #[test]
    fn truncated_varint_is_rejected() {
        let buf = [0x80u8, 0x80];
        let mut pos = 0;
        assert!(read_varint(&buf, &mut pos).is_err());
    }

    #[test]
    fn scan_rejects_bad_magic() {
        let mut buf = vec![0u8; 32];
        buf[..8].copy_from_slice(b"NOTADEMO");
        assert!(scan(&buf).is_err());
    }

    #[test]
    fn scan_walks_synthetic_frames_exactly() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        // three frames with varint widths 1, 2 and 3 in the size field
        for (cmd, tick, size) in [
            (CMD_PACKET, 1u32, 5usize),
            (CMD_PACKET, 200, 300),
            (CMD_STOP, 57_985, 0),
        ] {
            buf.extend_from_slice(&frame_header(cmd, false, tick, size as u32));
            buf.extend(std::iter::repeat(0xabu8).take(size));
        }
        let (frames, end) = scan(&buf).unwrap();
        assert_eq!(end, buf.len());
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1].payload_len, 300);
        assert_eq!(frames[2].cmd, CMD_STOP);
        assert_eq!(frames[2].tick(), 57_985);
    }

    #[test]
    fn scan_detects_payload_past_eof() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&frame_header(CMD_PACKET, false, 1, 9_999));
        buf.extend_from_slice(&[0u8; 10]);
        assert!(scan(&buf).is_err());
    }
}
