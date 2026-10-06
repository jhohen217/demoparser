//! Tag 12/schema 2: dictionary-coded lossless utility values, bounded codec envelope + CRC32.
use anyhow::{ensure, Result};
use parser::second_pass::utility::{UtilityData, UtilityValue};
use ahash::AHashMap as HashMap;
use std::io::{Read, Write};
use super::utility::MAX_UTILITY_JSON;

fn u32v(out: &mut Vec<u8>, v: u32) { out.extend(v.to_le_bytes()); }
fn sized(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    u32v(out, u32::try_from(bytes.len())?); out.extend(bytes); Ok(())
}
fn value(out: &mut Vec<u8>, v: Option<&UtilityValue>) -> Result<()> {
    use UtilityValue::*;
    match v {
        None => out.push(0),
        Some(Bool(v)) => { out.extend([1, u8::from(*v)]); }
        Some(U32(v)) => { out.push(2); u32v(out, *v); }
        Some(I32(v)) => { out.push(3); out.extend(v.to_le_bytes()); }
        Some(U64(v)) => { out.push(4); out.extend(v.to_le_bytes()); }
        Some(F32Bits(v)) => { out.push(5); u32v(out, *v); }
        Some(String(v)) => { out.push(6); sized(out, v.as_bytes())?; }
        Some(VecXYBits(v)) => { out.push(7); for x in v { u32v(out, *x); } }
        Some(VecXYZBits(v)) => { out.push(8); for x in v { u32v(out, *x); } }
        Some(U32Vec(v)) => { out.push(9); u32v(out, u32::try_from(v.len())?); for x in v { u32v(out, *x); } }
        Some(U64Vec(v)) => { out.push(10); u32v(out, u32::try_from(v.len())?); for x in v { out.extend(x.to_le_bytes()); } }
        Some(StringVec(v)) => { out.push(11); u32v(out, u32::try_from(v.len())?); for x in v { sized(out, x.as_bytes())?; } }
        Some(Other(v)) => { out.push(12); sized(out, &serde_json::to_vec(v)?)?; }
    }
    Ok(())
}
const ACTIONS: [&str; 8] = ["create", "snapshot", "update", "delete", "leave", "dormant", "replaced", "event"];

pub fn encode(data: Option<&UtilityData>, max_tick: i32) -> Result<(Vec<u8>, u32)> {
    encode_with_codec(data, max_tick, 1)
}

pub fn encode_with_codec(data: Option<&UtilityData>, max_tick: i32, codec: u8) -> Result<(Vec<u8>, u32)> {
    let rows: Vec<_> = data.into_iter().flat_map(|d| &d.records).filter(|r| r.tick <= max_tick).collect();
    let count = u32::try_from(rows.len())?;
    let mut strings = Vec::<&str>::new();
    let mut names = HashMap::<&str, u32>::new();
    let mut descriptors = Vec::<(&str, &[i32])>::new();
    let mut fields = HashMap::<(&str, &[i32]), u32>::new();
    for row in &rows {
        if !names.contains_key(row.name.as_str()) { names.insert(&row.name, strings.len() as u32); strings.push(&row.name); }
        for field in &row.fields {
            if !names.contains_key(field.name.as_str()) { names.insert(&field.name, strings.len() as u32); strings.push(&field.name); }
            let key = (field.name.as_str(), field.path.as_slice());
            if !fields.contains_key(&key) { fields.insert(key, descriptors.len() as u32); descriptors.push(key); }
        }
    }
    let mut raw = Vec::with_capacity(rows.len().saturating_mul(100).min(MAX_UTILITY_JSON));
    raw.extend(b"UTB2");
    raw.extend([u8::from(data.is_some_and(|d| d.captured)), u8::from(data.and_then(|d|d.server_tick_offset).is_some()), 0, 0]);
    raw.extend(data.and_then(|d|d.server_tick_offset).unwrap_or(0).to_le_bytes());
    u32v(&mut raw, count); u32v(&mut raw, strings.len() as u32); u32v(&mut raw, descriptors.len() as u32);
    for name in strings { sized(&mut raw, name.as_bytes())?; }
    for (name, path) in descriptors {
        u32v(&mut raw, names[name]); u32v(&mut raw, u32::try_from(path.len())?);
        for index in path { ensure!(*index >= 0, "Negative utility path"); raw.extend(index.to_le_bytes()); }
    }
    for row in rows {
        raw.extend(row.tick.to_le_bytes()); raw.extend(row.frame_offset.to_le_bytes());
        u32v(&mut raw, row.message_index); raw.extend(row.sequence.to_le_bytes());
        raw.push(ACTIONS.iter().position(|a| *a == row.action).ok_or_else(|| anyhow::anyhow!("Unknown utility action"))? as u8);
        raw.push(u8::from(row.entity_id.is_some()) | (u8::from(row.serial.is_some()) << 1)
            | (u8::from(row.position.is_some()) << 2) | (u8::from(row.thrower_steamid.is_some()) << 3));
        u32v(&mut raw, names[row.name.as_str()]); u32v(&mut raw, u32::try_from(row.fields.len())?);
        if let Some(v) = row.entity_id { raw.extend(v.to_le_bytes()); }
        if let Some(v) = row.serial { u32v(&mut raw, v); }
        if let Some(v) = row.position { for x in v { raw.extend(x.to_le_bytes()); } }
        if let Some(v) = row.thrower_steamid { raw.extend(v.to_le_bytes()); }
        for field in &row.fields {
            u32v(&mut raw, fields[&(field.name.as_str(), field.path.as_slice())]); value(&mut raw, field.value.as_ref())?;
        }
        ensure!(raw.len() <= MAX_UTILITY_JSON, "Utility authority exceeds reader limit");
    }
    let mut payload = (raw.len() as u32).to_le_bytes().to_vec();
    payload.extend([codec, 0, 0, 0]);
    u32v(&mut payload, crc32fast::hash(&raw));
    match codec {
        0 => payload.extend(&raw),
        1 => payload.extend(lz4_flex::block::compress(&raw)),
        2 => {
            let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gzip.write_all(&raw)?; payload.extend(gzip.finish()?);
        }
        3 => payload.extend(zstd::bulk::compress(&raw, 1)?),
        _ => anyhow::bail!("Unsupported utility codec"),
    }
    Ok((payload, count))
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> { let result = self.0.get(..n)?; self.0 = &self.0[n..]; Some(result) }
    fn byte(&mut self) -> Option<u8> { Some(self.take(1)?[0]) }
    fn uint(&mut self) -> Option<u32> { Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?)) }
    fn text(&mut self) -> Option<&'a str> { let n = self.uint()? as usize; std::str::from_utf8(self.take(n)?).ok() }
    fn skip_value(&mut self) -> Option<()> {
        match self.byte()? {
            0 => (), 1 => { if self.byte()? > 1 { return None; } }
            2 | 3 | 5 => { self.take(4)?; }
            4 | 7 => { self.take(8)?; }
            8 => { self.take(12)?; }
            6 => { self.text()?; }
            12 => { serde_json::from_str::<serde::de::IgnoredAny>(self.text()?).ok()?; }
            9 | 10 => { return None; } // handled with explicit widths below
            11 => { let n = self.uint()?; if n as usize > self.0.len()/4 { return None; } for _ in 0..n { self.text()?; } }
            _ => return None,
        } Some(())
    }
}
fn validate(raw: &[u8], declared: u32) -> Option<bool> {
    let mut r = Cursor(raw);
    if r.take(4)? != b"UTB2" { return None; }
    let captured = r.byte()?; if captured > 1 || r.byte()? > 1 || r.take(2)? != [0, 0] { return None; }
    r.take(4)?;
    let count = r.uint()?; if count != declared || captured == 0 && count != 0 { return None; }
    let strings = r.uint()?; let fields = r.uint()?;
    if strings as usize > r.0.len()/4 { return None; }
    let mut names = Vec::with_capacity(strings as usize);
    for _ in 0..strings { let name = r.text()?; if name.is_empty() { return None; } names.push(name); }
    if fields as usize > r.0.len()/8 { return None; }
    for _ in 0..fields {
        if r.uint()? >= strings { return None; }
        let count = r.uint()? as usize;
        if count > r.0.len()/4 { return None; }
        for _ in 0..count { if r.uint()? > i32::MAX as u32 { return None; } }
    }
    if count as usize > r.0.len()/34 { return None; }
    let mut previous = None;
    for _ in 0..count {
        r.take(4)?;
        let offset = u64::from_le_bytes(r.take(8)?.try_into().ok()?);
        let message = r.uint()?;
        let sequence = u64::from_le_bytes(r.take(8)?.try_into().ok()?);
        let order = (offset, message, sequence);
        if previous.is_some_and(|before| order <= before) { return None; }
        previous = Some(order);
        let action = r.byte()?; if action > 7 { return None; }
        let flags = r.byte()?; let name = *names.get(r.uint()? as usize)?;
        if flags > 15 || (action == 7 && flags != 0) || (action != 7 && (flags & 3 != 3
            || !matches!(name, "CFlashbangProjectile" | "CHEGrenadeProjectile" | "CSmokeGrenadeProjectile"
                | "CMolotovProjectile" | "CDecoyProjectile" | "CInferno"))) { return None; }
        let n = r.uint()?;
        if flags & 1 != 0 && r.uint()? > i32::MAX as u32 { return None; }
        if flags & 2 != 0 { r.take(4)?; }
        if flags & 4 != 0 { for _ in 0..3 { if !f32::from_bits(r.uint()?).is_finite() { return None; } } }
        if flags & 8 != 0 && r.take(8)? == [0; 8] { return None; }
        if n as usize > r.0.len()/5 { return None; }
        for _ in 0..n {
            if r.uint()? >= fields { return None; }
            let tag = *r.0.first()?;
            if tag == 9 || tag == 10 {
                r.byte()?; let count = r.uint()? as usize;
                r.take(count.checked_mul(if tag == 9 { 4 } else { 8 })?)?;
            } else { r.skip_value()?; }
        }
    }
    if !r.0.is_empty() { return None; }
    Some(captured == 1)
}
pub fn is_captured(payload: &[u8], declared: u32) -> bool {
    let Some(header) = payload.get(..12) else { return false; };
    let length = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    if length > MAX_UTILITY_JSON || header[5..8] != [0, 0, 0] { return false; }
    let mut raw = Vec::new();
    match header[4] {
        0 => { if payload.len()-12 != length { return false; } raw.extend(&payload[12..]); }
        1 => { raw.resize(length, 0); if lz4_flex::block::decompress_into(&payload[12..], &mut raw).ok() != Some(length) { return false; } }
        2 => { if flate2::read::GzDecoder::new(&payload[12..]).take(length as u64 + 1).read_to_end(&mut raw).is_err() { return false; } }
        3 => { let Ok(decoded) = zstd::bulk::decompress(&payload[12..], length) else { return false; }; raw = decoded; }
        _ => return false,
    }
    if raw.len() != length || crc32fast::hash(&raw) != u32::from_le_bytes(header[8..12].try_into().unwrap()) { return false; }
    validate(&raw, declared) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::second_pass::utility::{UtilityField, UtilityRecord};
    fn data() -> UtilityData {
        UtilityData { captured: true, server_tick_offset: Some(500), records: vec![UtilityRecord {
            tick: 2, frame_offset: 100, message_index: 3, sequence: 1, action: "snapshot".into(),
            entity_id: Some(17), serial: Some(4), name: "CDecoyProjectile".into(), position: None, thrower_steamid: None,
            fields: vec![UtilityField { name: "unknown_float".into(), path: vec![2, 3], value: Some(UtilityValue::F32Bits(0x7fc01234)) }],
        }] }
    }
    #[test]
    fn codecs_counts_crc_truncation_and_missing_capture() {
        for codec in 0..4 {
            let (payload, count) = encode_with_codec(Some(&data()), 2, codec).unwrap();
            assert_eq!(count, 1); assert!(is_captured(&payload, count));
            assert!(!is_captured(&payload, 2));
            for end in 0..payload.len() { assert!(!is_captured(&payload[..end], 1)); }
            for at in [4, 5, 8, payload.len()-1] {
                let mut broken = payload.clone(); broken[at] ^= 0xff; assert!(!is_captured(&broken, 1));
            }
            let mut oversized = payload.clone(); oversized[..4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(!is_captured(&oversized, 1));
            assert!(!is_captured(&encode_with_codec(None, 2, codec).unwrap().0, 0));
        }
    }
    #[test]
    fn rejects_invalid_semantics_even_with_valid_checksum() {
        let mut d = data(); d.records[0].entity_id = Some(-1);
        assert!(!is_captured(&encode(Some(&d), 2).unwrap().0, 1));
        d = data(); d.records[0].position = Some([f32::NAN, 0., 0.]);
        assert!(!is_captured(&encode(Some(&d), 2).unwrap().0, 1));
        d = data(); d.records.push(d.records[0].clone());
        assert!(!is_captured(&encode(Some(&d), 2).unwrap().0, 2));
        d = data(); d.records[0].thrower_steamid = Some(0);
        assert!(!is_captured(&encode(Some(&d), 2).unwrap().0, 1));
        d = data(); d.records[0].fields[0].path = vec![-1];
        assert!(encode(Some(&d), 2).is_err());
    }
}
