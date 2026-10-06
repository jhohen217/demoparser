//! Experimental: rewrite which map a demo asks the client to load.
//!
//! Map identity lives in three places, and only the last actually loads anything:
//!   * `DEM_FileHeader.map_name`        — metadata, what the UI shows
//!   * `svc_ServerInfo.map_name`        — session metadata
//!   * `CNETMsg_SpawnGroup_Load`        — the world the engine actually loads
//!
//! Note this cannot make a demo *make sense* on another map: entity positions are
//! coordinates in the original map's space. It is meaningful only for pointing a demo at a
//! different copy of the same map — an official map instead of a workshop addon, say.

use crate::bitwriter::{self, NetMessage};
use crate::frame::*;
use crate::index::DemoIndex;
use anyhow::{bail, Context, Result};
use csgoproto::{
    CDemoFileHeader, CDemoPacket, CnetMsgSignonState, CnetMsgSpawnGroupLoad, CsvcMsgServerInfo,
};
use prost::Message;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

const SVC_SERVER_INFO: u32 = 40;
const NET_SPAWN_GROUP_LOAD: u32 = 8;
const NET_SIGNON_STATE: u32 = 7;

pub struct RetargetOptions<'a> {
    pub map: &'a str,
    /// Workshop published file id. CS2 identifies workshop maps as
    /// `@workshop/<pubfileid>/<mapname>`; the id goes in the addon fields and is what the
    /// client feeds to its addon-download loop. Note the engine always resolves an id to
    /// the item's latest version — `host_workshop_map` says so in its own help text — so
    /// this selects a copy, never a version.
    pub addon: Option<&'a str>,
    /// Replacement world name for prefab spawn groups (skybox and friends). When absent,
    /// prefab spawn groups belonging to the old map are dropped.
    pub sky: Option<&'a str>,
    /// Keep the resource manifests. They list the old map's assets, so clearing them is
    /// usually what you want.
    pub keep_manifests: bool,
}

pub struct RetargetReport {
    pub output_bytes: u64,
    pub file_header_patched: bool,
    pub server_info_patched: usize,
    pub spawn_groups_patched: usize,
    pub spawn_groups_dropped: usize,
    pub signon_states_patched: usize,
    pub packets_rewritten: usize,
}

fn patch_messages(
    messages: &mut Vec<NetMessage>,
    options: &RetargetOptions,
    old_map: &str,
    report: &mut RetargetReport,
) -> Result<bool> {
    let mut changed = false;
    let mut keep = Vec::with_capacity(messages.len());

    for message in messages.drain(..) {
        match message.msg_type {
            SVC_SERVER_INFO => {
                let mut info = CsvcMsgServerInfo::decode(&message.payload[..])
                    .context("decoding svc_ServerInfo")?;
                info.map_name = Some(options.map.to_string());
                if let Some(addon) = options.addon {
                    info.addon_name = Some(addon.to_string());
                }
                // The session config carries its own copy, which drives the loading-screen
                // title and the maps/<name>_camera_nodes.kv3 lookup.
                if let Some(config) = info.game_session_config.as_mut() {
                    if config.s1_mapname.as_deref() == Some(old_map) {
                        config.s1_mapname = Some(options.map.to_string());
                    }
                }
                if !options.keep_manifests {
                    info.game_session_manifest = None;
                }
                let mut payload = Vec::with_capacity(info.encoded_len());
                info.encode(&mut payload)?;
                report.server_info_patched += 1;
                changed = true;
                keep.push(NetMessage {
                    msg_type: message.msg_type,
                    payload,
                });
            }
            NET_SPAWN_GROUP_LOAD => {
                let mut load = CnetMsgSpawnGroupLoad::decode(&message.payload[..])
                    .context("decoding CNETMsg_SpawnGroup_Load")?;
                let world = load.worldname().to_string();
                let is_main = world == old_map;
                let is_old_prefab = world.contains(old_map) && !is_main;

                if is_main {
                    load.worldname = Some(options.map.to_string());
                    if let Some(lump) = load.entitylumpname.clone() {
                        load.entitylumpname = Some(lump.replace(old_map, options.map));
                    }
                } else if is_old_prefab {
                    match options.sky {
                        Some(sky) => load.worldname = Some(sky.to_string()),
                        None => {
                            report.spawn_groups_dropped += 1;
                            changed = true;
                            continue; // drop it entirely
                        }
                    }
                } else {
                    // map-independent prefab (team intros, end_of_match) — leave alone
                    keep.push(message);
                    continue;
                }

                if !options.keep_manifests {
                    load.spawngroupmanifest = None;
                }
                let mut payload = Vec::with_capacity(load.encoded_len());
                load.encode(&mut payload)?;
                report.spawn_groups_patched += 1;
                changed = true;
                keep.push(NetMessage {
                    msg_type: message.msg_type,
                    payload,
                });
            }
            NET_SIGNON_STATE => {
                let mut state = CnetMsgSignonState::decode(&message.payload[..])
                    .context("decoding CNETMsg_SignonState")?;
                let mut touched = false;
                if state.map_name.as_deref() == Some(old_map) {
                    state.map_name = Some(options.map.to_string());
                    touched = true;
                }
                if let Some(addon) = options.addon {
                    state.addons = Some(addon.to_string());
                    touched = true;
                }
                if touched {
                    let mut payload = Vec::with_capacity(state.encoded_len());
                    state.encode(&mut payload)?;
                    report.signon_states_patched += 1;
                    changed = true;
                    keep.push(NetMessage {
                        msg_type: message.msg_type,
                        payload,
                    });
                } else {
                    keep.push(message);
                }
            }
            _ => keep.push(message),
        }
    }

    *messages = keep;
    Ok(changed)
}

pub fn retarget(
    demo: &[u8],
    index: &DemoIndex,
    options: &RetargetOptions,
    destination: &Path,
) -> Result<RetargetReport> {
    let header_frame = index
        .frames
        .iter()
        .find(|f| f.cmd == CMD_FILE_HEADER)
        .ok_or_else(|| anyhow::anyhow!("demo has no DEM_FileHeader"))?;
    let raw_header = if header_frame.compressed {
        snap::raw::Decoder::new().decompress_vec(header_frame.payload(demo))?
    } else { header_frame.payload(demo).to_vec() };
    let old_map = CDemoFileHeader::decode(raw_header.as_slice())
        .context("decoding DEM_FileHeader")?
        .map_name()
        .to_string();
    if old_map.is_empty() {
        bail!("source demo does not name a map");
    }
    if old_map == options.map {
        bail!("source demo is already on {}", options.map);
    }

    let mut report = RetargetReport {
        output_bytes: 0,
        file_header_patched: false,
        server_info_patched: 0,
        spawn_groups_patched: 0,
        spawn_groups_dropped: 0,
        signon_states_patched: 0,
        packets_rewritten: 0,
    };

    let file =
        File::create(destination).with_context(|| format!("creating {}", destination.display()))?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let mut offset: u64 = 0;
    let mut file_info_offset = 0u64;
    let mut spawn_groups_offset = 0u64;

    out.write_all(MAGIC)?;
    out.write_all(&0u32.to_le_bytes())?;
    out.write_all(&0u32.to_le_bytes())?;
    offset += HEADER_LEN as u64;

    for frame in &index.frames {
        if frame.cmd == CMD_FILE_INFO {
            file_info_offset = offset;
        }
        if frame.cmd == CMD_SPAWN_GROUPS && index.stop.map(|s| frame.index > s).unwrap_or(false) {
            spawn_groups_offset = offset;
        }

        // FileHeader: metadata only, but keep it consistent with what we load.
        if frame.cmd == CMD_FILE_HEADER {
            let mut header = CDemoFileHeader::decode(raw_header.as_slice())?;
            header.map_name = Some(options.map.to_string());
            if let Some(addon) = options.addon {
                header.addons = Some(addon.to_string());
            }
            let mut payload = Vec::with_capacity(header.encoded_len());
            header.encode(&mut payload)?;
            let head = frame_header(frame.cmd, false, frame.tick_raw, payload.len() as u32);
            out.write_all(&head)?;
            out.write_all(&payload)?;
            offset += head.len() as u64 + payload.len() as u64;
            report.file_header_patched = true;
            continue;
        }

        let touchable = frame.cmd == CMD_SIGNON_PACKET || frame.cmd == CMD_PACKET;
        if touchable {
            let raw = frame.payload(demo);
            let decoded = if frame.compressed {
                snap::raw::Decoder::new().decompress_vec(raw).ok()
            } else {
                Some(raw.to_vec())
            };
            if let Some(decoded) = decoded {
                if let Ok(packet) = CDemoPacket::decode(&decoded[..]) {
                    if let Some(data) = packet.data.clone() {
                        let mut messages = bitwriter::read_messages(&data)?;
                        let changed =
                            patch_messages(&mut messages, options, &old_map, &mut report)?;
                        if changed {
                            let rebuilt = bitwriter::write_messages(&messages);
                            let new_packet = CDemoPacket {
                                data: Some(rebuilt.into()),
                            };
                            let mut payload = Vec::with_capacity(new_packet.encoded_len());
                            new_packet.encode(&mut payload)?;
                            // Written uncompressed: simpler, and the size change is tiny.
                            let head = frame_header(
                                frame.cmd,
                                false,
                                frame.tick_raw,
                                payload.len() as u32,
                            );
                            out.write_all(&head)?;
                            out.write_all(&payload)?;
                            offset += head.len() as u64 + payload.len() as u64;
                            report.packets_rewritten += 1;
                            continue;
                        }
                    }
                }
            }
        }

        out.write_all(frame.bytes(demo))?;
        offset += frame.total_len();
    }

    if offset > u32::MAX as u64 {
        bail!("output exceeds the u32 range the short-header pointers use");
    }

    let mut file = out.into_inner().context("flushing output")?;
    file.seek(SeekFrom::Start(8))?;
    file.write_all(&(file_info_offset as u32).to_le_bytes())?;
    file.write_all(&(spawn_groups_offset as u32).to_le_bytes())?;
    file.flush()?;
    file.sync_all()?;

    report.output_bytes = offset;
    Ok(report)
}

#[cfg(test)]
mod release_retarget_tests {
    use super::*;

    fn push(demo: &mut Vec<u8>, cmd: u32, tick: u32, payload: Vec<u8>, compressed: bool) -> usize {
        let offset = demo.len();
        let payload = if compressed { snap::raw::Encoder::new().compress_vec(&payload).unwrap() } else { payload };
        demo.extend(frame_header(cmd, compressed, tick, payload.len() as u32));
        demo.extend(payload);
        offset
    }
    fn fixture(compressed: bool) -> Vec<u8> {
        let mut demo = MAGIC.to_vec(); demo.extend([0; 8]);
        let header = CDemoFileHeader { map_name: Some("de_nuke".into()), ..Default::default() };
        push(&mut demo, CMD_FILE_HEADER, u32::MAX, header.encode_to_vec(), compressed);
        let messages = vec![
            NetMessage { msg_type: SVC_SERVER_INFO, payload: CsvcMsgServerInfo { map_name: Some("de_nuke".into()), game_session_manifest: Some(vec![1, 2].into()), ..Default::default() }.encode_to_vec() },
            NetMessage { msg_type: NET_SIGNON_STATE, payload: CnetMsgSignonState { map_name: Some("de_nuke".into()), ..Default::default() }.encode_to_vec() },
            NetMessage { msg_type: NET_SPAWN_GROUP_LOAD, payload: CnetMsgSpawnGroupLoad { worldname: Some("de_nuke".into()), entitylumpname: Some("maps/de_nuke/entities".into()), ..Default::default() }.encode_to_vec() },
            NetMessage { msg_type: NET_SPAWN_GROUP_LOAD, payload: CnetMsgSpawnGroupLoad { worldname: Some("maps/prefabs/de_nuke/skybox".into()), ..Default::default() }.encode_to_vec() },
        ];
        let packet = CDemoPacket { data: Some(bitwriter::write_messages(&messages).into()) };
        push(&mut demo, CMD_SIGNON_PACKET, u32::MAX, packet.encode_to_vec(), compressed);
        let full = csgoproto::CDemoFullPacket { packet: Some(CDemoPacket::default()), ..Default::default() };
        push(&mut demo, CMD_FULL_PACKET, 1, full.encode_to_vec(), false);
        push(&mut demo, CMD_STOP, 2, vec![], false);
        let spawn = push(&mut demo, CMD_SPAWN_GROUPS, 2, vec![], false);
        let info = push(&mut demo, CMD_FILE_INFO, 2, csgoproto::CDemoFileInfo::default().encode_to_vec(), false);
        demo[8..12].copy_from_slice(&(info as u32).to_le_bytes());
        demo[12..16].copy_from_slice(&(spawn as u32).to_le_bytes());
        demo
    }
    #[test]
    fn compressed_workshop_retarget_changes_world_and_metadata_and_keeps_resources() {
        let demo = fixture(true);
        let index = DemoIndex::build(&demo).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("workshop.dem");
        let map = "@workshop/12345/nuke_night";
        let report = retarget(&demo, &index, &RetargetOptions { map, addon: Some("12345"), sky: Some("maps/prefabs/nuke_night/sky"), keep_manifests: true }, &output).unwrap();
        assert!(report.file_header_patched);
        assert_eq!(report.spawn_groups_patched, 2);
        assert_eq!(report.signon_states_patched, 1);
        let result = std::fs::read(output).unwrap();
        let idx = DemoIndex::build(&result).unwrap();
        assert!(idx.structural_report().iter().all(|c| c.passed));
        let header = CDemoFileHeader::decode(idx.frames[0].payload(&result)).unwrap();
        assert_eq!(header.map_name(), map); assert_eq!(header.addons(), "12345");
        let packet = CDemoPacket::decode(idx.frames[1].payload(&result)).unwrap();
        let msgs = bitwriter::read_messages(packet.data()).unwrap();
        let info = CsvcMsgServerInfo::decode(msgs[0].payload.as_slice()).unwrap();
        assert_eq!(info.map_name(), map); assert_eq!(info.addon_name(), "12345");
        assert_eq!(info.game_session_manifest(), [1, 2]);
        let load = CnetMsgSpawnGroupLoad::decode(msgs[2].payload.as_slice()).unwrap();
        assert_eq!(load.worldname(), map); assert_eq!(load.entitylumpname(), "maps/@workshop/12345/nuke_night/entities");
        assert_eq!(idx.frames.iter().map(|f| f.tick_raw).collect::<Vec<_>>(), index.frames.iter().map(|f| f.tick_raw).collect::<Vec<_>>());
    }
    #[test]
    fn invalid_retarget_does_not_replace_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.dem");
        let output = directory.path().join("output.dem");
        std::fs::write(&input, fixture(false)).unwrap();
        std::fs::write(&output, b"existing").unwrap();
        assert!(crate::cmd_retarget(&input, "de_nuke", None, None, true, &output, true).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }
    #[test]
    fn retarget_rejects_in_place_output_even_with_force() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.dem");
        let original = fixture(false);
        std::fs::write(&input, &original).unwrap();
        assert!(crate::cmd_retarget(&input, "de_mirage", None, None, true, &input, true).is_err());
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }
}
