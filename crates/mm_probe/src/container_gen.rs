//! Minimal-but-valid MKV and MP4 generators for tests and `testdata/media/`.

/// ISO-BMFF file with `ftyp` + `moov`/`trak`/`tkhd` + `avc1` sample description.
///
/// `pixel_*` go in the sample description; `display_*` go in `tkhd` (16.16).
pub fn minimal_mp4(pixel_w: u16, pixel_h: u16, display_w: u16, display_h: u16) -> Vec<u8> {
    let ftyp = box_of(b"ftyp", &{
        let mut p = Vec::new();
        p.extend_from_slice(b"isom");
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"isom");
        p
    });

    let avcc_payload = [
        1u8,  // configurationVersion
        66,   // Baseline profile
        0,    // constraint
        30,   // level
        0xFF, // lengthSizeMinusOne (low 2 bits = 3)
        0xE0, // numOfSPS = 0
        0,    // numOfPPS = 0
    ];
    let avcc = box_of(b"avcC", &avcc_payload);

    let mut avc1_payload = Vec::new();
    avc1_payload.extend_from_slice(&[0u8; 6]); // reserved
    avc1_payload.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    avc1_payload.extend_from_slice(&[0u8; 16]); // pre_defined / reserved
    avc1_payload.extend_from_slice(&pixel_w.to_be_bytes());
    avc1_payload.extend_from_slice(&pixel_h.to_be_bytes());
    avc1_payload.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horizresolution
    avc1_payload.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vertresolution
    avc1_payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
    avc1_payload.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    avc1_payload.extend_from_slice(&[0u8; 32]); // compressorname
    avc1_payload.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    avc1_payload.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined
    avc1_payload.extend_from_slice(&avcc);
    let avc1 = box_of(b"avc1", &avc1_payload);

    let mut stsd_payload = Vec::new();
    stsd_payload.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    stsd_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    stsd_payload.extend_from_slice(&avc1);
    let stsd = box_of(b"stsd", &stsd_payload);

    let empty_full = |typ: &[u8; 4]| {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // version/flags
        p.extend_from_slice(&0u32.to_be_bytes()); // entry_count
        box_of(typ, &p)
    };
    let stts = empty_full(b"stts");
    let stsc = empty_full(b"stsc");
    let stco = empty_full(b"stco");
    let mut stsz_payload = Vec::new();
    stsz_payload.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    stsz_payload.extend_from_slice(&0u32.to_be_bytes()); // sample_size
    stsz_payload.extend_from_slice(&0u32.to_be_bytes()); // sample_count
    let stsz = box_of(b"stsz", &stsz_payload);

    let mut stbl_payload = Vec::new();
    stbl_payload.extend_from_slice(&stsd);
    stbl_payload.extend_from_slice(&stts);
    stbl_payload.extend_from_slice(&stsc);
    stbl_payload.extend_from_slice(&stsz);
    stbl_payload.extend_from_slice(&stco);
    let stbl = box_of(b"stbl", &stbl_payload);

    let mut vmhd_payload = Vec::new();
    vmhd_payload.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // version=0 flags=1
    vmhd_payload.extend_from_slice(&[0u8; 8]); // graphicsmode + opcolor
    let vmhd = box_of(b"vmhd", &vmhd_payload);

    let url = box_of(b"url ", &0x0000_0001u32.to_be_bytes()); // self-contained
    let mut dref_payload = Vec::new();
    dref_payload.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    dref_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    dref_payload.extend_from_slice(&url);
    let dref = box_of(b"dref", &dref_payload);
    let dinf = box_of(b"dinf", &dref);

    let mut minf_payload = Vec::new();
    minf_payload.extend_from_slice(&vmhd);
    minf_payload.extend_from_slice(&dinf);
    minf_payload.extend_from_slice(&stbl);
    let minf = box_of(b"minf", &minf_payload);

    // hdlr
    let mut hdlr_payload = Vec::new();
    hdlr_payload.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    hdlr_payload.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    hdlr_payload.extend_from_slice(b"vide");
    hdlr_payload.extend_from_slice(&[0u8; 12]); // reserved
    hdlr_payload.extend_from_slice(b"VideoHandler\0");
    let hdlr = box_of(b"hdlr", &hdlr_payload);

    // mdhd version 0
    let mut mdhd_payload = Vec::new();
    mdhd_payload.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    mdhd_payload.extend_from_slice(&0u32.to_be_bytes()); // creation
    mdhd_payload.extend_from_slice(&0u32.to_be_bytes()); // modification
    mdhd_payload.extend_from_slice(&1000u32.to_be_bytes()); // timescale
    mdhd_payload.extend_from_slice(&0u32.to_be_bytes()); // duration
    mdhd_payload.extend_from_slice(&0x55C4u16.to_be_bytes()); // language 'und'
    mdhd_payload.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    let mdhd = box_of(b"mdhd", &mdhd_payload);

    let mut mdia_payload = Vec::new();
    mdia_payload.extend_from_slice(&mdhd);
    mdia_payload.extend_from_slice(&hdlr);
    mdia_payload.extend_from_slice(&minf);
    let mdia = box_of(b"mdia", &mdia_payload);

    // tkhd version 0, width/height as 16.16
    let mut tkhd_payload = Vec::new();
    tkhd_payload.extend_from_slice(&0x0000_0007u32.to_be_bytes()); // enabled, in movie, in preview
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes()); // creation
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes()); // modification
    tkhd_payload.extend_from_slice(&1u32.to_be_bytes()); // track_id
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes()); // duration
    tkhd_payload.extend_from_slice(&0u64.to_be_bytes()); // reserved
    tkhd_payload.extend_from_slice(&0u16.to_be_bytes()); // layer
    tkhd_payload.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    tkhd_payload.extend_from_slice(&0u16.to_be_bytes()); // volume
    tkhd_payload.extend_from_slice(&0u16.to_be_bytes()); // reserved
    tkhd_payload.extend_from_slice(&identity_matrix());
    tkhd_payload.extend_from_slice(&u32::from(display_w).wrapping_shl(16).to_be_bytes());
    tkhd_payload.extend_from_slice(&u32::from(display_h).wrapping_shl(16).to_be_bytes());
    let tkhd = box_of(b"tkhd", &tkhd_payload);

    let mut trak_payload = Vec::new();
    trak_payload.extend_from_slice(&tkhd);
    trak_payload.extend_from_slice(&mdia);
    let trak = box_of(b"trak", &trak_payload);

    // mvhd version 0
    let mut mvhd_payload = Vec::new();
    mvhd_payload.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    mvhd_payload.extend_from_slice(&0u32.to_be_bytes()); // creation
    mvhd_payload.extend_from_slice(&0u32.to_be_bytes()); // modification
    mvhd_payload.extend_from_slice(&1000u32.to_be_bytes()); // timescale
    mvhd_payload.extend_from_slice(&0u32.to_be_bytes()); // duration
    mvhd_payload.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate
    mvhd_payload.extend_from_slice(&0x0100u16.to_be_bytes()); // volume
    mvhd_payload.extend_from_slice(&0u16.to_be_bytes()); // reserved
    mvhd_payload.extend_from_slice(&0u64.to_be_bytes()); // reserved
    mvhd_payload.extend_from_slice(&identity_matrix());
    mvhd_payload.extend_from_slice(&[0u8; 24]); // pre_defined
    mvhd_payload.extend_from_slice(&2u32.to_be_bytes()); // next_track_id
    let mvhd = box_of(b"mvhd", &mvhd_payload);

    let mut moov_payload = Vec::new();
    moov_payload.extend_from_slice(&mvhd);
    moov_payload.extend_from_slice(&trak);
    let moov = box_of(b"moov", &moov_payload);

    let mdat = box_of(b"mdat", &[]);

    let mut file = Vec::new();
    file.extend_from_slice(&ftyp);
    file.extend_from_slice(&moov);
    file.extend_from_slice(&mdat);
    file
}

/// Tiny EBML Matroska with one video track (pixel + optional display dims).
pub fn minimal_mkv(
    pixel_w: u64,
    pixel_h: u64,
    display_w: Option<u64>,
    display_h: Option<u64>,
) -> Vec<u8> {
    // EBML header
    let mut ebml_body = Vec::new();
    ebml_body.extend(ebml_uint(&[0x42, 0x86], 1)); // EBMLVersion
    ebml_body.extend(ebml_uint(&[0x42, 0xF7], 1)); // EBMLReadVersion
    ebml_body.extend(ebml_uint(&[0x42, 0xF2], 4)); // EBMLMaxIDLength
    ebml_body.extend(ebml_uint(&[0x42, 0xF3], 8)); // EBMLMaxSizeLength
    ebml_body.extend(ebml_bytes(&[0x42, 0x82], b"matroska")); // DocType
    ebml_body.extend(ebml_uint(&[0x42, 0x87], 4)); // DocTypeVersion
    ebml_body.extend(ebml_uint(&[0x42, 0x85], 2)); // DocTypeReadVersion
    let ebml = ebml_element(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_body);

    // Info
    let mut info_body = Vec::new();
    info_body.extend(ebml_uint(&[0x2A, 0xD7, 0xB1], 1_000_000)); // TimestampScale
    info_body.extend(ebml_utf8(&[0x4D, 0x80], "mm-probe")); // MuxingApp
    info_body.extend(ebml_utf8(&[0x57, 0x41], "mm-probe")); // WritingApp
    let info = ebml_element(&[0x15, 0x49, 0xA9, 0x66], &info_body);

    // Video
    let mut video_body = Vec::new();
    video_body.extend(ebml_uint(&[0xB0], pixel_w)); // PixelWidth
    video_body.extend(ebml_uint(&[0xBA], pixel_h)); // PixelHeight
    if let Some(w) = display_w {
        video_body.extend(ebml_uint(&[0x54, 0xB0], w)); // DisplayWidth
    }
    if let Some(h) = display_h {
        video_body.extend(ebml_uint(&[0x54, 0xBA], h)); // DisplayHeight
    }
    let video = ebml_element(&[0xE0], &video_body);

    // TrackEntry
    let mut entry = Vec::new();
    entry.extend(ebml_uint(&[0xD7], 1)); // TrackNumber
    entry.extend(ebml_uint(&[0x73, 0xC5], 1)); // TrackUID
    entry.extend(ebml_uint(&[0x83], 1)); // TrackType = video
    entry.extend(ebml_bytes(&[0x86], b"V_MPEG4/ISO/AVC")); // CodecID
    entry.extend(video);
    let track_entry = ebml_element(&[0xAE], &entry);
    let tracks = ebml_element(&[0x16, 0x54, 0xAE, 0x6B], &track_entry);

    let mut segment_body = Vec::new();
    segment_body.extend(info);
    segment_body.extend(tracks);
    let segment = ebml_element(&[0x18, 0x53, 0x80, 0x67], &segment_body);

    let mut file = Vec::new();
    file.extend(ebml);
    file.extend(segment);
    file
}

fn box_of(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = 8 + payload.len() as u32;
    let mut v = Vec::with_capacity(size as usize);
    v.extend_from_slice(&size.to_be_bytes());
    v.extend_from_slice(typ);
    v.extend_from_slice(payload);
    v
}

fn identity_matrix() -> [u8; 36] {
    let mut m = [0u8; 36];
    m[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    m[16..20].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    m[32..36].copy_from_slice(&0x4000_0000u32.to_be_bytes());
    m
}

fn ebml_size(n: u64) -> Vec<u8> {
    if n <= 0x7E {
        vec![0x80 | n as u8]
    } else if n <= 0x3FFE {
        (0x4000 | n as u16).to_be_bytes().to_vec()
    } else if n <= 0x1F_FFFE {
        let v = 0x20_0000 | n as u32;
        vec![(v >> 16) as u8, (v >> 8) as u8, v as u8]
    } else if n <= 0x0FFF_FFFE {
        (0x1000_0000 | n as u32).to_be_bytes().to_vec()
    } else {
        (0x0100_0000_0000_0000_u64 | n).to_be_bytes().to_vec()
    }
}

fn ebml_element(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend(ebml_size(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

fn ebml_uint(id: &[u8], value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let start = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    ebml_element(id, &bytes[start..])
}

fn ebml_bytes(id: &[u8], payload: &[u8]) -> Vec<u8> {
    ebml_element(id, payload)
}

fn ebml_utf8(id: &[u8], s: &str) -> Vec<u8> {
    ebml_element(id, s.as_bytes())
}
