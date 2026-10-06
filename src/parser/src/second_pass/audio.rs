//! Lossless, typed capture of demo-carried audio transport messages.
//!
//! `packed_params` and `packed_fields` deliberately remain opaque bytes. Their
//! layout is not stable enough to infer here, and consumers need the original
//! payload rather than a speculative projection.

use crate::first_pass::read_bits::DemoParserError;
use crate::second_pass::parser_settings::SecondPassParser;
use csgoproto::{
    CMsgSosSetLibraryStackFields, CMsgSosSetSoundEventParams, CMsgSosStartSoundEvent, CMsgSosStopSoundEvent, CMsgSosStopSoundEventHash, CsvcMsgSounds,
    CsvcMsgStopSound,
};
use prost::Message;

pub const SVC_SOUNDS: u32 = 49;
pub const SVC_STOP_SOUND: u32 = 59;
pub const GE_SOS_START_SOUND_EVENT: u32 = 208;
pub const GE_SOS_STOP_SOUND_EVENT: u32 = 209;
pub const GE_SOS_SET_SOUND_EVENT_PARAMS: u32 = 210;
pub const GE_SOS_SET_LIBRARY_STACK_FIELDS: u32 = 211;
pub const GE_SOS_STOP_SOUND_EVENT_HASH: u32 = 212;

/// A stable source-order key. `demo_frame_offset` names the start of the
/// enclosing DEM frame in the original byte stream; it does not depend on how
/// a second pass is chunked. `network_message_index` is the zero-based index
/// inside that frame's network packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioEventOrder {
    pub demo_frame_offset: u64,
    pub network_message_index: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioEvent {
    pub tick: i32,
    pub order: AudioEventOrder,
    pub payload: AudioEventPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioEventPayload {
    /// One envelope retains its `reliable_sound` presence and every repeated
    /// `SounddataT` entry in original vector order.
    SvcSounds {
        reliable_sound: Option<bool>,
        sounds: Vec<SvcSoundEntry>,
    },
    SvcStopSound {
        guid: Option<u32>,
    },
    SosStartSoundEvent {
        soundevent_guid: Option<i32>,
        soundevent_hash: Option<u32>,
        source_entity_index: Option<i32>,
        seed: Option<i32>,
        packed_params: Option<Vec<u8>>,
        start_time: Option<f32>,
    },
    SosStopSoundEvent {
        soundevent_guid: Option<i32>,
    },
    SosStopSoundEventHash {
        soundevent_hash: Option<u32>,
        source_entity_index: Option<i32>,
    },
    SosSetSoundEventParams {
        soundevent_guid: Option<i32>,
        packed_params: Option<Vec<u8>>,
    },
    SosSetLibraryStackFields {
        stack_hash: Option<u32>,
        packed_fields: Option<Vec<u8>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvcSoundEntry {
    pub origin_x: Option<i32>,
    pub origin_y: Option<i32>,
    pub origin_z: Option<i32>,
    pub volume: Option<u32>,
    pub delay_value: Option<f32>,
    pub sequence_number: Option<i32>,
    pub entity_index: Option<i32>,
    pub channel: Option<i32>,
    pub pitch: Option<i32>,
    pub flags: Option<i32>,
    pub sound_num: Option<u32>,
    pub sound_num_handle: Option<u32>,
    pub speaker_entity: Option<i32>,
    pub random_seed: Option<i32>,
    pub sound_level: Option<i32>,
    pub is_sentence: Option<bool>,
    pub is_ambient: Option<bool>,
    pub guid: Option<u32>,
    pub sound_resource_id: Option<u64>,
}

impl AudioEvent {
    pub fn sort_key(&self) -> AudioEventOrder {
        self.order
    }
}

impl<'a> SecondPassParser<'a> {
    /// Decode one recognized audio transport payload at the currently tracked
    /// source position. The caller gates this to the authoritative event lane.
    pub(crate) fn capture_audio_message(&mut self, message_type: u32, bytes: &[u8]) -> Result<(), DemoParserError> {
        let payload = match message_type {
            SVC_SOUNDS => {
                let msg = CsvcMsgSounds::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                AudioEventPayload::SvcSounds {
                    reliable_sound: msg.reliable_sound,
                    sounds: msg
                        .sounds
                        .into_iter()
                        .map(|sound| SvcSoundEntry {
                            origin_x: sound.origin_x,
                            origin_y: sound.origin_y,
                            origin_z: sound.origin_z,
                            volume: sound.volume,
                            delay_value: sound.delay_value,
                            sequence_number: sound.sequence_number,
                            entity_index: sound.entity_index,
                            channel: sound.channel,
                            pitch: sound.pitch,
                            flags: sound.flags,
                            sound_num: sound.sound_num,
                            sound_num_handle: sound.sound_num_handle,
                            speaker_entity: sound.speaker_entity,
                            random_seed: sound.random_seed,
                            sound_level: sound.sound_level,
                            is_sentence: sound.is_sentence,
                            is_ambient: sound.is_ambient,
                            guid: sound.guid,
                            sound_resource_id: sound.sound_resource_id,
                        })
                        .collect(),
                }
            }
            SVC_STOP_SOUND => {
                let msg = CsvcMsgStopSound::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                AudioEventPayload::SvcStopSound { guid: msg.guid }
            }
            GE_SOS_START_SOUND_EVENT => {
                let msg = CMsgSosStartSoundEvent::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                AudioEventPayload::SosStartSoundEvent {
                    soundevent_guid: msg.soundevent_guid,
                    soundevent_hash: msg.soundevent_hash,
                    source_entity_index: msg.source_entity_index,
                    seed: msg.seed,
                    packed_params: msg.packed_params.map(|bytes| bytes.to_vec()),
                    start_time: msg.start_time,
                }
            }
            GE_SOS_STOP_SOUND_EVENT => {
                let msg = CMsgSosStopSoundEvent::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                AudioEventPayload::SosStopSoundEvent {
                    soundevent_guid: msg.soundevent_guid,
                }
            }
            GE_SOS_STOP_SOUND_EVENT_HASH => {
                let msg = CMsgSosStopSoundEventHash::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                AudioEventPayload::SosStopSoundEventHash {
                    soundevent_hash: msg.soundevent_hash,
                    source_entity_index: msg.source_entity_index,
                }
            }
            GE_SOS_SET_SOUND_EVENT_PARAMS => {
                let msg = CMsgSosSetSoundEventParams::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                AudioEventPayload::SosSetSoundEventParams {
                    soundevent_guid: msg.soundevent_guid,
                    packed_params: msg.packed_params.map(|bytes| bytes.to_vec()),
                }
            }
            GE_SOS_SET_LIBRARY_STACK_FIELDS => {
                let msg = CMsgSosSetLibraryStackFields::decode(bytes).map_err(|_| DemoParserError::MalformedMessage)?;
                AudioEventPayload::SosSetLibraryStackFields {
                    stack_hash: msg.stack_hash,
                    packed_fields: msg.packed_fields.map(|bytes| bytes.to_vec()),
                }
            }
            _ => return Ok(()),
        };
        self.audio_events.push(AudioEvent {
            tick: self.tick,
            order: AudioEventOrder {
                demo_frame_offset: self.current_demo_frame_offset,
                network_message_index: self.current_network_message_index,
            },
            payload,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csgoproto::message_type::NetMessageType;

    #[test]
    fn source_order_does_not_depend_on_tick_or_chunk_counter() {
        let early = AudioEventOrder {
            demo_frame_offset: 32,
            network_message_index: 9,
        };
        let late = AudioEventOrder {
            demo_frame_offset: 40,
            network_message_index: 0,
        };
        assert!(early < late);
    }

    #[test]
    fn audited_protocol_ids_match_the_generated_message_type_table() {
        assert_eq!(NetMessageType::from(SVC_SOUNDS as i32), NetMessageType::svc_Sounds);
        assert_eq!(NetMessageType::from(SVC_STOP_SOUND as i32), NetMessageType::svc_StopSound);
        assert_eq!(NetMessageType::from(GE_SOS_START_SOUND_EVENT as i32), NetMessageType::GE_SosStartSoundEvent);
        assert_eq!(NetMessageType::from(GE_SOS_STOP_SOUND_EVENT as i32), NetMessageType::GE_SosStopSoundEvent);
        assert_eq!(
            NetMessageType::from(GE_SOS_SET_SOUND_EVENT_PARAMS as i32),
            NetMessageType::GE_SosSetSoundEventParams
        );
        assert_eq!(
            NetMessageType::from(GE_SOS_SET_LIBRARY_STACK_FIELDS as i32),
            NetMessageType::GE_SosSetLibraryStackFields
        );
        assert_eq!(
            NetMessageType::from(GE_SOS_STOP_SOUND_EVENT_HASH as i32),
            NetMessageType::GE_SosStopSoundEventHash
        );
    }
}
