const ROUND: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn compress(state: &mut [u32; 8], block: &[u8]) {
    let mut words = [0u32; 64];
    for (word, bytes) in words[..16].iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(bytes.try_into().unwrap());
    }
    for index in 16..64 {
        let first = words[index - 15];
        let second = words[index - 2];
        let small_first = first.rotate_right(7) ^ first.rotate_right(18) ^ (first >> 3);
        let small_second = second.rotate_right(17) ^ second.rotate_right(19) ^ (second >> 10);
        words[index] = words[index - 16]
            .wrapping_add(small_first)
            .wrapping_add(words[index - 7])
            .wrapping_add(small_second);
    }
    let mut working = *state;
    for (constant, word) in ROUND.iter().zip(words) {
        let upper =
            working[4].rotate_right(6) ^ working[4].rotate_right(11) ^ working[4].rotate_right(25);
        let choose = (working[4] & working[5]) ^ (!working[4] & working[6]);
        let first = working[7]
            .wrapping_add(upper)
            .wrapping_add(choose)
            .wrapping_add(*constant)
            .wrapping_add(word);
        let lower =
            working[0].rotate_right(2) ^ working[0].rotate_right(13) ^ working[0].rotate_right(22);
        let majority =
            (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
        let second = lower.wrapping_add(majority);
        working = [
            first.wrapping_add(second),
            working[0],
            working[1],
            working[2],
            working[3].wrapping_add(first),
            working[4],
            working[5],
            working[6],
        ];
    }
    for (value, addition) in state.iter_mut().zip(working) {
        *value = value.wrapping_add(addition);
    }
}

pub(super) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut state = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut chunks = bytes.chunks_exact(64);
    for block in &mut chunks {
        compress(&mut state, block);
    }
    let remainder = chunks.remainder();
    let mut tail = [0u8; 128];
    tail[..remainder.len()].copy_from_slice(remainder);
    tail[remainder.len()] = 0x80;
    let padded = if remainder.len() < 56 { 64 } else { 128 };
    tail[padded - 8..padded].copy_from_slice(&((bytes.len() as u64) * 8).to_be_bytes());
    for block in tail[..padded].chunks_exact(64) {
        compress(&mut state, block);
    }
    let mut result = [0u8; 32];
    for (output, word) in result.chunks_exact_mut(4).zip(state) {
        output.copy_from_slice(&word.to_be_bytes());
    }
    result
}
