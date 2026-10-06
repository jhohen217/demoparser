pub fn light_snapshot(demo: &[u8], idx: &index::DemoIndex, id: i32, tick: i32) -> Result<serde_json::Value> {
    ensure!((0..32768).contains(&id) && tick >= 0, "invalid snapshot entity or tick");
    let builder = BoundaryBuilder::new(demo, idx)?;
    let checkpoint = idx.frames.iter().rev().find(|f| f.cmd == CMD_FULL_PACKET && f.tick() <= tick).context("no checkpoint")?;
    let mut tables = builder.checkpoints[&checkpoint.index].0.clone();
    let mut state = State {classes:&builder.classes, qf:&builder.qf, huf:&builder.huf,
        entities:BTreeMap::new(), baselines:BTreeMap::new(), baseline_entries:Vec::new(),
        alternate_baselines:BTreeMap::new(), non_transmitted:BTreeSet::new(), template:None};
    state.tables(&tables.snapshot);
    for frame in &idx.frames[checkpoint.index..] {
        if frame.tick() > tick { break; }
        let packet = match frame.cmd {
            CMD_FULL_PACKET => CDemoFullPacket::decode(payload(demo,frame)?.as_slice())?.packet,
            CMD_PACKET => Some(CDemoPacket::decode(payload(demo,frame)?.as_slice())?),
            _ => None,
        };
        if let Some(packet) = packet {
            for message in read_messages(packet.data())? {
                if message.msg_type == 55 { state.packet(&message.payload)?; }
                else if message.msg_type == 44 || message.msg_type == 45 {
                    tables.message(&message)?; state.tables(&tables.snapshot);
                }
            }
        }
    }
    let e = state.entities.get(&id).context("entity missing at requested tick")?;
    let class = &builder.classes[e.class as usize];
    let mut rows = Vec::new();
    for (key,bits) in &e.values {
        let mut fp = generate_fp(); fp.last = key.len()-1;
        for (i,v) in key.iter().enumerate(){fp.path[i]=*v;}
        let field = find_field(&fp,&class.serializer)?;
        let value = Bitreader::new(&bits.bytes).decode(&get_decoder_from_field(field)?, &builder.qf)?;
        rows.push(serde_json::json!({"path":key,"name":field_name(field),"value":format!("{value:?}"),"bytes":bits.bytes,"bit_len":bits.len}));
    }
    use sha2::Digest;
    Ok(serde_json::json!({"donor_sha256":format!("{:x}", sha2::Sha256::digest(demo)),"tick":tick,"entity":id,"class_id":e.class,"class_name":class.name,
        "serial":e.serial,"unknown":e.unknown,"fields":rows,
        "baseline":state.baselines.get(&e.class)}))
}

fn add_light_baseline(tables: &mut CDemoStringTables, class: u32, bytes: &[u8]) -> Result<()> {
    let table = tables.tables.iter_mut().find(|t| t.table_name() == "instancebaseline").context("missing instancebaseline")?;
    let key = class.to_string();
    ensure!(!table.items.iter().any(|i| i.str() == key), "target already has light baseline; prototype requires unused class");
    table.items.push(csgoproto::c_demo_string_tables::ItemsT {str:Some(key), data:Some(bytes.to_vec().into())});
    Ok(())
}

fn packet_end(msg: &CsvcMsgPacketEntities, state: &State<'_>) -> Result<(i32,usize)> {
    let data=msg.entity_data(); let mut reader=Bitreader::new(data); let mut id=-1;
    let class_bits=(state.classes.len() as f32).log2().ceil() as u32;
    for _ in 0..msg.updated_entries() {
        id+=1+reader.read_u_bit_var()? as i32;
        let command=reader.read_nbits(2)?;
        if command&1!=0 {continue;}
        let class=if command==2 {
            let c=reader.read_nbits(class_bits)?;reader.read_nbits(17)?;reader.read_varint()?;c
        } else {
            if msg.has_pvs_vis_bits_deprecated()!=0 {let p=reader.read_nbits(2)?;ensure!(p==0 || p==2,"unsupported PVS");}
            state.entities.get(&id).context("unknown entity")?.class
        };
        let serializer=&state.classes[class as usize].serializer;
        for fp in paths(&mut reader,state.huf)? {
            reader.decode(&get_decoder_from_field(find_field(&fp,serializer)?)?,state.qf)?;
        }
    }
    ensure!(msg.updated_entries() >= 0 && id < 32768, "invalid entity record bounds");
    let remain=reader.bits_remaining().context("missing bit count")?;
    ensure!(remain<8,"unexpected entity packet tail");
    Ok((id,data.len()*8-remain))
}

pub fn light_inject(demo: &[u8], donor: &[u8], snap: &serde_json::Value, new_id: i32) -> Result<(Vec<u8>,serde_json::Value)> {
    validate_snapshot_shape(snap)?;
    ensure!((0..32768).contains(&new_id), "invalid reserved light index");
    use sha2::Digest;
    let donor_hash = format!("{:x}", sha2::Sha256::digest(donor));
    ensure!(snap["donor_sha256"].as_str() == Some(donor_hash.as_str()), "snapshot is not bound to supplied donor; generate it with this tool");
    let idx=index::DemoIndex::build(demo)?; let donor_idx=index::DemoIndex::build(donor)?;
    for cmd in [CMD_CLASS_INFO,CMD_SEND_TABLES] {
        let a=idx.frames.iter().find(|f| f.cmd==cmd).context("missing target schema")?;
        let b=donor_idx.frames.iter().find(|f| f.cmd==cmd).context("missing donor schema")?;
        ensure!(payload(demo,a)?==payload(donor,b)?,"donor and target schemas differ");
    }
    let builder=BoundaryBuilder::new(demo,&idx)?;
    let class=snap["class_id"].as_u64().context("class")? as u32;
    ensure!(matches!(builder.classes.get(class as usize).context("snapshot class absent from target schema")?.name.as_str(), "COmniLight" | "CBarnLight" | "CRectLight"), "unsupported light class");
    let baseline: Vec<u8>=serde_json::from_value(snap["baseline"].clone())?;
    let mut light=Entity {class,serial:snap["serial"].as_u64().context("serial")? as u32,unknown:snap["unknown"].as_u64().context("create flags")? as u32,values:BTreeMap::new()};
    for f in snap["fields"].as_array().context("fields")? {
        let key: Vec<i32>=serde_json::from_value(f["path"].clone())?;
        let bytes: Vec<u8>=serde_json::from_value(f["bytes"].clone())?;
        let mut bits=Bits{bytes,len:f["bit_len"].as_u64().context("bit len")? as usize};
        if f["name"]=="m_nHierarchyId" {bits=encode_varint(new_id as u32);}
        light.values.insert(key,bits);
    }
    let mut state=State {classes:&builder.classes,qf:&builder.qf,huf:&builder.huf,
        entities:BTreeMap::new(),baselines:BTreeMap::new(),baseline_entries:Vec::new(),
        alternate_baselines:BTreeMap::new(),non_transmitted:BTreeSet::new(),template:None};
    let mut tables=Tables::default();
    let first_full=*idx.full_packets.first().context("target has no full checkpoint")?;
    let mut first_tables=builder.checkpoints[&first_full].0.snapshot.clone();
    add_light_baseline(&mut first_tables,class,&baseline)?;
    let mut out=demo[..16].to_vec();let mut offsets=BTreeMap::new();let mut injected=Vec::new();
    let mut sync_done=false; let mut max_original=-1;let mut entity_packets=0;let mut nondelta=0;
    for frame in &idx.frames {
        let mut raw=payload(demo,frame)?;
        let mut full: Option<CDemoFullPacket>=None;
        let mut packet=match frame.cmd {
            CMD_FULL_PACKET=>{
                let mut f=CDemoFullPacket::decode(raw.as_slice())?;
                if let Some(t)=&f.string_table {tables.overlay(t.clone());state.tables(t);}
                if let Some(t)=&mut f.string_table {add_light_baseline(t,class,&baseline)?;}
                let p=f.packet.take();full=Some(f);p
            },
            CMD_SIGNON_PACKET|CMD_PACKET=>Some(CDemoPacket::decode(raw.as_slice())?),
            CMD_STRING_TABLES=>{
                let mut t=CDemoStringTables::decode(raw.as_slice())?;
                tables.overlay(t.clone());state.tables(&t);
                add_light_baseline(&mut t,class,&baseline)?;raw=t.encode_to_vec();None
            },
            _=>None,
        };
        if let Some(p)=&mut packet {
            let mut messages=read_messages(p.data())?;
            for m in &mut messages {
                if m.msg_type!=55 {
                    tables.message(m)?;
                    if m.msg_type==44 || m.msg_type==45 {state.tables(&tables.snapshot);}
                    continue;
                }
                if !sync_done {
                    // Match the production writer's explicit pre-gameplay baseline sync.
                    let data=first_tables.encode_to_vec();
                    out.extend(frame_header(CMD_STRING_TABLES,false,frame.tick_raw,data.len() as u32));out.extend(data);
                    sync_done=true;
                }
                let mut msg=CsvcMsgPacketEntities::decode(m.payload.as_slice())?;
                state.packet(&m.payload)?;
                let (last,used)=packet_end(&msg,&state)?;
                max_original=max_original.max(last);ensure!(last<new_id,"reserved light index is not above all source records");
                ensure!(!state.entities.contains_key(&new_id) && !state.non_transmitted.contains(&new_id),"light index collision");
                entity_packets+=1;
                if !msg.legacy_is_delta() {nondelta+=1;}
                if !msg.legacy_is_delta() || frame.cmd==CMD_FULL_PACKET || entity_packets==1 {
                    let mut w=BitWriter::new();append_bits(&mut w,&slice_bits(msg.entity_data(),0,used));
                    w.write_u_bit_var((new_id-last-1) as u32);w.write_nbits(2,2);
                    w.write_nbits(class,(builder.classes.len() as f32).log2().ceil() as u32);
                    w.write_nbits(light.serial,17);w.write_varint(light.unknown);
                    let start=w.bits_written();
                    encode_paths(&mut w,light.values.keys(),&builder.huf)?;
                    for bits in light.values.values(){append_bits(&mut w,bits);}
                    let mut lengths=msg.serialized_entities().to_vec();
                    write_varint(&mut lengths,(w.bits_written()-start) as u32);
                    msg.serialized_entities=Some(lengths.into());
                    msg.entity_data=Some(w.finish().into());msg.updated_entries=Some(msg.updated_entries()+1);
                    msg.max_entries=Some(msg.max_entries().max(new_id+1));
                    m.payload=msg.encode_to_vec();injected.push(serde_json::json!({"frame":frame.index,"tick":frame.tick(),"full_checkpoint":frame.cmd==CMD_FULL_PACKET}));
                }
            }
            p.data=Some(write_messages(&messages).into());
            raw=p.encode_to_vec();
        }
        if let Some(mut f)=full {f.packet=packet;raw=f.encode_to_vec();}
        offsets.insert(frame.frame_offset,out.len() as u32);
        let compressed=if frame.compressed {snap::raw::Encoder::new().compress_vec(&raw)?} else {raw};
        out.extend(frame_header(frame.cmd,frame.compressed,frame.tick_raw,compressed.len() as u32));out.extend(compressed);
    }
    for (range,old) in [(8..12,idx.header_file_info_offset),(12..16,idx.header_spawn_groups_offset)] {
        let new=if old==0 {0} else {*offsets.get(&(old as u64)).context("unmapped trailer pointer")?};
        out[range].copy_from_slice(&new.to_le_bytes());
    }
    let report=serde_json::json!({"new_entity":new_id,"class_id":class,"maximum_original_record":max_original,
        "entity_packets":entity_packets,"non_delta_packets":nondelta,"injected":injected,"output_bytes":out.len()});
    Ok((out,report))
}

fn validate_snapshot_shape(snapshot: &serde_json::Value) -> Result<()> {
    ensure!(snapshot["class_id"].as_u64().is_some_and(|x| x <= u32::MAX as u64), "invalid snapshot class");
    ensure!(snapshot["serial"].as_u64().is_some_and(|x| x < (1 << 17)), "invalid snapshot serial");
    ensure!(snapshot["unknown"].as_u64().is_some_and(|x| x <= u32::MAX as u64), "invalid create flags");
    let _: Vec<u8> = serde_json::from_value(snapshot["baseline"].clone()).context("invalid snapshot baseline")?;
    let rows = snapshot["fields"].as_array().context("missing snapshot fields")?;
    ensure!(!rows.is_empty(), "snapshot has no fields");
    let mut seen = BTreeSet::new();
    for row in rows {
        let path: Vec<i32> = serde_json::from_value(row["path"].clone()).context("invalid field path")?;
        ensure!(!path.is_empty() && path.len() <= 7 && path.iter().all(|x| *x >= 0), "invalid snapshot field path");
        ensure!(seen.insert(path), "duplicate snapshot field path");
        let bytes: Vec<u8> = serde_json::from_value(row["bytes"].clone()).context("invalid field bytes")?;
        let bits = row["bit_len"].as_u64().context("invalid field bit count")?;
        ensure!(bits > 0 && bits <= bytes.len() as u64 * 8, "field bit count exceeds its bytes");
    }
    Ok(())
}

#[cfg(test)]
mod release_light_tests {
    use super::*;
    fn snapshot() -> serde_json::Value {
        serde_json::json!({"class_id":1,"serial":1,"unknown":0,"baseline":[],
            "fields":[{"path":[0],"bytes":[1],"bit_len":1}]})
    }
    #[test]
    fn malformed_snapshots_fail_before_demo_processing() {
        assert!(validate_snapshot_shape(&snapshot()).is_ok());
        let mut value = snapshot(); value["serial"] = serde_json::json!(1 << 17);
        assert!(light_inject(&[], &[], &value, 2047).is_err());
        let mut value = snapshot(); value["fields"][0]["bit_len"] = serde_json::json!(9);
        assert!(light_inject(&[], &[], &value, 2047).is_err());
        let mut value = snapshot(); value["fields"][0]["path"] = serde_json::json!([-1]);
        assert!(validate_snapshot_shape(&value).is_err());
        let mut value = snapshot();
        let duplicate = value["fields"][0].clone();
        value["fields"].as_array_mut().unwrap().push(duplicate);
        assert!(validate_snapshot_shape(&value).is_err());
    }
    #[test]
    fn snapshots_from_another_donor_are_rejected_before_decoding() {
        let mut value = snapshot(); value["donor_sha256"] = serde_json::json!("0".repeat(64));
        let error = light_inject(&[], b"different donor", &value, 2047).unwrap_err();
        assert!(error.to_string().contains("supplied donor"));
    }
}
