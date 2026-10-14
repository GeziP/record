//! 最小的 WAV 读写（32 位浮点），只供 S1 / S3 的采集与分析使用，不依赖 ffmpeg。

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub struct WavWriter {
    file: BufWriter<File>,
    frames: u64,
    channels: u16,
    rate: u32,
}

impl WavWriter {
    pub fn create(path: &Path, channels: u16, rate: u32) -> io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(channels, rate, 0))?;
        Ok(Self { file, frames: 0, channels, rate })
    }

    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        let mut buf = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        self.file.write_all(&buf)?;
        self.frames += (samples.len() / usize::from(self.channels)) as u64;
        Ok(())
    }

    /// 回填头部里的长度并关闭。
    pub fn finish(mut self) -> io::Result<u64> {
        self.file.flush()?;
        let mut f = self.file.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(self.channels, self.rate, self.frames))?;
        f.sync_all()?;
        Ok(self.frames)
    }
}

fn header(channels: u16, rate: u32, frames: u64) -> [u8; 44] {
    let data = (frames * u64::from(channels) * 4) as u32;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * u32::from(channels) * 4).to_le_bytes());
    h[32..34].copy_from_slice(&(channels * 4).to_le_bytes());
    h[34..36].copy_from_slice(&32u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

pub struct Wav {
    pub channels: u16,
    pub rate: u32,
    pub samples: Vec<f32>,
}

impl Wav {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }

    /// 所有声道取平均得到单声道。
    pub fn mono(&self) -> Vec<f32> {
        let ch = usize::from(self.channels);
        self.samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()
    }
}

pub fn read(path: &Path) -> io::Result<Wav> {
    let mut b = Vec::new();
    File::open(path)?.read_to_end(&mut b)?;
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}：{m}", path.display()));
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad("不是 WAV 文件"));
    }
    let (mut pos, mut fmt) = (12usize, None);
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        match id {
            b"fmt " => {
                let tag = u16::from_le_bytes(b[body..body + 2].try_into().unwrap());
                let ch = u16::from_le_bytes(b[body + 2..body + 4].try_into().unwrap());
                let rate = u32::from_le_bytes(b[body + 4..body + 8].try_into().unwrap());
                let bits = u16::from_le_bytes(b[body + 14..body + 16].try_into().unwrap());
                if tag != 3 || bits != 32 {
                    return Err(bad("只支持 32 位浮点 WAV"));
                }
                fmt = Some((ch, rate));
            }
            b"data" => {
                let (channels, rate) = fmt.ok_or_else(|| bad("data 出现在 fmt 之前"))?;
                // 采集被强杀时头部长度可能没回填，按文件实际长度取
                let end = if size == 0 || body + size > b.len() { b.len() } else { body + size };
                let samples = b[body..end].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                return Ok(Wav { channels, rate, samples });
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }
    Err(bad("没有 data 块"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let mut w = WavWriter::create(&p, 2, 48_000).unwrap();
        let data: Vec<f32> = (0..2_000).map(|i| (i as f32 / 100.0).sin()).collect();
        w.write(&data).unwrap();
        assert_eq!(w.finish().unwrap(), 1_000);
        let r = read(&p).unwrap();
        assert_eq!((r.channels, r.rate, r.frames()), (2, 48_000, 1_000));
        assert_eq!(r.samples, data);
        assert_eq!(r.mono().len(), 1_000);
    }

    #[test]
    fn a_file_that_was_never_finished_is_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let mut w = WavWriter::create(&p, 1, 48_000).unwrap();
        w.write(&[0.5; 480]).unwrap();
        w.file.flush().unwrap();
        // 不调用 finish：头部里的数据长度仍是 0
        let r = read(&p).unwrap();
        assert_eq!(r.frames(), 480);
    }

    #[test]
    fn rejects_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        std::fs::write(&p, b"not a wav file at all, definitely not, no way, padding padding").unwrap();
        assert!(read(&p).is_err());
    }
}
