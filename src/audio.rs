//! 实时音频：IMA ADPCM 解码与 PCM 数据块。
//!
//! 设备端链路（`core/device/audio/Adpcm.kt`，与二代卡片固件 `adpcm.c` 逐条一致）：
//! AudioRecord 16kHz 16bit 单声道 → 标准 IMA ADPCM（4bit，4:1）→ FF12 二进制帧
//! `type=0x01`，20ms 块（160B）。解码状态跨整段流连续，仅在流开始时复位；
//! 检测到 seq 断流（BLE 通知丢失）时上层应复位解码器并标记 [`PcmChunk::discontinuous`]。

/// 设备端录音的固定采样率（Hz）。
pub const SAMPLE_RATE_HZ: u32 = 16_000;
/// 固定单声道。
pub const CHANNELS: u8 = 1;

/// 标准 AMR-WB 文件魔数（写入文件开头即为 ffmpeg 可解的 .amr）。
pub const AMR_WB_MAGIC: &[u8] = b"#!AMR-WB\n";

/// AMR-WB 各模式（ToC 高 4 位）的载荷字节数（3GPP TS 26.201，mode 0-8）。
pub const WB_PAYLOAD_BYTES: [usize; 9] = [17, 23, 32, 36, 40, 46, 50, 58, 60];

/// 单帧解码输出样本数（20ms @ 16kHz）。
pub const AMR_WB_FRAME_SAMPLES: usize = 320;

mod ffi {
    extern "C" {
        pub fn D_IF_init() -> *mut std::ffi::c_void;
        pub fn D_IF_exit(state: *mut std::ffi::c_void);
        pub fn D_IF_decode(state: *mut std::ffi::c_void, bits: *const u8, pcm: *mut i16, bfi: i32);
    }
}

/// 连续 AMR-WB 流解码器（vendored opencore-amrwb，标准文件格式帧：ToC + 载荷）。
///
/// 状态（激励历史/ISP 记忆）跨帧保持；检测到流断裂时重建实例（`= reset()`）。
/// 输入帧按 ToC 的 mode 校验长度，坏帧返回 None 不推进状态。
pub struct AmrWbDecoder {
    state: *mut std::ffi::c_void,
}

// FFI 状态指针无跨线程共享；所有方法 &mut self，所有权串行移动。
unsafe impl Send for AmrWbDecoder {}

impl AmrWbDecoder {
    pub fn new() -> Self {
        Self {
            state: unsafe { ffi::D_IF_init() },
        }
    }

    pub fn reset(&mut self) {
        unsafe {
            ffi::D_IF_exit(self.state);
            self.state = ffi::D_IF_init();
        }
    }

    /// 解码一帧（ToC + 载荷），输出恰好 [`AMR_WB_FRAME_SAMPLES`] 个样本。
    /// ToC 非法或长度不符返回 None。
    pub fn decode(&mut self, frame: &[u8]) -> Option<Vec<i16>> {
        if frame.is_empty() {
            return None;
        }
        let mode = (frame[0] >> 3) as usize & 0x0F;
        let quality = frame[0] & 0x04 != 0;
        if mode > 8 || !quality || frame.len() != 1 + WB_PAYLOAD_BYTES[mode] {
            return None;
        }
        let mut out = vec![0i16; AMR_WB_FRAME_SAMPLES];
        unsafe {
            ffi::D_IF_decode(self.state, frame.as_ptr(), out.as_mut_ptr(), 0);
        }
        Some(out)
    }
}

impl Default for AmrWbDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AmrWbDecoder {
    fn drop(&mut self) {
        unsafe { ffi::D_IF_exit(self.state) }
    }
}

/// 一段解码后的 PCM 数据块。
#[derive(Debug, Clone, PartialEq)]
pub struct PcmChunk {
    /// 16bit 有符号单声道样本，`sample_rate` 为 16000。
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u8,
    /// true 表示此前检测到数据丢失（seq 断流），解码流出现过断裂，
    /// 断裂点附近会有短暂杂音直至解码状态重新收敛。
    pub discontinuous: bool,
}

impl PcmChunk {
    /// 时长（秒）。
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate as f64
    }
}

/// 标准 IMA ADPCM 索引调整表（与设备端 `Adpcm.kt` / 卡片固件 `adpcm.c` 一致）。
const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// 标准 IMA ADPCM 步长表。
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// 连续 IMA ADPCM 流的解码器。
///
/// 状态（预测样本 + 步长索引）在 [`Self::decode_chunk`] 调用之间保持；
/// 一次实时流会话对应一个实例，检测到断流时调用 [`Self::reset`]。
#[derive(Debug, Clone)]
pub struct AdpcmDecoder {
    predictor: i16,
    index: i32,
}

impl Default for AdpcmDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AdpcmDecoder {
    /// 新解码器（predictor=0, index=0），与设备端 `Adpcm.Encoder` 冷启动一致。
    pub fn new() -> Self {
        Self {
            predictor: 0,
            index: 0,
        }
    }

    pub fn reset(&mut self) {
        self.predictor = 0;
        self.index = 0;
    }

    /// 解码一段 ADPCM 字节，样本追加到 `out`。低位 nibble 是先编码的样本。
    pub fn decode_chunk(&mut self, adpcm: &[u8], out: &mut Vec<i16>) {
        out.reserve(adpcm.len() * 2);
        for byte in adpcm {
            out.push(self.decode_nibble(byte & 0x0F));
            out.push(self.decode_nibble(byte >> 4));
        }
    }

    /// 解码一段 ADPCM 字节并返回样本。
    pub fn decode(&mut self, adpcm: &[u8]) -> Vec<i16> {
        let mut out = Vec::new();
        self.decode_chunk(adpcm, &mut out);
        out
    }

    fn decode_nibble(&mut self, code: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = step >> 3;
        if code & 0b0100 != 0 {
            diff += step;
        }
        if code & 0b0010 != 0 {
            diff += step >> 1;
        }
        if code & 0b0001 != 0 {
            diff += step >> 2;
        }
        let mut predictor = i32::from(self.predictor);
        if code & 0b1000 != 0 {
            predictor -= diff;
        } else {
            predictor += diff;
        }
        self.predictor = predictor.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        self.index = (self.index + INDEX_TABLE[code as usize]).clamp(0, 88);
        self.predictor
    }
}

/// 将 PCM 样本封装为 16bit 单声道 WAV 文件字节（44 字节头 + 小端样本）。
pub fn wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_len);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes()); // fmt 块长度
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // 字节率
    wav.extend_from_slice(&2_u16.to_le_bytes()); // 块对齐
    wav.extend_from_slice(&16_u16.to_le_bytes()); // 位深
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机采样的 AMR-WB 帧（2026-10-24 dyj_v1 录音，mode 2 / 12.65kbps，ToC=0x14）。
    /// 用于锁定 FFI 解码链路可用性与帧长度校验。
    const DEVICE_FRAMES: [&[u8]; 3] = [
        &[
            0x14, 0x50, 0x46, 0x0c, 0x15, 0x17, 0x57, 0x22, 0x71, 0x71, 0x42, 0x0a, 0x16, 0x97,
            0x72, 0xbf, 0xbc, 0x56, 0xc6, 0x4e, 0x4c, 0x46, 0xc7, 0xdf, 0xd4, 0x4f, 0x56, 0xd6,
            0x55, 0x16, 0x5b, 0xc4, 0x50,
        ],
        &[
            0x14, 0x60, 0x5a, 0x10, 0x77, 0x79, 0xdc, 0x6e, 0xb4, 0x5f, 0xab, 0x54, 0x46, 0x57,
            0x38, 0x06, 0x06, 0x86, 0x07, 0x9c, 0x34, 0x56, 0xc6, 0x89, 0xf4, 0x42, 0x44, 0x07,
            0x60, 0x16, 0x53, 0x9a, 0xd0,
        ],
        &[
            0x14, 0x60, 0x03, 0xe0, 0xdd, 0xdf, 0x74, 0xa5, 0x61, 0x7d, 0x6f, 0x1a, 0x17, 0xf4,
            0xef, 0x8f, 0x00, 0x00, 0x01, 0x53, 0x20, 0x10, 0x20, 0x52, 0x70, 0x20, 0x00, 0x59,
            0x10, 0x10, 0x71, 0x79, 0x18,
        ],
    ];

    #[test]
    fn amrwb_decodes_device_frames_to_320_samples() {
        let mut dec = AmrWbDecoder::new();
        for frame in DEVICE_FRAMES {
            let pcm = dec.decode(frame).expect("device frame must decode");
            assert_eq!(pcm.len(), AMR_WB_FRAME_SAMPLES);
        }
    }

    #[test]
    fn amrwb_rejects_bad_toc_and_bad_length() {
        let mut dec = AmrWbDecoder::new();
        // Q 位未置（0x10 = mode2, Q=0）→ 拒绝
        let mut bad_quality = DEVICE_FRAMES[0].to_vec();
        bad_quality[0] = 0x10;
        assert!(dec.decode(&bad_quality).is_none());
        // 长度不符（mode 2 应为 33B）→ 拒绝
        assert!(dec.decode(&DEVICE_FRAMES[0][..20]).is_none());
        // 空帧 → 拒绝
        assert!(dec.decode(&[]).is_none());
        // 坏帧不推进状态：随后好帧照常解码
        assert!(dec.decode(DEVICE_FRAMES[1]).is_some());
    }

    #[test]
    fn amrwb_decode_produces_sane_energy() {
        // 设备帧是真实环境音；解码能量应显著非零且无异常溢出
        let mut dec = AmrWbDecoder::new();
        let mut all = Vec::new();
        for _ in 0..4 {
            for frame in DEVICE_FRAMES {
                all.extend(dec.decode(frame).expect("decode"));
            }
        }
        assert_eq!(all.len(), 12 * AMR_WB_FRAME_SAMPLES);
        let rms: f64 = (all
            .iter()
            .map(|s| i64::from(*s) * i64::from(*s))
            .sum::<i64>() as f64
            / all.len() as f64)
            .sqrt();
        assert!(
            rms > 1.0,
            "decoded energy too low (rms={rms}): decoder broken?"
        );
        assert!(rms < 30_000.0, "decoded energy absurd (rms={rms})");
    }

    /// 测试内 IMA 编码器，算法与设备端 `Adpcm.Encoder` 逐条一致。
    struct AdpcmEncoder {
        prev: i16,
        index: i32,
    }

    impl AdpcmEncoder {
        fn new() -> Self {
            Self { prev: 0, index: 0 }
        }

        fn encode(&mut self, sample: i16) -> u8 {
            let step = STEP_TABLE[self.index as usize];
            let mut diff = i32::from(sample) - i32::from(self.prev);
            let mut code = 0_u8;
            if diff < 0 {
                code = 8;
                diff = -diff;
            }
            if diff >= step {
                code |= 4;
                diff -= step;
            }
            let half = step >> 1;
            if diff >= half {
                code |= 2;
                diff -= half;
            }
            if diff >= half >> 1 {
                code |= 1;
            }
            let mut recon = step >> 3;
            if code & 0b0100 != 0 {
                recon += step;
            }
            if code & 0b0010 != 0 {
                recon += half;
            }
            if code & 0b0001 != 0 {
                recon += half >> 1;
            }
            let mut predictor = i32::from(self.prev);
            if code & 0b1000 != 0 {
                predictor -= recon;
            } else {
                predictor += recon;
            }
            self.prev = predictor.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            self.index = (self.index + INDEX_TABLE[code as usize]).clamp(0, 88);
            code
        }

        fn encode_buffer(&mut self, samples: &[i16]) -> Vec<u8> {
            samples
                .chunks(2)
                .map(|pair| match pair {
                    [low] => self.encode(*low),
                    [low, high] => self.encode(*low) | (self.encode(*high) << 4),
                    _ => unreachable!("chunks(2) never yields empty pairs"),
                })
                .collect()
        }
    }

    fn sine(freq: f64, count: usize) -> Vec<i16> {
        (0..count)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / 16_000.0).sin() * 8_000.0)
            .map(|v| v as i16)
            .collect()
    }

    #[test]
    fn golden_vector_matches_device_encoder() {
        // 与 app/core/device 的 AdpcmTest 黄金向量同一组（独立实现交叉锁定）
        let mut enc = AdpcmEncoder::new();
        let encoded = enc.encode_buffer(&[1000, -1000, 2000, -2000, 3000, -3000, 0, 0, 1, -1]);
        assert_eq!(encoded, [0xf7, 0xf7, 0xf7, 0x82, 0x80]);
    }

    #[test]
    fn round_trips_a_sine_within_adpcm_tolerance() {
        let pcm = sine(440.0, 4_000);
        let encoded = AdpcmEncoder::new().encode_buffer(&pcm);
        let decoded = AdpcmDecoder::new().decode(&encoded);
        assert_eq!(decoded.len(), pcm.len());
        const WARMUP: usize = 32;
        let max_error = decoded[WARMUP..]
            .iter()
            .zip(&pcm[WARMUP..])
            .map(|(out, reference)| (i32::from(*out) - i32::from(*reference)).abs())
            .max()
            .unwrap();
        assert!(max_error < 1_000, "max error {max_error}");
    }

    #[test]
    fn decodes_low_nibble_before_high_nibble() {
        let mut enc = AdpcmEncoder::new();
        let pcm = [1_000_i16, -1_000, 2_000, -2_000];
        let encoded = enc.encode_buffer(&pcm);
        let decoded = AdpcmDecoder::new().decode(&encoded);
        assert!(decoded[0] > 0);
        assert!(decoded[1] < 0);
        assert!(decoded[2] > 0);
        assert!(decoded[3] < 0);
    }

    #[test]
    fn decodes_silence_as_silence() {
        let encoded = AdpcmEncoder::new().encode_buffer(&vec![0_i16; 1_000]);
        let decoded = AdpcmDecoder::new().decode(&encoded);
        assert!(decoded.iter().all(|sample| sample.abs() <= 8));
    }

    #[test]
    fn state_persists_across_20ms_blocks() {
        let pcm = sine(220.0, 2_000);
        let encoded = AdpcmEncoder::new().encode_buffer(&pcm);
        // 按 20ms 块（160B ADPCM）切开分别喂给同一个解码器，
        // 结果必须与一次性解码完全一致——对应设备端跨块不复位。
        let mut streamed = AdpcmDecoder::new();
        let mut streamed_out = Vec::new();
        for chunk in encoded.chunks(160) {
            streamed.decode_chunk(chunk, &mut streamed_out);
        }
        let whole = AdpcmDecoder::new().decode(&encoded);
        assert_eq!(streamed_out, whole);
    }

    #[test]
    fn reset_restores_cold_start() {
        let pcm = sine(440.0, 1_000);
        let encoded = AdpcmEncoder::new().encode_buffer(&pcm);
        let mut decoder = AdpcmDecoder::new();
        let _ = decoder.decode(&encoded[..200]);
        decoder.reset();
        let fresh = AdpcmDecoder::new().decode(&encoded);
        let mut rerun = Vec::new();
        decoder.decode_chunk(&encoded, &mut rerun);
        assert_eq!(rerun, fresh);
    }

    #[test]
    fn wav_header_describes_the_payload() {
        let samples = [1_i16, -1, 2];
        let wav = wav_bytes(&samples, SAMPLE_RATE_HZ);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
        assert_eq!(&wav[44..46], &1_i16.to_le_bytes());
    }
}
