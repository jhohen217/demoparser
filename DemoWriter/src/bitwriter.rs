//! LSB-first bit writer, the exact inverse of `parser::first_pass::read_bits::Bitreader`.
//!
//! Inner packet payloads (`CDemoPacket.data`) are a bit-packed stream of
//! `u_bit_var message type | varint size | size bytes`. Editing one message means
//! rebuilding the whole stream, so this has to round-trip byte-for-byte — see
//! `roundtrip_packet_is_identical`.

pub struct BitWriter {
    bytes: Vec<u8>,
    /// Bits already written into the byte under construction (0..8).
    bit_count: u32,
    current: u32,
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter {
            bytes: Vec::new(),
            bit_count: 0,
            current: 0,
        }
    }

    pub fn write_nbits(&mut self, value: u32, n: u32) {
        for i in 0..n {
            let bit = (value >> i) & 1;
            self.current |= bit << self.bit_count;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.bytes.push(self.current as u8);
                self.current = 0;
                self.bit_count = 0;
            }
        }
    }

    /// Inverse of `read_u_bit_var`: 6 bits, where bits 4-5 select how many more follow.
    pub fn write_u_bit_var(&mut self, value: u32) {
        if value < 16 {
            self.write_nbits(value, 6);
        } else if value < (1 << 8) {
            self.write_nbits((value & 0b1111) | 0b010000, 6);
            self.write_nbits(value >> 4, 4);
        } else if value < (1 << 12) {
            self.write_nbits((value & 0b1111) | 0b100000, 6);
            self.write_nbits(value >> 4, 8);
        } else {
            self.write_nbits((value & 0b1111) | 0b110000, 6);
            self.write_nbits(value >> 4, 28);
        }
    }

    pub fn write_varint(&mut self, mut value: u32) {
        loop {
            let mut byte = value & 0x7f;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            self.write_nbits(byte, 8);
            if value == 0 {
                return;
            }
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.write_nbits(*byte as u32, 8);
        }
    }

    /// Flush the partial byte with zero padding, exactly as the source stream does.
    pub fn finish(mut self) -> Vec<u8> {
        if self.bit_count > 0 {
            self.bytes.push(self.current as u8);
        }
        self.bytes
    }

    pub fn bits_written(&self) -> usize {
        self.bytes.len() * 8 + self.bit_count as usize
    }
}

/// One message from an inner packet stream.
#[derive(Debug, Clone)]
pub struct NetMessage {
    pub msg_type: u32,
    pub payload: Vec<u8>,
}

/// Split a `CDemoPacket.data` blob into its messages.
pub fn read_messages(data: &[u8]) -> anyhow::Result<Vec<NetMessage>> {
    use parser::first_pass::read_bits::Bitreader;
    let mut bitreader = Bitreader::new(data);
    let mut out = Vec::new();
    while bitreader.bits_remaining().unwrap_or(0) > 8 {
        let msg_type = bitreader.read_u_bit_var()?;
        let size = bitreader.read_varint()?;
        let payload = bitreader.read_n_bytes(size as usize)?;
        out.push(NetMessage { msg_type, payload });
    }
    Ok(out)
}

/// Rebuild a `CDemoPacket.data` blob from messages. Returns the bytes and how many bits
/// of the last one are meaningful — the source pads the tail with leftover encoder bits
/// rather than zeros, and the reader ignores them, so only these bits can be compared.
pub fn write_messages_with_bits(messages: &[NetMessage]) -> (Vec<u8>, usize) {
    let mut writer = BitWriter::new();
    for message in messages {
        writer.write_u_bit_var(message.msg_type);
        writer.write_varint(message.payload.len() as u32);
        writer.write_bytes(&message.payload);
    }
    let bits = writer.bits_written();
    (writer.finish(), bits)
}

pub fn write_messages(messages: &[NetMessage]) -> Vec<u8> {
    write_messages_with_bits(messages).0
}

/// Compare a rebuilt stream against the original, ignoring the padding bits.
pub fn streams_match(original: &[u8], rebuilt: &[u8], bits: usize) -> bool {
    let whole = bits / 8;
    let spare = bits % 8;
    if original.len() < whole || rebuilt.len() < whole {
        return false;
    }
    if original[..whole] != rebuilt[..whole] {
        return false;
    }
    if spare == 0 {
        return true;
    }
    let mask = ((1u16 << spare) - 1) as u8;
    match (original.get(whole), rebuilt.get(whole)) {
        (Some(a), Some(b)) => a & mask == b & mask,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::first_pass::read_bits::Bitreader;

    #[test]
    fn nbits_roundtrip() {
        let cases = [
            (0u32, 1u32),
            (1, 1),
            (5, 6),
            (63, 6),
            (255, 8),
            (4095, 12),
            (1 << 27, 28),
        ];
        let mut writer = BitWriter::new();
        for (value, bits) in cases {
            writer.write_nbits(value, bits);
        }
        let encoded = writer.finish();
        let mut reader = Bitreader::new(&encoded);
        for (value, bits) in cases {
            assert_eq!(reader.read_nbits(bits).unwrap(), value);
        }
    }

    #[test]
    fn u_bit_var_roundtrip() {
        // Values either side of every width boundary the encoding switches on.
        let values = [0u32, 1, 15, 16, 17, 255, 256, 4095, 4096, 40, 76, 1_000_000];
        let mut writer = BitWriter::new();
        for value in values {
            writer.write_u_bit_var(value);
        }
        let encoded = writer.finish();
        let mut reader = Bitreader::new(&encoded);
        for value in values {
            assert_eq!(reader.read_u_bit_var().unwrap(), value, "u_bit_var {value}");
        }
    }

    #[test]
    fn varint_roundtrip() {
        let values = [0u32, 1, 127, 128, 300, 16_383, 16_384, 1 << 21, u32::MAX];
        let mut writer = BitWriter::new();
        for value in values {
            writer.write_varint(value);
        }
        let encoded = writer.finish();
        let mut reader = Bitreader::new(&encoded);
        for value in values {
            assert_eq!(reader.read_varint().unwrap(), value, "varint {value}");
        }
    }

    #[test]
    fn message_stream_roundtrip() {
        let messages = vec![
            NetMessage {
                msg_type: 40,
                payload: vec![1, 2, 3, 4, 5],
            },
            NetMessage {
                msg_type: 4,
                payload: vec![],
            },
            NetMessage {
                msg_type: 300,
                payload: (0..200).map(|i| i as u8).collect(),
            },
            NetMessage {
                msg_type: 7,
                payload: vec![0xff; 3],
            },
        ];
        let encoded = write_messages(&messages);
        let decoded = read_messages(&encoded).unwrap();
        assert_eq!(decoded.len(), messages.len());
        for (a, b) in decoded.iter().zip(&messages) {
            assert_eq!(a.msg_type, b.msg_type);
            assert_eq!(a.payload, b.payload);
        }
    }
}
