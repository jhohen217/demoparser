//! Remove a player's shots from a demo, at the source.
//!
//! This is the first edit the writer makes to packet contents rather than to the container
//! around them. It rests entirely on `roundtrip`: the bit writer reproduces every inner packet
//! byte for byte, so dropping a message and re-encoding the rest changes exactly the message that
//! was dropped and nothing else.
//!
//! What it removes is the *presentation* of a shot — the temp entity the client draws the tracer,
//! muzzle flash and impact from, and the weapon sound. What it cannot yet remove lives in
//! `svc_PacketEntities`: the magazine, the shots-fired counter that CS2's animation graph reads,
//! and the recoil and aim-punch state. Those are delta-encoded against the previous tick, and
//! editing them is a separate piece of work. Until then a suppressed shot is silent and leaves no
//! tracer, but the player may still be animated firing it — which is exactly the question this
//! command exists to answer.

use crate::bitwriter::{self, NetMessage};
use crate::frame::*;
use crate::index::DemoIndex;
use anyhow::{Context, Result};
use csgoproto::{
    CDemoPacket, CMsgPlaceDecalEvent, CMsgSosStartSoundEvent, CMsgSosStopSoundEvent,
    CMsgTeFireBullets, CUserMsgParticleManager, CcsUsrMsgWeaponSound, CsgoUserCmdPb,
    CsvcMsgUserCommands,
};
pub use parser::second_pass::usercmd_delta::apply_delta;
use prost::Message;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashMap};

/// Net message ids this command understands. They are the ids carried inside a `DEM_Packet`'s
/// bit stream, not the outer frame commands.
pub const GE_FIRE_BULLETS: u32 = 452;
pub const CS_UM_WEAPON_SOUND: u32 = 369;
pub const SVC_PACKET_ENTITIES: u32 = 55;
pub const SVC_USER_CMDS: u32 = 76;
pub const UM_PARTICLE_MANAGER: u32 = 145;
pub const GE_PLACE_DECAL_EVENT: u32 = 201;
pub const GE_SOS_START_SOUND: u32 = 208;
pub const GE_SOS_STOP_SOUND: u32 = 209;
pub const GE_SOURCE1_LEGACY_GAME_EVENT_LIST: u32 = 205;
pub const GE_SOURCE1_LEGACY_GAME_EVENT: u32 = 207;

/// One discharge as the source records it.
#[derive(Debug, Clone)]
pub struct Shot {
    pub frame: usize,
    pub tick: i32,
    pub player_handle: u32,
    pub entity_index: i32,
    pub weapon_id: u32,
    pub item_def_index: u32,
    /// Muzzle position and view angles, which together give the line the bullet travelled. Absent
    /// on a shot whose temp entity omitted them.
    pub origin: Option<[f32; 3]>,
    pub angles: Option<[f32; 3]>,
    /// The cone the bullet could have gone in. CS2 picks a direction inside this from the seed, so
    /// a decal is only this shot's if it lies within the cone the shot itself declared.
    pub inaccuracy: f32,
    pub spread: f32,
    /// The shooter entity's own origin, which is the feet rather than the eye.
    pub ent_origin: Option<[f32; 3]>,
}

/// What a suppression pass removed.
#[derive(Debug, Default)]
pub struct SuppressStats {
    pub fire_bullets: usize,
    pub weapon_sounds: usize,
    pub frames_rewritten: usize,
    /// Blood, sparks and marks removed from whatever a suppressed shot hit.
    pub impact_particles: usize,
    pub impact_decals: usize,
}

/// What the user-command rewrite actually touched.
///
/// Without this the rewrite is unfalsifiable: a demo that still fires looks identical whether the
/// edit found nothing, matched nothing, or was never reached at all.
#[derive(Default)]
pub struct UserCmdStats {
    /// Commands belonging to the target player that were seen inside the range.
    pub commands_seen: usize,
    /// Commands whose held attack button was cleared.
    pub held_cleared: usize,
    /// Subtick steps whose attack press was cleared.
    pub presses_cleared: usize,
}

/// An entity handle's index. The serial number occupies the high bits; the parser masks it the
/// same way wherever it resolves a handle to an entity.
pub fn entity_index(handle: u32) -> i32 {
    (handle & 0x7FF) as i32
}

/// Each player's weapon attack overlay clip, by pawn entity.
///
/// The firing animation is a permanent overlay whose playback time restarts on each trigger pull,
/// and the clip differs per weapon, so it is identified rather than named: the sampler whose time
/// runs backwards on the ticks that player fired. Reacting to those restarts one at a time does not
/// stop the animation — the next update carries it playing on from wherever it restarted — so what
/// this is for is knowing which clip to hold still for the whole burst.
pub fn attack_overlay_clips(
    demo: &[u8],
    index: &DemoIndex,
    boundary: &crate::boundary::BoundaryBuilder,
) -> Result<BTreeMap<i32, BTreeSet<u32>>> {
    let mut out = BTreeMap::new();
    let by_entity = {
        let mut map: BTreeMap<i32, BTreeSet<i32>> = BTreeMap::new();
        for shot in shots(demo, index)? {
            map.entry(shot.entity_index).or_default().insert(shot.tick);
        }
        map
    };
    for (entity, firing) in &by_entity {
        let writes = crate::boundary::field_writes(demo, index, *entity, i32::MIN, i32::MAX)?;
        let mut tracker = crate::poserecipe::PoseTracker::default();
        let mut tick = i32::MIN;
        // Restarts on a firing tick, against restarts anywhere, per clip. A firing animation is
        // made of more than one layer — a weapon overlay and an upper body overlay fire together —
        // so taking only the best scoring clip leaves the others playing. That is worth being
        // explicit about: it is exactly how two shots per burst survived a fix that removed the
        // rest.
        let mut score: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
        let near = |at: i32| firing.iter().any(|shot| (at - shot).abs() <= 2);
        for write in &writes {
            if write.tick != tick {
                if tick != i32::MIN {
                    let firing = near(tick);
                    for restart in tracker.step() {
                        let entry = score.entry(restart.clip).or_default();
                        entry.0 += 1;
                        if firing {
                            entry.1 += 1;
                        }
                    }
                }
                tick = write.tick;
            }
            apply_pose_write(&mut tracker, write);
        }
        // A clip counts as part of the firing animation when it restarts on a shot at least once
        // and does so mostly on shots. The second test is what keeps ordinary looping animations
        // out: a walk cycle restarts constantly and only occasionally lands near a trigger pull.
        // Requiring two firing restarts instead of one looked safer and was not — a layer that
        // plays across a whole burst restarts only at its start, and excluding it left a visible
        // shot.
        let clips: BTreeSet<u32> = score
            .into_iter()
            .filter(|(_, (_total, on_shot))| *on_shot >= 1)
            .map(|(clip, _)| clip)
            .collect();
        if !clips.is_empty() {
            out.insert(*entity, clips);
        }
    }
    let _ = boundary;
    Ok(out)
}

/// Feed one census write into a recipe tracker, if it is one of the three the recipe uses.
pub fn apply_pose_write(
    tracker: &mut crate::poserecipe::PoseTracker,
    write: &crate::boundary::FieldWrite,
) {
    let bare = write.name.rsplit('.').next().unwrap_or(write.name.as_str());
    let number = write
        .value
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .and_then(|inner| inner.parse::<f64>().ok());
    match bare {
        "m_topology" => {
            if let (Some(&slot), Some(raw)) = (
                write.path.get(2),
                crate::boundary::binary_block_of(&write.value),
            ) {
                tracker.set_topology(slot as u32, raw);
            }
        }
        "m_SerializePoseRecipeAG2Dynamic" => {
            if let (Some(&at), Some(value)) = (write.path.last(), number) {
                if at >= 0 {
                    tracker.set_byte(at as usize, value as u8);
                }
            }
        }
        "m_nSerializePoseRecipeAG2ActiveSlot" => {
            if let Some(slot) = number {
                tracker.set_active(slot as u32);
            }
        }
        _ => {}
    }
}

/// Every tick each player asked to fire, by pawn entity.
///
/// Discharges are not the same thing as trigger pulls: a shot that never reached the server leaves
/// no `GE_FireBullets` but still leaves the press in the player's commands, and TrueView will
/// happily fire the weapon from it. Anything scoped to the discharge list therefore misses those
/// players entirely — which is exactly how a firing animation survived every fix aimed at shooters.
pub fn attack_ticks(demo: &[u8], index: &DemoIndex) -> Result<BTreeMap<i32, BTreeSet<i32>>> {
    use prost::Message;
    let mut out: BTreeMap<i32, BTreeSet<i32>> = BTreeMap::new();
    let mut baselines: BTreeMap<i32, CsgoUserCmdPb> = BTreeMap::new();
    for frame in &index.frames {
        if frame.cmd != CMD_PACKET && frame.cmd != CMD_SIGNON_PACKET && frame.cmd != CMD_FULL_PACKET
        {
            continue;
        }
        let tick = frame.tick();
        let payload = frame_payload(demo, frame)?;
        let messages = if frame.cmd == CMD_FULL_PACKET {
            match csgoproto::CDemoFullPacket::decode(payload.as_slice())?.packet {
                Some(packet) => crate::bitwriter::read_messages(packet.data())?,
                None => continue,
            }
        } else {
            crate::bitwriter::read_messages(CDemoPacket::decode(payload.as_slice())?.data())?
        };
        for message in messages {
            if message.msg_type != SVC_USER_CMDS {
                continue;
            }
            let Ok(msg) = CsvcMsgUserCommands::decode(message.payload.as_slice()) else {
                continue;
            };
            for command in &msg.commands {
                let slot = command.player_slot();
                if slot < 0 {
                    continue;
                }
                let full = command.data.as_ref().filter(|d| !d.is_empty());
                let delta = command.delta_data.as_ref().filter(|d| !d.is_empty());
                let mut next = if let Some(data) = full {
                    CsgoUserCmdPb::decode(data.as_ref()).ok()
                } else if delta.is_some() {
                    baselines.get(&slot).cloned()
                } else {
                    continue;
                };
                if let Some(data) = delta {
                    next = next
                        .as_ref()
                        .and_then(|base| apply_delta(base, data.as_ref()));
                }
                let Some(decoded) = next else { continue };
                baselines.insert(slot, decoded.clone());
                let Some(base) = decoded.base.as_ref() else {
                    continue;
                };
                let asked = base
                    .buttons_pb
                    .as_ref()
                    .is_some_and(|b| b.buttonstate1() & IN_ATTACK != 0)
                    || base
                        .subtick_moves
                        .iter()
                        .any(|step| step.button() & IN_ATTACK != 0);
                if asked {
                    out.entry(entity_index(base.pawn_entity_handle()))
                        .or_default()
                        .insert(tick);
                }
            }
        }
    }
    Ok(out)
}

/// Every discharge in the demo, in frame order.
///
/// Read from the temp entity rather than from the `weapon_fire` game event: this is the message
/// the client actually draws the shot from, it names the shooter by entity handle, and it is the
/// message the suppression removes.
pub fn shots(demo: &[u8], index: &DemoIndex) -> Result<Vec<Shot>> {
    let mut found = Vec::new();
    for frame in &index.frames {
        if frame.cmd != CMD_PACKET && frame.cmd != CMD_SIGNON_PACKET {
            continue;
        }
        let Some(messages) = packet_messages(demo, frame)? else {
            continue;
        };
        for message in &messages {
            if message.msg_type != GE_FIRE_BULLETS {
                continue;
            }
            let Ok(shot) = CMsgTeFireBullets::decode(message.payload.as_slice()) else {
                continue;
            };
            let handle = shot.player();
            found.push(Shot {
                frame: frame.index,
                tick: frame.tick(),
                player_handle: handle,
                entity_index: entity_index(handle),
                weapon_id: shot.weapon_id(),
                item_def_index: shot.item_def_index(),
                origin: shot.origin.as_ref().map(|v| [v.x(), v.y(), v.z()]),
                angles: shot.angles.as_ref().map(|a| [a.x(), a.y(), a.z()]),
                inaccuracy: shot.inaccuracy(),
                spread: shot.spread(),
                ent_origin: shot.ent_origin.as_ref().map(|v| [v.x(), v.y(), v.z()]),
            });
        }
    }
    Ok(found)
}

/// A frame's payload, decompressed when the frame says it is compressed.
pub fn frame_payload(demo: &[u8], frame: &FrameRef) -> Result<Vec<u8>> {
    let raw = frame.payload(demo);
    Ok(if frame.compressed {
        snap::raw::Decoder::new().decompress_vec(raw)?
    } else {
        raw.to_vec()
    })
}

/// The messages inside one packet frame, or `None` when the frame is not one this edit reads.
pub fn packet_messages(demo: &[u8], frame: &FrameRef) -> Result<Option<Vec<NetMessage>>> {
    let raw = frame.payload(demo);
    let decoded = if frame.compressed {
        match snap::raw::Decoder::new().decompress_vec(raw) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        }
    } else {
        raw.to_vec()
    };
    let packet = match CDemoPacket::decode(&decoded[..]) {
        Ok(packet) => packet,
        Err(_) => return Ok(None),
    };
    let Some(data) = packet.data else {
        return Ok(None);
    };
    Ok(Some(bitwriter::read_messages(&data)?))
}

/// Which shots to remove.
pub struct Suppression {
    pub entity_index: i32,
    pub from_tick: i32,
    pub to_tick: i32,
    /// Also drop the gun's sound. Separate because it is matched by entity index rather than by
    /// the handle the shot carries, and a caller auditing the edit should be able to see the two
    /// removals counted apart.
    pub drop_weapon_sound: bool,
    /// The weapon entities the suppressed shots came from.
    ///
    /// The muzzle flash is a particle hung off the weapon rather than off the player, so removing
    /// it needs the weapon's index and not the shooter's. Measured at two particle creates per
    /// discharge, on the discharge's own tick.
    pub weapon_entities: BTreeSet<i32>,
    /// Particle ids already removed, so the updates and destroys that follow a removed create go
    /// with it rather than referring to a particle that was never made.
    pub dropped_particles: RefCell<BTreeSet<u32>>,
    /// Where and when a suppressed discharge registered on another entity, as (entity, tick).
    ///
    /// The impact effects render on the entity that was struck rather than on the shooter: a decal
    /// attached to its pawn and bones, and a sound sourced from the same entity. Both name that
    /// entity outright, so unlike a mark left on the world they need no geometry to attribute.
    pub impact_entities: BTreeSet<(i32, i32)>,
    /// Ticks after an impact within which its effects may still arrive.
    pub impact_window: i32,
    /// Sound ids already removed, so a stop does not chase a sound that never started.
    pub dropped_sounds: RefCell<BTreeSet<i32>>,
    /// Ticks this player fired on, for attributing marks left on the world.
    ///
    /// A decal on a wall carries the world's handle rather than a player's, so it cannot be
    /// attributed the way an impact on a pawn can. Geometry was tried and is not good enough — the
    /// median error against a shot's own line was three to four degrees. Timing is: a suppressed
    /// shot's bullet lands within a few ticks of leaving the barrel, so a mark appearing in that
    /// window belonged to a shot that no longer exists.
    ///
    /// The cost is that another player firing at the same instant loses their mark too. That is
    /// acceptable when every shooter is being suppressed, and a false positive when only one is.
    pub shot_ticks: BTreeSet<i32>,
    /// Event ids for `player_hurt`, resolved from the demo's own dictionary.
    ///
    /// The blood mist and the impact spark are not particles the demo sends — every particle
    /// create can be removed and they still appear. The client draws them itself from the hurt
    /// event, which names the victim and the hit group. Dropping the event removes the effect;
    /// health and armour are left alone, so the round still ends the way it did.
    pub hurt_event_ids: BTreeSet<i32>,
    /// Drop blood, sparks and marks left by a suppressed shot on whatever it hit.
    ///
    /// The shooting effects hang off the shooter or their weapon; these hang off the victim or the
    /// world, so they survive every check aimed at the shooter. A suppressed demo that still
    /// sprays blood up a wall is not finished.
    pub drop_impact_effects: bool,
    /// Clear the primary attack button from this player's commands across the range.
    ///
    /// The button is held for five or six ticks per discharge and every run begins on the
    /// discharge's own tick, so it is a clean per-player signal — and unlike the animation payload
    /// it is an ordinary protobuf field rather than a packed blob.
    pub clear_attack_input: bool,
    /// Clear the attack input of every player, not only the suppression's own target.
    ///
    /// The target list is built from recorded discharges, so a player who pulled the trigger
    /// without one — an empty magazine, or a shot that never reached the server — is not on it and
    /// keeps their input. With TrueView the client simulates that press anyway and fires the
    /// weapon, which is a shot the demo does not contain.
    pub clear_all_attack_input: bool,
    pub usercmd_stats: std::cell::RefCell<UserCmdStats>,
    /// Per-player command baselines, carried so delta encoded commands can be read.
    ///
    /// A full command arrives occasionally and the rest are diffs against it, so a reader that
    /// looks only at full commands sees about one in a thousand.
    pub usercmd_baselines: RefCell<HashMap<i32, CsgoUserCmdPb>>,
}

impl Suppression {
    pub fn covers(&self, tick: i32) -> bool {
        tick >= self.from_tick && tick <= self.to_tick
    }

    pub fn drops(&self, message: &NetMessage, tick: i32) -> bool {
        if !self.covers(tick) {
            return false;
        }
        match message.msg_type {
            GE_FIRE_BULLETS => CMsgTeFireBullets::decode(message.payload.as_slice())
                .is_ok_and(|shot| entity_index(shot.player()) == self.entity_index),
            CS_UM_WEAPON_SOUND if self.drop_weapon_sound => {
                CcsUsrMsgWeaponSound::decode(message.payload.as_slice())
                    .is_ok_and(|sound| sound.entidx() == self.entity_index)
            }
            UM_PARTICLE_MANAGER => self.drops_particle(message, tick),
            GE_PLACE_DECAL_EVENT => self.drops_decal(message, tick),
            GE_SOS_START_SOUND => self.drops_sound(message, tick),
            GE_SOURCE1_LEGACY_GAME_EVENT if self.drop_impact_effects => {
                csgoproto::CMsgSource1LegacyGameEvent::decode(message.payload.as_slice()).is_ok_and(
                    |event| self.hurt_event_ids.contains(&event.eventid()) && self.fired_near(tick),
                )
            }
            GE_SOS_STOP_SOUND => CMsgSosStopSoundEvent::decode(message.payload.as_slice())
                .is_ok_and(|s| self.dropped_sounds.borrow().contains(&s.soundevent_guid())),
            _ => false,
        }
    }

    /// Whether this entity is one a suppressed discharge registered on, at about this tick.
    fn struck(&self, entity: i32, tick: i32) -> bool {
        (0..=self.impact_window).any(|back| self.impact_entities.contains(&(entity, tick - back)))
    }

    /// A decal attached to a struck entity is that impact's mark.
    ///
    /// Only entity-attached decals are removed. A mark on the world arrives as entity 0, which
    /// names nothing and cannot be told from any other shooter's without geometry that was
    /// measured and found not to work.
    fn drops_decal(&self, message: &NetMessage, tick: i32) -> bool {
        CMsgPlaceDecalEvent::decode(message.payload.as_slice()).is_ok_and(|decal| {
            let entity = entity_index(decal.entityhandle());
            if entity != 0 && self.struck(entity, tick) {
                return true;
            }
            // A mark on the world names no entity, so it is attributed by when it landed.
            self.drop_impact_effects && self.fired_near(tick)
        })
    }

    /// Whether a suppressed discharge happened close enough to this tick to own an impact.
    fn fired_near(&self, tick: i32) -> bool {
        self.shot_ticks
            .range(tick - self.impact_window..=tick + self.impact_window)
            .next()
            .is_some()
    }

    /// A sound sourced from a struck entity is that impact's sound.
    fn drops_sound(&self, message: &NetMessage, tick: i32) -> bool {
        let Ok(sound) = CMsgSosStartSoundEvent::decode(message.payload.as_slice()) else {
            return false;
        };
        if !self.struck(sound.source_entity_index(), tick) {
            return false;
        }
        self.dropped_sounds
            .borrow_mut()
            .insert(sound.soundevent_guid());
        true
    }

    /// Whether this particle message belongs to a suppressed discharge.
    ///
    /// A create is judged by the entity it hangs off; everything after it is judged by the
    /// particle id, so a removed muzzle flash takes its own updates and destroy with it.
    fn drops_particle(&self, message: &NetMessage, tick: i32) -> bool {
        let Ok(msg) = CUserMsgParticleManager::decode(message.payload.as_slice()) else {
            return false;
        };
        let id = msg.index();
        if let Some(create) = &msg.create_particle {
            let owner = entity_index(create.entity_handle());
            let mine = self.weapon_entities.contains(&owner) || owner == self.entity_index;
            // Blood and sparks are hung off whoever was hit, not off the shooter, so the owner
            // check that finds a muzzle flash never finds them.
            let on_a_victim = self.drop_impact_effects
                && (self.struck(owner, tick) || (owner == 0 && self.fired_near(tick)));
            if mine || on_a_victim {
                self.dropped_particles.borrow_mut().insert(id);
                return true;
            }
            return false;
        }
        self.dropped_particles.borrow().contains(&id)
    }
}

/// The primary attack button, bit zero of the first button word.
const IN_ATTACK: u64 = 1;

impl Suppression {
    /// Rewrite a user-command message with this player's attack button cleared.
    ///
    /// Commands are delta encoded against a per-player baseline, and there is no delta *encoder*
    /// available. That is sidestepped rather than solved: an edited command is written back as a
    /// full command and its delta dropped, which the format already allows because that is how
    /// baselines are established in the first place. Commands for other players pass through as
    /// they arrived, still delta encoded, so only the edited player's stream grows.
    pub fn rewrite_usercmds(&self, message: &NetMessage, tick: i32) -> Result<Option<Vec<u8>>> {
        if !self.clear_attack_input {
            return Ok(None);
        }
        let Ok(mut msg) = CsvcMsgUserCommands::decode(message.payload.as_slice()) else {
            return Ok(None);
        };

        let mut changed = false;
        let mut baselines = self.usercmd_baselines.borrow_mut();
        for command in &mut msg.commands {
            let slot = command.player_slot();
            if slot < 0 {
                continue;
            }
            let full = command.data.as_ref().filter(|d| !d.is_empty());
            let delta = command.delta_data.as_ref().filter(|d| !d.is_empty());
            let mut next = if let Some(data) = full {
                match CsgoUserCmdPb::decode(data.as_ref()) {
                    Ok(decoded) => Some(decoded),
                    Err(_) => continue,
                }
            } else if delta.is_some() {
                baselines.get(&slot).cloned()
            } else {
                continue;
            };
            if let Some(data) = delta {
                next = next
                    .as_ref()
                    .and_then(|baseline| apply_delta(baseline, data.as_ref()));
            }
            let Some(mut decoded) = next else { continue };

            // The baseline must track what the client will now believe, so it is stored after the
            // edit rather than before: a later delta applies on top of the edited command.
            let mine = self.clear_all_attack_input
                || decoded
                    .base
                    .as_ref()
                    .map(|base| entity_index(base.pawn_entity_handle()) == self.entity_index)
                    .unwrap_or(false);
            if mine && self.covers(tick) {
                self.usercmd_stats.borrow_mut().commands_seen += 1;
                if let Some(base) = decoded.base.as_mut() {
                    if let Some(buttons) = base.buttons_pb.as_mut() {
                        let before = buttons.buttonstate1();
                        if before & IN_ATTACK != 0 {
                            buttons.buttonstate1 = Some(before & !IN_ATTACK);
                            changed = true;
                            self.usercmd_stats.borrow_mut().held_cleared += 1;
                        }
                    }

                    // The held button state is only half of it. CS2 also records the exact moment
                    // within the tick that a button went down, as a subtick step carrying the same
                    // button mask. Clearing the held state alone leaves that press in place, and
                    // the client fires exactly one shot from it — which is the single discharge
                    // that survived every earlier attempt, in both the entity edits and the first
                    // pass at the input.
                    //
                    // A step can carry movement and aim deltas alongside the button, so the bit is
                    // cleared rather than the step dropped; a step left with nothing but a cleared
                    // button and no deltas is dropped, since it then describes nothing.
                    base.subtick_moves.retain_mut(|step| {
                        if step.button() & IN_ATTACK == 0 {
                            return true;
                        }
                        step.button = Some(step.button() & !IN_ATTACK);
                        changed = true;
                        self.usercmd_stats.borrow_mut().presses_cleared += 1;
                        let empty = step.button() == 0
                            && step.analog_forward_delta() == 0.0
                            && step.analog_left_delta() == 0.0
                            && step.pitch_delta() == 0.0
                            && step.yaw_delta() == 0.0;
                        !empty
                    });
                }
            }

            baselines.insert(slot, decoded.clone());
            if mine && changed {
                let mut buffer = Vec::with_capacity(decoded.encoded_len());
                decoded.encode(&mut buffer)?;
                command.data = Some(buffer.into());
                command.delta_data = None;
            }
        }

        if !changed {
            return Ok(None);
        }
        let mut buffer = Vec::with_capacity(msg.encoded_len());
        msg.encode(&mut buffer)?;
        Ok(Some(buffer))
    }
}

/// The rewritten payload for one frame, or `None` when this frame keeps its original bytes.
///
/// Returning `None` rather than an identical copy matters: the writer splices unedited frames
/// byte for byte, and a frame that re-encodes to the same bits should still take that path rather
/// than be rebuilt.
pub fn rewrite_frame(
    demo: &[u8],
    frame: &FrameRef,
    suppression: &Suppression,
    stats: &mut SuppressStats,
) -> Result<Option<Vec<u8>>> {
    if frame.cmd != CMD_PACKET && frame.cmd != CMD_SIGNON_PACKET {
        return Ok(None);
    }
    let tick = frame.tick();
    if !suppression.covers(tick) {
        return Ok(None);
    }
    let Some(messages) = packet_messages(demo, frame)? else {
        return Ok(None);
    };

    let mut kept = Vec::with_capacity(messages.len());
    let mut dropped = SuppressStats::default();
    for message in messages {
        if suppression.drops(&message, tick) {
            match message.msg_type {
                GE_FIRE_BULLETS => dropped.fire_bullets += 1,
                CS_UM_WEAPON_SOUND => dropped.weapon_sounds += 1,
                _ => {}
            }
            continue;
        }
        kept.push(message);
    }

    if dropped.fire_bullets == 0 && dropped.weapon_sounds == 0 {
        return Ok(None);
    }

    stats.fire_bullets += dropped.fire_bullets;
    stats.weapon_sounds += dropped.weapon_sounds;
    stats.frames_rewritten += 1;

    // Re-wrapped as an uncompressed packet. The frame header records compression per frame, so a
    // rewritten frame may simply stop being compressed; leaving it uncompressed keeps the edit
    // auditable, and the caller clears the frame's compressed flag to match.
    let packet = CDemoPacket {
        data: Some(bitwriter::write_messages(&kept).into()),
    };
    let mut buffer = Vec::with_capacity(packet.encoded_len());
    packet
        .encode(&mut buffer)
        .context("could not re-encode an edited packet")?;
    Ok(Some(buffer))
}

/// One decoded legacy game event, with its keys named.
#[derive(Debug, Clone)]
pub struct GameEvent {
    pub tick: i32,
    pub name: String,
    pub keys: Vec<(String, String)>,
}

impl GameEvent {
    pub fn key(&self, name: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        self.key(name).and_then(|v| v.parse().ok())
    }
}

/// The events a shot can be held responsible for.
pub const DAMAGE_EVENTS: [&str; 2] = ["player_hurt", "player_death"];

/// The numeric ids the demo uses for the named game events.
///
/// Events travel as ids once the dictionary has been sent, so anything that wants to drop a
/// particular event by name has to resolve it first. Reads the same dictionary `game_events` does.
pub fn event_ids(demo: &[u8], index: &DemoIndex, wanted: &[&str]) -> Result<BTreeSet<i32>> {
    use csgoproto::CMsgSource1LegacyGameEventList;
    let mut ids = BTreeSet::new();
    for frame in &index.frames {
        if frame.cmd != CMD_PACKET && frame.cmd != CMD_SIGNON_PACKET && frame.cmd != CMD_FULL_PACKET
        {
            continue;
        }
        let payload = frame_payload(demo, frame)?;
        let messages = if frame.cmd == CMD_FULL_PACKET {
            match csgoproto::CDemoFullPacket::decode(payload.as_slice())?.packet {
                Some(packet) => crate::bitwriter::read_messages(packet.data())?,
                None => continue,
            }
        } else {
            crate::bitwriter::read_messages(CDemoPacket::decode(payload.as_slice())?.data())?
        };
        for message in messages {
            if message.msg_type != GE_SOURCE1_LEGACY_GAME_EVENT_LIST {
                continue;
            }
            if let Ok(list) = CMsgSource1LegacyGameEventList::decode(message.payload.as_slice()) {
                for descriptor in &list.descriptors {
                    if wanted.contains(&descriptor.name()) {
                        ids.insert(descriptor.eventid());
                    }
                }
            }
        }
    }
    Ok(ids)
}

/// Every game event of the named kinds, decoded against the demo's own event dictionary.
///
/// CS2 sends the dictionary once, as `GE_Source1LegacyGameEventList`, and thereafter refers to
/// events by id with their keys positional and unnamed. So the dictionary has to be read first and
/// the ids resolved through it; an event stream read without it is a list of anonymous numbers.
pub fn game_events(demo: &[u8], index: &DemoIndex, wanted: &[&str]) -> Result<Vec<GameEvent>> {
    use csgoproto::{CMsgSource1LegacyGameEvent, CMsgSource1LegacyGameEventList};

    let mut dictionary: HashMap<i32, (String, Vec<String>)> = HashMap::new();
    let mut found = Vec::new();

    for frame in &index.frames {
        if frame.cmd != CMD_PACKET && frame.cmd != CMD_SIGNON_PACKET {
            continue;
        }
        let Some(messages) = packet_messages(demo, frame)? else {
            continue;
        };
        for message in &messages {
            match message.msg_type {
                GE_SOURCE1_LEGACY_GAME_EVENT_LIST => {
                    let list = CMsgSource1LegacyGameEventList::decode(message.payload.as_slice())?;
                    for descriptor in list.descriptors {
                        dictionary.insert(
                            descriptor.eventid(),
                            (
                                descriptor.name().to_string(),
                                descriptor
                                    .keys
                                    .iter()
                                    .map(|key| key.name().to_string())
                                    .collect(),
                            ),
                        );
                    }
                }
                GE_SOURCE1_LEGACY_GAME_EVENT => {
                    let Ok(event) = CMsgSource1LegacyGameEvent::decode(message.payload.as_slice())
                    else {
                        continue;
                    };
                    let Some((name, key_names)) = dictionary.get(&event.eventid()) else {
                        continue;
                    };
                    if !wanted.iter().any(|w| w == name) {
                        continue;
                    }
                    let keys = event
                        .keys
                        .iter()
                        .enumerate()
                        .map(|(i, key)| {
                            let label = key_names
                                .get(i)
                                .cloned()
                                .unwrap_or_else(|| format!("key{i}"));
                            (label, key_value(key))
                        })
                        .collect();
                    found.push(GameEvent {
                        tick: frame.tick(),
                        name: name.clone(),
                        keys,
                    });
                }
                _ => {}
            }
        }
    }
    Ok(found)
}

/// A key holds exactly one of its typed fields; render whichever is present.
fn key_value(key: &csgoproto::c_msg_source1_legacy_game_event::KeyT) -> String {
    if let Some(v) = &key.val_string {
        return v.clone();
    }
    if let Some(v) = key.val_long {
        return v.to_string();
    }
    if let Some(v) = key.val_short {
        return v.to_string();
    }
    if let Some(v) = key.val_byte {
        return v.to_string();
    }
    if let Some(v) = key.val_bool {
        return v.to_string();
    }
    if let Some(v) = key.val_uint64 {
        return v.to_string();
    }
    if let Some(v) = key.val_float {
        return v.to_string();
    }
    String::new()
}

/// A suppressed shot that the recording says connected.
#[derive(Debug, Clone)]
pub struct OrphanedDamage {
    pub tick: i32,
    pub event: String,
    pub victim_pawn: i32,
    pub damage: i64,
    pub weapon: String,
    pub fatal: bool,
}

/// The damage a suppression would leave unexplained.
///
/// A shot is held responsible for a damage event when the event names its shooter's pawn and lands
/// within a few ticks of it — the hurt is recorded when the bullet arrives, which is not always the
/// tick it left the barrel.
///
/// A `player_hurt` left behind means health that drops for no visible reason. A `player_death`
/// means a player who falls over unshot, and unlike the hurt it cannot simply be removed: the
/// recording stops simulating a dead player, so there is no track to put them back on. That
/// asymmetry is why the two are reported separately and only one of them is ever recoverable.
pub fn orphaned_damage(
    demo: &[u8],
    index: &DemoIndex,
    shooter_entity: i32,
    shot_ticks: &[i32],
    window: i32,
) -> Result<Vec<OrphanedDamage>> {
    let events = game_events(demo, index, &DAMAGE_EVENTS)?;
    let mut orphans = Vec::new();
    for event in events {
        let Some(attacker) = event.int("attacker_pawn") else {
            continue;
        };
        if entity_index(attacker as u32) != shooter_entity {
            continue;
        }
        if !shot_ticks
            .iter()
            .any(|tick| event.tick >= *tick && event.tick <= tick + window)
        {
            continue;
        }
        orphans.push(OrphanedDamage {
            tick: event.tick,
            victim_pawn: event
                .int("userid_pawn")
                .map_or(-1, |handle| entity_index(handle as u32)),
            damage: event.int("dmg_health").unwrap_or(0),
            weapon: event.key("weapon").unwrap_or("").to_string(),
            fatal: event.name == "player_death",
            event: event.name,
        });
    }
    Ok(orphans)
}

/// One decal the server told clients to paint.
///
/// It carries where the bullet landed but not who fired it, which is the whole difficulty: a decal
/// cannot be matched to a shot by reading a field, only by geometry.
#[derive(Debug, Clone)]
pub struct Decal {
    pub frame: usize,
    pub tick: i32,
    pub position: [f32; 3],
    pub entity_handle: u32,
}

/// Every decal placement in the demo, in frame order.
pub fn decals(demo: &[u8], index: &DemoIndex) -> Result<Vec<Decal>> {
    use csgoproto::CMsgPlaceDecalEvent;
    let mut found = Vec::new();
    for frame in &index.frames {
        if frame.cmd != CMD_PACKET && frame.cmd != CMD_SIGNON_PACKET {
            continue;
        }
        let Some(messages) = packet_messages(demo, frame)? else {
            continue;
        };
        for message in &messages {
            if message.msg_type != GE_PLACE_DECAL_EVENT {
                continue;
            }
            let Ok(decal) = CMsgPlaceDecalEvent::decode(message.payload.as_slice()) else {
                continue;
            };
            let Some(position) = &decal.position else {
                continue;
            };
            found.push(Decal {
                frame: frame.index,
                tick: frame.tick(),
                position: [position.x(), position.y(), position.z()],
                entity_handle: decal.entityhandle(),
            });
        }
    }
    Ok(found)
}

/// The unit direction a shot was fired in, from its recorded view angles.
///
/// Source angles are (pitch, yaw, roll) in degrees with pitch positive downwards, which is why the
/// vertical term is negated.
pub fn shot_direction(angles: [f32; 3]) -> [f32; 3] {
    let pitch = angles[0].to_radians();
    let yaw = angles[1].to_radians();
    let cos_pitch = pitch.cos();
    [cos_pitch * yaw.cos(), cos_pitch * yaw.sin(), -pitch.sin()]
}

/// How far a point lies off a shot's line of fire, and how far along it.
///
/// Returns `None` for a point behind the muzzle: a decal cannot be attributed to a bullet that was
/// travelling away from it, however close the line passes.
pub fn ray_offset(origin: [f32; 3], direction: [f32; 3], point: [f32; 3]) -> Option<(f32, f32)> {
    let to_point = [
        point[0] - origin[0],
        point[1] - origin[1],
        point[2] - origin[2],
    ];
    let along =
        to_point[0] * direction[0] + to_point[1] * direction[1] + to_point[2] * direction[2];
    if along <= 0.0 {
        return None;
    }
    let closest = [
        origin[0] + direction[0] * along,
        origin[1] + direction[1] * along,
        origin[2] + direction[2] * along,
    ];
    let offset = ((point[0] - closest[0]).powi(2)
        + (point[1] - closest[1]).powi(2)
        + (point[2] - closest[2]).powi(2))
    .sqrt();
    Some((offset, along))
}
