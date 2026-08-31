//! RIPEMD-128，仅为 MDX 的键索引解扰而存在。
//!
//! 这不是安全用途，也不该被当成安全用途：MDX 用它从**文件里明摆着的 4 个字节**派生
//! 密钥（见 `super::deobfuscate`），任何人都能算出来。它是防呆，不是加密。
//!
//! 自己写而不引依赖的理由：Rust 生态里 RIPEMD-128（注意不是更常见的 160 位）的实现
//! 要么在 `ripemd` crate 的老版本里、要么随 `digest` 拖进一串 trait 层。为了一个
//! 64 字节输入的哈希引入那条链，与本项目的依赖克制不相称。算法本身是公开标准，
//! 60 行写完，且有官方测试向量可钉死。

const S: [u32; 64] = [
    11, 14, 15, 12, 5, 8, 7, 9, 11, 13, 14, 15, 6, 7, 9, 8, //
    7, 6, 8, 13, 11, 9, 7, 15, 7, 12, 15, 9, 11, 7, 13, 12, //
    11, 13, 6, 7, 14, 9, 13, 15, 14, 8, 13, 6, 5, 12, 7, 5, //
    11, 12, 14, 15, 14, 15, 9, 8, 9, 14, 5, 6, 8, 6, 5, 12,
];
const SP: [u32; 64] = [
    8, 9, 9, 11, 13, 15, 15, 5, 7, 7, 8, 11, 14, 14, 12, 6, //
    9, 13, 15, 7, 12, 8, 9, 11, 7, 7, 12, 7, 6, 15, 13, 11, //
    9, 7, 15, 11, 8, 6, 6, 14, 12, 13, 5, 14, 13, 13, 7, 5, //
    15, 5, 8, 11, 14, 14, 6, 14, 6, 9, 12, 9, 12, 5, 15, 8,
];
const R: [usize; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, //
    7, 4, 13, 1, 10, 6, 15, 3, 12, 0, 9, 5, 2, 14, 11, 8, //
    3, 10, 14, 4, 9, 15, 8, 1, 2, 7, 0, 6, 13, 11, 5, 12, //
    1, 9, 11, 10, 0, 8, 12, 4, 13, 3, 7, 15, 14, 5, 6, 2,
];
const RP: [usize; 64] = [
    5, 14, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12, //
    6, 11, 3, 7, 0, 13, 5, 10, 14, 15, 8, 12, 4, 9, 1, 2, //
    15, 5, 1, 3, 7, 14, 6, 9, 11, 8, 12, 2, 10, 0, 4, 13, //
    8, 6, 4, 1, 3, 11, 15, 0, 5, 12, 2, 13, 9, 7, 10, 14,
];

fn f(round: usize, x: u32, y: u32, z: u32) -> u32 {
    match round {
        0 => x ^ y ^ z,
        1 => (x & y) | (!x & z),
        2 => (x | !y) ^ z,
        _ => (x & z) | (y & !z),
    }
}

/// 返回 16 字节摘要。
pub fn ripemd128(msg: &[u8]) -> [u8; 16] {
    const K: [u32; 4] = [0x0000_0000, 0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc];
    const KP: [u32; 4] = [0x50a2_8be6, 0x5c4d_d124, 0x6d70_3ef3, 0x0000_0000];

    let mut h: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

    // 填充：0x80、补零至 56 (mod 64)、再接 8 字节小端比特长度。
    let mut data = msg.to_vec();
    let bits = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_le_bytes());

    for chunk in data.as_chunks::<64>().0 {
        let mut x = [0u32; 16];
        for (i, w) in x.iter_mut().enumerate() {
            *w = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut ap, mut bp, mut cp, mut dp) = (h[0], h[1], h[2], h[3]);

        for j in 0..64 {
            let round = j / 16;
            let t = a
                .wrapping_add(f(round, b, c, d))
                .wrapping_add(x[R[j]])
                .wrapping_add(K[round])
                .rotate_left(S[j]);
            a = d;
            d = c;
            c = b;
            b = t;

            // 平行线走**相反**的轮函数顺序：3-round，不是 round。
            let t = ap
                .wrapping_add(f(3 - round, bp, cp, dp))
                .wrapping_add(x[RP[j]])
                .wrapping_add(KP[round])
                .rotate_left(SP[j]);
            ap = dp;
            dp = cp;
            cp = bp;
            bp = t;
        }

        // 两条线交叉汇合，且整体右移一格——写成直白赋值会互相覆盖，故先算 t。
        let t = h[1].wrapping_add(c).wrapping_add(dp);
        h[1] = h[2].wrapping_add(d).wrapping_add(ap);
        h[2] = h[3].wrapping_add(a).wrapping_add(bp);
        h[3] = h[0].wrapping_add(b).wrapping_add(cp);
        h[0] = t;
    }

    let mut out = [0u8; 16];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::ripemd128;

    fn hex(b: [u8; 16]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// 官方测试向量（RIPEMD-128 规范附录）。这些常量是外部权威，不是本实现的产物——
    /// 换言之它们能证伪本文件，而本文件不能修改它们。
    #[test]
    fn 官方测试向量() {
        assert_eq!(hex(ripemd128(b"")), "cdf26213a150dc3ecb610f18f6b38b46");
        assert_eq!(hex(ripemd128(b"a")), "86be7afa339d0fc7cfc785e72f578d33");
        assert_eq!(hex(ripemd128(b"abc")), "c14a12199c66e4ba84636b0f69144c77");
        assert_eq!(
            hex(ripemd128(b"message digest")),
            "9e327b3d6e523062afc1132d7df9d1b8"
        );
        assert_eq!(
            hex(ripemd128(b"abcdefghijklmnopqrstuvwxyz")),
            "fd2aa607f71dc8f510714922b371834e"
        );
    }

    /// 跨块（>55 字节即需第二个块）与长度字段的正确性。
    #[test]
    fn 多块输入() {
        assert_eq!(
            hex(ripemd128(
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
            )),
            "d1e959eb179c911faea4624c60c5c702"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(hex(ripemd128(&million)), "4a7f5723f954eba1216c9d8f6320431f");
    }
}
