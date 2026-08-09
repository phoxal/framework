//! Checked v0.2 mmWave scans.

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Detection {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub snr: f32,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "ScanWire")]
pub struct Scan {
    pub detections: Vec<Detection>,
}
#[derive(serde::Deserialize)]
struct ScanWire {
    detections: Vec<Detection>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidScan(&'static str);
impl std::fmt::Display for InvalidScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidScan {}
impl Scan {
    pub fn try_new(detections: Vec<Detection>) -> Result<Self, InvalidScan> {
        if detections.iter().any(|d| {
            !d.position
                .iter()
                .chain(d.velocity.iter())
                .chain(std::iter::once(&d.snr))
                .all(|v| v.is_finite())
                || d.snr < 0.0
        }) {
            return Err(InvalidScan(
                "mmWave positions, velocities and SNR must be finite; SNR must be nonnegative",
            ));
        }
        Ok(Self { detections })
    }
}
impl TryFrom<ScanWire> for Scan {
    type Error = InvalidScan;
    fn try_from(v: ScanWire) -> Result<Self, Self::Error> {
        Self::try_new(v.detections)
    }
}
