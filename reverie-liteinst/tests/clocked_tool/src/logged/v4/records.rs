pub const READY: &[u8] = b"host\0ready\xff";
pub const INIT: &[u8] = b"host\0init\xff";
pub const CLEANUP: &[u8] = b"host\0cleanup\xff";
pub const DROP: &[u8] = b"host\0global-drop\xff";
pub const AFTER: &[u8] = b"host\0after-drop\xff";

pub fn request(clocked: bool, index: usize) -> u64 {
    if clocked {
        [7, 8, 9, 10, 17, 18, 21][index]
    } else {
        100 + index as u64
    }
}

pub fn rpc(index: usize, request: u64) -> Vec<u8> {
    let mut bytes = b"host\0rpc\xff".to_vec();
    bytes.extend_from_slice(&(index as u64).to_le_bytes());
    bytes.extend_from_slice(&request.to_le_bytes());
    bytes
}

pub fn expected(prefix: &[u8], burst: &[u8], clocked: bool, case: u8) -> Vec<(bool, Vec<u8>)> {
    assert!(case <= 5);
    let mut records = vec![(false, READY.to_vec()), (false, INIT.to_vec())];
    for index in 0..if case == 0 { 7 } else { 1 } {
        let guest = match index {
            0 => prefix.to_vec(),
            1 => burst.to_vec(),
            _ => vec![0, index as u8, 0xff],
        };
        records.push((true, guest));
        records.push((false, rpc(index, request(clocked, index))));
    }
    if case == 2 {
        records.push((false, rpc(1, u64::MAX)));
    }
    if case != 3 {
        for bytes in if case == 0 {
            [CLEANUP, DROP, AFTER]
        } else {
            [DROP, CLEANUP, AFTER]
        } {
            records.push((false, bytes.to_vec()));
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_causal_order_and_guest_projection() {
        for clocked in [false, true] {
            let records = expected(b"prefix", b"burst", clocked, 0);
            assert_eq!(records.len(), 19);
            assert_eq!(records[0], (false, READY.to_vec()));
            assert_eq!(records[1], (false, INIT.to_vec()));
            for index in 0..7 {
                assert!(records[2 + index * 2].0);
                assert_eq!(
                    records[3 + index * 2],
                    (false, rpc(index, request(clocked, index)))
                );
            }
            assert_eq!(
                records[16..],
                [
                    (false, CLEANUP.to_vec()),
                    (false, DROP.to_vec()),
                    (false, AFTER.to_vec())
                ]
            );
            let guest: Vec<_> = records
                .iter()
                .filter(|record| record.0)
                .flat_map(|record| record.1.clone())
                .collect();
            assert_eq!(
                guest,
                b"prefixburst\0\x02\xff\0\x03\xff\0\x04\xff\0\x05\xff\0\x06\xff"
            );
        }
    }

    #[test]
    fn refusal_records_do_not_become_success_records() {
        for case in 1..=5 {
            let records = expected(b"prefix", b"burst", true, case);
            assert_eq!(records.iter().filter(|record| record.0).count(), 1);
            assert_eq!(records[2], (true, b"prefix".to_vec()));
            assert_eq!(
                records.len(),
                match case {
                    2 => 8,
                    3 => 4,
                    _ => 7,
                }
            );
            if case == 2 {
                assert_eq!(records[4], (false, rpc(1, u64::MAX)));
            }
            if case != 3 {
                assert_eq!(
                    records[records.len() - 3..],
                    [
                        (false, DROP.to_vec()),
                        (false, CLEANUP.to_vec()),
                        (false, AFTER.to_vec())
                    ]
                );
            }
        }
    }
}
