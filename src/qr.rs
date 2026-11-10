// Pure-Rust, zero-dependency QR Code generator (ISO/IEC 18004)
// Supports Version 1 to 10 with Error Correction Level L and M in 8-bit Byte Mode.
#![allow(clippy::all)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QrEcLevel {
    L, // ~7% recovery
    M, // ~15% recovery
}

impl QrEcLevel {
    fn format_bits(&self) -> u8 {
        match self {
            QrEcLevel::L => 0b01,
            QrEcLevel::M => 0b00,
        }
    }
}

// Version specifications: (version, size, data_codewords_L, ec_per_block_L, num_blocks_L, data_codewords_M, ec_per_block_M, num_blocks_M)
struct VersionSpec {
    version: usize,
    size: usize,
    data_cw_l: usize,
    ec_per_block_l: usize,
    num_blocks_l: usize,
    data_cw_m: usize,
    ec_per_block_m: usize,
    num_blocks_m: usize,
    align_positions: &'static [usize],
}

static VERSION_SPECS: &[VersionSpec] = &[
    VersionSpec {
        version: 1,
        size: 21,
        data_cw_l: 19,
        ec_per_block_l: 7,
        num_blocks_l: 1,
        data_cw_m: 16,
        ec_per_block_m: 10,
        num_blocks_m: 1,
        align_positions: &[],
    },
    VersionSpec {
        version: 2,
        size: 25,
        data_cw_l: 34,
        ec_per_block_l: 10,
        num_blocks_l: 1,
        data_cw_m: 28,
        ec_per_block_m: 16,
        num_blocks_m: 1,
        align_positions: &[6, 18],
    },
    VersionSpec {
        version: 3,
        size: 29,
        data_cw_l: 55,
        ec_per_block_l: 15,
        num_blocks_l: 1,
        data_cw_m: 44,
        ec_per_block_m: 26,
        num_blocks_m: 1,
        align_positions: &[6, 22],
    },
    VersionSpec {
        version: 4,
        size: 33,
        data_cw_l: 80,
        ec_per_block_l: 20,
        num_blocks_l: 1,
        data_cw_m: 64,
        ec_per_block_m: 18,
        num_blocks_m: 2,
        align_positions: &[6, 26],
    },
    VersionSpec {
        version: 5,
        size: 37,
        data_cw_l: 108,
        ec_per_block_l: 26,
        num_blocks_l: 1,
        data_cw_m: 86,
        ec_per_block_m: 24,
        num_blocks_m: 2,
        align_positions: &[6, 30],
    },
    VersionSpec {
        version: 6,
        size: 41,
        data_cw_l: 136,
        ec_per_block_l: 18,
        num_blocks_l: 2,
        data_cw_m: 108,
        ec_per_block_m: 16,
        num_blocks_m: 4,
        align_positions: &[6, 34],
    },
    VersionSpec {
        version: 7,
        size: 45,
        data_cw_l: 156,
        ec_per_block_l: 20,
        num_blocks_l: 2,
        data_cw_m: 124,
        ec_per_block_m: 18,
        num_blocks_m: 4,
        align_positions: &[6, 22, 38],
    },
    VersionSpec {
        version: 8,
        size: 49,
        data_cw_l: 194,
        ec_per_block_l: 24,
        num_blocks_l: 2,
        data_cw_m: 154,
        ec_per_block_m: 22,
        num_blocks_m: 4,
        align_positions: &[6, 24, 42],
    },
    VersionSpec {
        version: 9,
        size: 53,
        data_cw_l: 232,
        ec_per_block_l: 30,
        num_blocks_l: 2,
        data_cw_m: 182,
        ec_per_block_m: 22,
        num_blocks_m: 5,
        align_positions: &[6, 26, 46],
    },
    VersionSpec {
        version: 10,
        size: 57,
        data_cw_l: 274,
        ec_per_block_l: 18,
        num_blocks_l: 4,
        data_cw_m: 216,
        ec_per_block_m: 26,
        num_blocks_m: 5,
        align_positions: &[6, 28, 50],
    },
];

pub struct QrCode {
    pub size: usize,
    modules: Vec<bool>,
}

impl QrCode {
    pub fn get_module(&self, x: usize, y: usize) -> bool {
        if x < self.size && y < self.size {
            self.modules[y * self.size + x]
        } else {
            false
        }
    }

    pub fn encode_text(text: &str, ec_level: QrEcLevel) -> Result<Self, &'static str> {
        let bytes = text.as_bytes();
        let num_data_bytes = bytes.len();

        // 8-bit byte mode overhead: 4-bit mode indicator + 8-bit char count = 12 bits = 1.5 bytes
        let required_cw = num_data_bytes + 2;

        let spec = VERSION_SPECS
            .iter()
            .find(|s| {
                let capacity = match ec_level {
                    QrEcLevel::L => s.data_cw_l,
                    QrEcLevel::M => s.data_cw_m,
                };
                capacity >= required_cw
            })
            .ok_or("Text too long for supported QR Code versions (max 270 bytes)")?;

        let (total_data_cw, ec_per_block, num_blocks) = match ec_level {
            QrEcLevel::L => (spec.data_cw_l, spec.ec_per_block_l, spec.num_blocks_l),
            QrEcLevel::M => (spec.data_cw_m, spec.ec_per_block_m, spec.num_blocks_m),
        };

        // 1. Bit buffer encoding (Byte Mode)
        let mut bits = Vec::new();
        // Mode indicator for Byte mode is 0100
        push_bits(&mut bits, 0b0100, 4);
        // Character count indicator (8 bits for Version 1..=9)
        push_bits(
            &mut bits,
            num_data_bytes as u32,
            if spec.version <= 9 { 8 } else { 16 },
        );
        // Data bytes
        for &b in bytes {
            push_bits(&mut bits, b as u32, 8);
        }

        // Terminator (up to 4 zero bits)
        let total_data_bits = total_data_cw * 8;
        let terminator_len = (total_data_bits - bits.len()).min(4);
        push_bits(&mut bits, 0, terminator_len);

        // Pad to byte boundary
        while bits.len() % 8 != 0 {
            bits.push(false);
        }

        // Pad codewords to capacity with 0xEC and 0x11
        let mut data_codewords = Vec::with_capacity(total_data_cw);
        for chunk in bits.chunks(8) {
            let mut byte = 0u8;
            for (i, &bit) in chunk.iter().enumerate() {
                if bit {
                    byte |= 1 << (7 - i);
                }
            }
            data_codewords.push(byte);
        }

        let pad_bytes = [0xEC, 0x11];
        let mut pad_idx = 0;
        while data_codewords.len() < total_data_cw {
            data_codewords.push(pad_bytes[pad_idx]);
            pad_idx ^= 1;
        }

        // 2. Error Correction Codewords per Block
        let block_data_len = total_data_cw / num_blocks;
        let mut blocks_data = Vec::with_capacity(num_blocks);
        let mut blocks_ec = Vec::with_capacity(num_blocks);

        let rs_gen = reed_solomon_generator(ec_per_block);

        for b in 0..num_blocks {
            let start = b * block_data_len;
            let end = if b == num_blocks - 1 {
                total_data_cw
            } else {
                start + block_data_len
            };
            let chunk = &data_codewords[start..end];
            blocks_data.push(chunk);
            let ec = reed_solomon_encode(chunk, &rs_gen, ec_per_block);
            blocks_ec.push(ec);
        }

        // 3. Interleaving data and EC codewords
        let mut final_codewords = Vec::with_capacity(total_data_cw + ec_per_block * num_blocks);
        let max_data_len = blocks_data.iter().map(|b| b.len()).max().unwrap_or(0);
        for i in 0..max_data_len {
            for b in 0..num_blocks {
                if i < blocks_data[b].len() {
                    final_codewords.push(blocks_data[b][i]);
                }
            }
        }
        for i in 0..ec_per_block {
            for b in 0..num_blocks {
                final_codewords.push(blocks_ec[b][i]);
            }
        }

        // 4. Matrix construction
        let size = spec.size;
        let mut matrix = vec![None; size * size];
        let mut reserved = vec![false; size * size];

        // Finder patterns
        place_finder_pattern(&mut matrix, &mut reserved, size, 0, 0);
        place_finder_pattern(&mut matrix, &mut reserved, size, size - 7, 0);
        place_finder_pattern(&mut matrix, &mut reserved, size, 0, size - 7);

        // Timing patterns
        for i in 8..size - 8 {
            let bit = i % 2 == 0;
            set_module(&mut matrix, &mut reserved, size, i, 6, bit);
            set_module(&mut matrix, &mut reserved, size, 6, i, bit);
        }

        // Dark module
        set_module(
            &mut matrix,
            &mut reserved,
            size,
            8,
            4 * spec.version + 9,
            true,
        );

        // Alignment patterns
        if spec.align_positions.len() >= 2 {
            for &r in spec.align_positions {
                for &c in spec.align_positions {
                    // Skip if overlapping with finder patterns
                    if (r <= 8 && c <= 8) || (r <= 8 && c >= size - 8) || (r >= size - 8 && c <= 8)
                    {
                        continue;
                    }
                    place_alignment_pattern(&mut matrix, &mut reserved, size, c, r);
                }
            }
        }

        // Reserve format information areas
        for i in 0..=8 {
            if i != 6 {
                reserved[8 * size + i] = true;
                reserved[i * size + 8] = true;
            }
        }
        for i in (size - 8)..size {
            reserved[8 * size + i] = true;
        }
        for i in (size - 7)..size {
            reserved[i * size + 8] = true;
        }

        // 5. Data placement (zigzag)
        let mut all_bits = Vec::with_capacity(final_codewords.len() * 8);
        for &cw in &final_codewords {
            for bit_pos in (0..8).rev() {
                all_bits.push((cw >> bit_pos) & 1 == 1);
            }
        }

        let mut bit_idx = 0;
        let mut right = size as isize - 1;
        let mut upward = true;

        while right > 0 {
            if right == 6 {
                right -= 1; // Skip vertical timing column
            }
            let rows: Vec<usize> = if upward {
                (0..size).rev().collect()
            } else {
                (0..size).collect()
            };

            for row in rows {
                for col_offset in 0..=1 {
                    let col = (right - col_offset) as usize;
                    if !reserved[row * size + col] {
                        let bit = if bit_idx < all_bits.len() {
                            all_bits[bit_idx]
                        } else {
                            false
                        };
                        matrix[row * size + col] = Some(bit);
                        bit_idx += 1;
                    }
                }
            }
            right -= 2;
            upward = !upward;
        }

        // 6. Masking & Format Info (use Mask 0: (row + col) % 2 == 0)
        let mask_num = 0u8;
        for r in 0..size {
            for c in 0..size {
                if !reserved[r * size + c] {
                    if let Some(val) = matrix[r * size + c] {
                        let invert = (r + c) % 2 == 0;
                        matrix[r * size + c] = Some(val ^ invert);
                    }
                }
            }
        }

        // Write Format Information
        let format_bits = calculate_format_bits(ec_level.format_bits(), mask_num);
        place_format_bits(&mut matrix, size, format_bits);

        let final_modules: Vec<bool> = matrix.into_iter().map(|m| m.unwrap_or(false)).collect();

        Ok(QrCode {
            size,
            modules: final_modules,
        })
    }
}

fn push_bits(bits: &mut Vec<bool>, value: u32, count: usize) {
    for i in (0..count).rev() {
        bits.push((value >> i) & 1 == 1);
    }
}

fn set_module(
    matrix: &mut [Option<bool>],
    reserved: &mut [bool],
    size: usize,
    x: usize,
    y: usize,
    val: bool,
) {
    matrix[y * size + x] = Some(val);
    reserved[y * size + x] = true;
}

fn place_finder_pattern(
    matrix: &mut [Option<bool>],
    reserved: &mut [bool],
    size: usize,
    top_x: usize,
    top_y: usize,
) {
    // 1. Draw 7x7 finder pattern
    for r in 0..7 {
        for c in 0..7 {
            let is_black = r == 0
                || r == 6
                || c == 0
                || c == 6
                || ((2..=4).contains(&r) && (2..=4).contains(&c));
            set_module(matrix, reserved, size, top_x + c, top_y + r, is_black);
        }
    }

    // 2. Draw 1-module white separator border around finder pattern
    for r in -1..=7 {
        for c in -1..=7 {
            if r == -1 || r == 7 || c == -1 || c == 7 {
                let px = top_x as isize + c;
                let py = top_y as isize + r;
                if px >= 0 && px < size as isize && py >= 0 && py < size as isize {
                    set_module(matrix, reserved, size, px as usize, py as usize, false);
                }
            }
        }
    }
}

fn place_alignment_pattern(
    matrix: &mut [Option<bool>],
    reserved: &mut [bool],
    size: usize,
    cx: usize,
    cy: usize,
) {
    for r in 0..5 {
        for c in 0..5 {
            let x = cx + c - 2;
            let y = cy + r - 2;
            let is_black = r == 0 || r == 4 || c == 0 || c == 4 || (r == 2 && c == 2);
            set_module(matrix, reserved, size, x, y, is_black);
        }
    }
}

fn calculate_format_bits(ec_bits: u8, mask_num: u8) -> u16 {
    let data = ((ec_bits as u32) << 3) | (mask_num as u32);
    let mut rem = data << 10;
    let gen = 0x537;
    for i in (0..5).rev() {
        if (rem >> (i + 10)) & 1 == 1 {
            rem ^= gen << i;
        }
    }
    let full = ((data << 10) | rem) as u16;
    full ^ 0b101010000010010
}

fn place_format_bits(matrix: &mut [Option<bool>], size: usize, bits: u16) {
    let get_bit = |i: usize| -> bool { (bits >> i) & 1 == 1 };

    // Top-left finder placement
    matrix[8 * size + 0] = Some(get_bit(14));
    matrix[8 * size + 1] = Some(get_bit(13));
    matrix[8 * size + 2] = Some(get_bit(12));
    matrix[8 * size + 3] = Some(get_bit(11));
    matrix[8 * size + 4] = Some(get_bit(10));
    matrix[8 * size + 5] = Some(get_bit(9));
    matrix[8 * size + 7] = Some(get_bit(8));
    matrix[8 * size + 8] = Some(get_bit(7));
    matrix[7 * size + 8] = Some(get_bit(6));
    matrix[5 * size + 8] = Some(get_bit(5));
    matrix[4 * size + 8] = Some(get_bit(4));
    matrix[3 * size + 8] = Some(get_bit(3));
    matrix[2 * size + 8] = Some(get_bit(2));
    matrix[1 * size + 8] = Some(get_bit(1));
    matrix[0 * size + 8] = Some(get_bit(0));

    // Bottom-left and Top-right placement
    for i in 0..7 {
        matrix[(size - 1 - i) * size + 8] = Some(get_bit(i));
    }
    for i in 0..8 {
        matrix[8 * size + (size - 8 + i)] = Some(get_bit(7 + i));
    }
}

// ----------------------------------------------------------------------------
// Galois Field GF(256) & Reed-Solomon Codec
// ----------------------------------------------------------------------------

static GF_EXP: [u8; 512] = {
    let mut table = [0u8; 512];
    let mut val = 1u16;
    let mut i = 0;
    while i < 255 {
        table[i] = val as u8;
        table[i + 255] = val as u8;
        val <<= 1;
        if val >= 256 {
            val ^= 0x11D; // Primitive polynomial x^8 + x^4 + x^3 + x^2 + 1
        }
        i += 1;
    }
    table[510] = table[0];
    table[511] = table[1];
    table
};

static GF_LOG: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut val = 1u16;
    let mut i = 0;
    while i < 255 {
        table[val as usize] = i as u8;
        val <<= 1;
        if val >= 256 {
            val ^= 0x11D;
        }
        i += 1;
    }
    table
};

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        let log_sum = (GF_LOG[a as usize] as usize) + (GF_LOG[b as usize] as usize);
        GF_EXP[log_sum]
    }
}

fn reed_solomon_generator(degree: usize) -> Vec<u8> {
    let mut poly = vec![1u8];
    for i in 0..degree {
        let factor = [1u8, GF_EXP[i]];
        let mut next = vec![0u8; poly.len() + 1];
        for (j, &p) in poly.iter().enumerate() {
            next[j] ^= gf_mul(p, factor[0]);
            next[j + 1] ^= gf_mul(p, factor[1]);
        }
        poly = next;
    }
    poly
}

fn reed_solomon_encode(data: &[u8], gen: &[u8], ec_len: usize) -> Vec<u8> {
    let mut remainder = vec![0u8; ec_len];
    for &byte in data {
        let factor = byte ^ remainder[0];
        for i in 0..ec_len - 1 {
            remainder[i] = remainder[i + 1] ^ gf_mul(gen[i + 1], factor);
        }
        remainder[ec_len - 1] = gf_mul(gen[ec_len], factor);
    }
    remainder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qr_generation_basic() {
        let url = "http://192.168.1.15:49281/#t=0123456789abcdef";
        let qr = QrCode::encode_text(url, QrEcLevel::L).expect("QR encoding should succeed");
        assert!(qr.size >= 21);
        let s = qr.size;

        // Top-left finder pattern
        assert!(qr.get_module(0, 0));
        assert!(qr.get_module(0, 6));
        assert!(qr.get_module(6, 0));
        assert!(qr.get_module(6, 6));
        assert!(qr.get_module(3, 3));
        assert!(!qr.get_module(1, 1));
        assert!(!qr.get_module(7, 0)); // separator
        assert!(!qr.get_module(0, 7)); // separator

        // Top-right finder pattern (at size - 7, 0)
        assert!(qr.get_module(s - 7, 0));
        assert!(qr.get_module(s - 1, 0));
        assert!(qr.get_module(s - 7, 6));
        assert!(qr.get_module(s - 1, 6));
        assert!(!qr.get_module(s - 8, 0)); // separator
        assert!(!qr.get_module(s - 7, 7)); // separator

        // Bottom-left finder pattern (at 0, size - 7)
        assert!(qr.get_module(0, s - 7));
        assert!(qr.get_module(6, s - 7));
        assert!(qr.get_module(0, s - 1));
        assert!(qr.get_module(6, s - 1));
        assert!(!qr.get_module(0, s - 8)); // separator
        assert!(!qr.get_module(7, s - 7)); // separator
    }

    #[test]
    fn test_qr_generation_long_url() {
        let long_url = "http://192.168.1.15:49281/#t=a1b2c3d4e5f67890123456789012345678901234567890123456789012345678";
        let qr = QrCode::encode_text(long_url, QrEcLevel::L)
            .expect("QR encoding long url should succeed");
        assert!(qr.size >= 37);
        assert!(qr.get_module(0, 0));
    }
}
