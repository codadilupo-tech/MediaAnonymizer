//! Photoshop Image Resource Blocks (JPEG APP13 `Photoshop 3.0`).
//!
//! APP13 mostly carries IPTC captions/keywords but can also hold a JPEG
//! thumbnail of the full image. When the `keep` policy is combined with face
//! redaction we preserve the captions and drop only the thumbnails.

/// Resource IDs of embedded thumbnails (Photoshop 4 and 5+ variants).
const THUMBNAIL_IDS: [u16; 2] = [0x0409, 0x040C];

/// Rebuilds the resource list without thumbnails.
///
/// `data` is the IRB payload after the `Photoshop 3.0\0` signature. Returns
/// `None` if the structure cannot be parsed, in which case the caller must
/// drop the whole segment to stay on the safe side. The boolean tells whether
/// anything was removed.
pub fn strip_thumbnails(data: &[u8]) -> Option<(Vec<u8>, bool)> {
    let mut out = Vec::with_capacity(data.len());
    let mut removed = false;
    let mut pos = 0;
    while pos < data.len() {
        let start = pos;
        if data.get(pos..pos + 4)? != b"8BIM" {
            return None;
        }
        let id = u16::from_be_bytes(data.get(pos + 4..pos + 6)?.try_into().ok()?);
        pos += 6;
        // Pascal string, padded so that length byte + chars is even.
        let name_len = *data.get(pos)? as usize;
        pos += (name_len + 2) & !1;
        let size = u32::from_be_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let end = pos.checked_add(size)?;
        let padded_end = end + (size & 1);
        data.get(pos..end)?;
        let block_end = padded_end.min(data.len());
        if THUMBNAIL_IDS.contains(&id) {
            removed = true;
        } else {
            out.extend_from_slice(&data[start..block_end]);
        }
        pos = block_end;
    }
    Some((out, removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = b"8BIM".to_vec();
        v.extend_from_slice(&id.to_be_bytes());
        v.extend_from_slice(&[0, 0]); // empty name, padded
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    #[test]
    fn removes_only_thumbnails() {
        let iptc = block(0x0404, b"caption");
        let mut data = iptc.clone();
        data.extend(block(0x040C, b"JPEGDATA"));
        let (out, removed) = strip_thumbnails(&data).unwrap();
        assert!(removed);
        assert_eq!(out, iptc);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(strip_thumbnails(b"8BIM\x04").is_none());
        assert!(strip_thumbnails(b"XXXXXXXXXXXX").is_none());
    }
}
