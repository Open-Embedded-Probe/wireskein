//! Serial-port framing (transports §1; oep-client-python's `cobs`): message + CRC-16 little endian, COBS-encoded,
//! sent as 0x00 <COBS> 0x00. CRC-16/CCITT-FALSE: poly 0x1021, init 0xFFFF, no reflection, no final xor.

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Standard COBS (no delimiter); no empty block after a full one at the end (transports §1).
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 254 + 2);
    let (mut i, n) = (0usize, data.len());
    loop {
        let mut j = i;
        while j < n && data[j] != 0 && j - i < 254 {
            j += 1;
        }
        let code = j - i + 1;
        out.push(code as u8);
        out.extend_from_slice(&data[i..j]);
        if j >= n {
            return out;
        }
        i = if code == 0xFF { j } else { j + 1 };
    }
}

pub fn decode(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(raw.len());
    let (mut i, n) = (0usize, raw.len());
    while i < n {
        let code = raw[i] as usize;
        i += 1;
        if code == 0 || i + code - 1 > n {
            return Err("COBS block runs past the frame".into());
        }
        out.extend_from_slice(&raw[i..i + code - 1]);
        i += code - 1;
        if code != 0xFF && i < n {
            out.push(0);
        }
    }
    Ok(out)
}

/// 0x00 <COBS(message + CRC)> 0x00.
pub fn frame(message: &[u8]) -> Vec<u8> {
    let crc = crc16(message);
    let mut body = message.to_vec();
    body.extend(crc.to_le_bytes());
    let mut out = vec![0u8];
    out.extend(encode(&body));
    out.push(0);
    out
}

/// raw: the bytes between two delimiters (without the 0x00).
pub fn unframe(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut data = decode(raw)?;
    if data.len() < 3 {
        return Err("frame shorter than a message and its CRC".into());
    }
    let got = u16::from_le_bytes([data[data.len() - 2], data[data.len() - 1]]);
    data.truncate(data.len() - 2);
    if crc16(&data) != got {
        return Err("CRC mismatch".into());
    }
    Ok(data)
}

/// The longest frame a message of this length makes.
pub fn frame_max(message_len: usize) -> usize {
    let n = message_len + 2;
    n + n.div_ceil(254) + 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_round_trips() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
        for n in [0usize, 1, 253, 254, 255, 508, 600] {
            let m: Vec<u8> = (0..n)
                .map(|i| if i % 97 == 3 { 0 } else { (i % 251) as u8 + 1 })
                .collect();
            let f = frame(&m);
            assert_eq!(f[0], 0);
            assert_eq!(*f.last().unwrap(), 0);
            assert!(!f[1..f.len() - 1].contains(&0));
            assert!(f.len() <= frame_max(n));
            if n > 0 {
                assert_eq!(unframe(&f[1..f.len() - 1]).unwrap(), m);
            }
        }
        let mut f = frame(b"hello");
        f[2] ^= 1;
        assert!(unframe(&f[1..f.len() - 1]).is_err());
    }
}
