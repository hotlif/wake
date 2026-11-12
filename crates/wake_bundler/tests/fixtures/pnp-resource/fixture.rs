// Synthetic STORE archive generated in memory; no checked-in binary or third-party source.
pub fn archive(version: u8) -> Vec<u8> {
    let entries = [
        (
            "package.json",
            r#"{"name":"dep","main":"index.js"}"#.to_owned(),
        ),
        (
            "index.js",
            format!(
                "globalThis.revision = {version}; globalThis.resource = new URL('./payload.data', import.meta.url);"
            ),
        ),
        ("payload.data", format!("payload-v{version}")),
    ];
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, contents) in entries {
        let name = format!("node_modules/dep/{name}");
        let data = contents.as_bytes();
        let mut crc = !0u32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        crc = !crc;
        let offset = bytes.len() as u32;
        let mut local = vec![0u8; 30];
        local[0..4].copy_from_slice(&0x0403_4b50u32.to_le_bytes());
        local[4..6].copy_from_slice(&20u16.to_le_bytes());
        local[14..18].copy_from_slice(&crc.to_le_bytes());
        local[18..22].copy_from_slice(&(data.len() as u32).to_le_bytes());
        local[22..26].copy_from_slice(&(data.len() as u32).to_le_bytes());
        local[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend(local);
        bytes.extend(name.as_bytes());
        bytes.extend(data);
        let mut record = vec![0u8; 46];
        record[0..4].copy_from_slice(&0x0201_4b50u32.to_le_bytes());
        record[4..6].copy_from_slice(&20u16.to_le_bytes());
        record[6..8].copy_from_slice(&20u16.to_le_bytes());
        record[16..20].copy_from_slice(&crc.to_le_bytes());
        record[20..24].copy_from_slice(&(data.len() as u32).to_le_bytes());
        record[24..28].copy_from_slice(&(data.len() as u32).to_le_bytes());
        record[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        record[42..46].copy_from_slice(&offset.to_le_bytes());
        central.extend(record);
        central.extend(name.as_bytes());
    }
    let mut end = vec![0u8; 22];
    end[0..4].copy_from_slice(&0x0605_4b50u32.to_le_bytes());
    end[8..10].copy_from_slice(&3u16.to_le_bytes());
    end[10..12].copy_from_slice(&3u16.to_le_bytes());
    end[12..16].copy_from_slice(&(central.len() as u32).to_le_bytes());
    end[16..20].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    bytes.extend(central);
    bytes.extend(end);
    bytes
}
