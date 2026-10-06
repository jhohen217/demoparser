//! Round trip proof for the topology encoder.
//!
//! Every distinct topology blob carried by dw_orig over ticks 250..450. The encoder is only
//! trustworthy for a topology we invent if it reproduces these byte for byte first, including the
//! `count_bits` width each blob chose for itself.

use super::*;

const BLOBS: &[&str] = &[
    "050300211c31c221e50413ce41012515d062ae1507",
    "054338200011c221251c73ce40012515d062ae1507",
    "1543082788700639a084700e422605a55640ac39571e",
    "1543382084700639a284634e402805a55640ac39571e",
    "94211c88706402a272560e",
    "94211c88806402a272560e",
    "94211c88806482a272560e",
    "94212088a06402a272560e",
    "b4211c88706402a20ac8ce6149",
    "b4211c88706402a20acace6149",
    "b4211c88706402a27216d46149",
    "b4211c887064c2a10ac8ce6149",
    "b4211c88806482a20ac8ce6149",
    "d4211c88706402a20ac82e204ca76501",
    "d4211c88706402a20aca2e204ca76501",
    "d4211c887064423865882e284ca76501",
    "d4211c887064c2a10ac82e204ca76501",
    "d4211c887064c2a10ac82e284ca76501",
    "d421841312ced0810ac82e204ca76501",
    "e401400847261caa1316d06140a96e0b03",
    "f3100e1290e6580a",
    "f310101290e6580a",
    "f310101294e6580a",
    "f4211c8810ce10920a211ccc73aa6c1d9706",
    "f4211c88706402a20aca2e84938a701d9706",
    "f4211c887064423865882e28cc80741d9706",
    "f421841312ced0810ac82e20cc80741d9706",
    "f421841312ced0810ac82e28cc80741d9706",
];

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn every_recorded_blob_parses() {
    for text in BLOBS {
        assert!(
            parse_topology(&unhex(text)).is_some(),
            "failed to parse {text}"
        );
    }
}

#[test]
fn every_recorded_blob_round_trips_byte_for_byte() {
    for text in BLOBS {
        let blob = unhex(text);
        let parsed = parse_topology(&blob).expect("parses");
        let encoded = encode_topology(&parsed).expect("encodes");
        assert_eq!(encoded, blob, "round trip changed {text}");
    }
}

#[test]
fn appending_preserves_every_existing_task() {
    for text in BLOBS {
        let blob = unhex(text);
        let before = parse_topology(&blob).expect("parses");

        // A sample task takes no dependencies, then a blend consumes the old last task and it.
        let mut after = before.clone();
        let sample = append_task(&mut after, 1, vec![]).expect("append sample");
        let previous_root = sample - 1;
        append_task(&mut after, 7, vec![previous_root, sample]).expect("append blend");

        assert_eq!(after.tasks.len(), before.tasks.len() + 2);
        assert_eq!(
            after.tasks[..before.tasks.len()],
            before.tasks[..],
            "appending disturbed an existing task in {text}"
        );

        // It must survive a write and read back as what we built.
        let encoded = encode_topology(&after).expect("encodes");
        let reparsed = parse_topology(&encoded).expect("re-parses");
        assert_eq!(
            reparsed, after,
            "appended topology did not survive a round trip"
        );
    }
}

#[test]
fn appending_widens_count_bits_only_when_it_must() {
    for text in BLOBS {
        let blob = unhex(text);
        let before = parse_topology(&blob).expect("parses");
        let ceiling = (1usize << before.count_bits) - 1;

        let mut after = before.clone();
        append_task(&mut after, 1, vec![]).expect("append");

        if before.tasks.len() < ceiling {
            assert_eq!(after.count_bits, before.count_bits, "widened unnecessarily");
        } else {
            assert_eq!(after.count_bits, before.count_bits + 1, "failed to widen");
        }
    }
}

#[test]
fn an_appended_task_pair_reads_back_from_the_payload() {
    for text in BLOBS {
        let blob = unhex(text);
        let before = parse_topology(&blob).expect("parses");
        let sequence_before: Vec<u32> = before.tasks.iter().map(|t| t.type_id).collect();

        // A zeroed payload walks cleanly: a sample reads clip 0 at time 0, and a blend's mask flag
        // reads as absent, so every existing task has a valid, if empty, set of fields.
        let mut payload = vec![0u8; PAYLOAD_BYTES];
        let end_before = payload_end(&sequence_before, &payload).expect("walks");
        let samplers_before = samplers(&sequence_before, &payload).expect("walks").len();

        // Append the pair: our clip, then a blend consuming the old result and it.
        let mut after = before.clone();
        let sample = append_task(&mut after, 1, vec![]).expect("append sample");
        append_task(&mut after, 7, vec![sample - 1, sample]).expect("append blend");

        const CLIP: u32 = 971;
        const TIME: u32 = 0x2BCD;
        const MASK: u32 = 12;
        let after_sample =
            append_sample_payload(&mut payload, end_before, CLIP, TIME).expect("sample payload");
        let after_blend = append_blend_payload(&mut payload, after_sample, 255, Some(MASK))
            .expect("blend payload");

        // The walk must now land exactly where the writes stopped.
        let sequence_after: Vec<u32> = after.tasks.iter().map(|t| t.type_id).collect();
        assert_eq!(
            payload_end(&sequence_after, &payload),
            Some(after_blend),
            "walk and write disagree for {text}"
        );

        // And the new sampler must read back as what we wrote, with the earlier ones untouched.
        let found = samplers(&sequence_after, &payload).expect("walks");
        assert_eq!(found.len(), samplers_before + 1);
        let ours = found.last().expect("at least one");
        assert_eq!(
            (ours.clip, ours.time),
            (CLIP, TIME),
            "appended sampler changed"
        );
        assert!(
            found[..samplers_before]
                .iter()
                .all(|s| s.clip == 0 && s.time == 0),
            "appending disturbed an existing sampler in {text}"
        );
    }
}

#[test]
fn moving_task_15_aligns_aim_and_snap_against_stationary_control() {
    // Current-build AK recordings at matched recoil phase. The moving recipe
    // inserts type 15 before the same AimCS and SnapWeapon tasks as the idle
    // recording. Its 209-bit field must not shift those later task values.
    let moving = unhex("8e1f0000200300980d00f03f0012b4d0f81f00928ab6fe2fe5a8146d4339437441a553b195b511b677627eee82e202ade3fe0100a407024000c07f0080e98100004c0f04380500000000000038050000189503ee3805000000000000da000000");
    let idle = unhex("8c480000200300980d00f03f0012b4d0f81f005e81d671ff0000d203017f0000e981000000027000480f00213905000060a7c62238050000900d672139050000");
    let moving_tasks = topology(&unhex("8421208880641eb92a06")).unwrap();
    let idle_tasks = topology(&unhex("f310101290e6580a")).unwrap();
    assert_eq!(aim_fields(&moving_tasks, &moving), Some((361, 377)));
    assert_eq!(aim_fields(&idle_tasks, &idle), Some((152, 168)));
    assert_eq!(read_u16(&moving, 361), Some(33137));
    assert_eq!(read_u16(&moving, 377), Some(29142));
    assert_eq!(read_u16(&idle, 152), Some(33118));
    assert_eq!(read_u16(&idle, 168), Some(29142));
    assert_eq!(payload_end(&moving_tasks, &moving), Some(435));
    assert_eq!(payload_end(&idle_tasks, &idle), Some(226));
}

#[test]
fn a_masked_blend_costs_twenty_one_bits_and_an_unmasked_one_nine() {
    let mut payload = vec![0u8; PAYLOAD_BYTES];
    let masked = append_blend_payload(&mut payload, 0, 128, Some(12)).expect("masked");
    let plain = append_blend_payload(&mut payload, 0, 128, None).expect("unmasked");
    assert_eq!(masked, 8 + 1 + 5 + 3 + 4);
    assert_eq!(plain, 8 + 1);
}
