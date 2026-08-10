const MAX_DETECTIONS: usize = 4_096;

#[derive(
    phoxal_macros::DescribeWire, Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Detection {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub snr: f32,
}
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
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
        if detections.len() > MAX_DETECTIONS {
            return Err(InvalidScan("mmWave scan exceeds the detection bound"));
        }
        if detections.iter().any(|d| {
            !d.position
                .iter()
                .chain(d.velocity.iter())
                .chain(std::iter::once(&d.snr))
                .all(|v| v.is_finite())
        }) {
            return Err(InvalidScan(
                "mmWave positions, velocities, and received power must be finite",
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_rejects_nonfinite_detection() {
        assert!(
            Scan::try_new(vec![Detection {
                position: [f32::NAN, 0.0, 0.0],
                velocity: [0.0; 3],
                snr: 0.0
            }])
            .is_err()
        );
    }
    #[test]
    fn constructor_bounds_detection_count() {
        assert!(
            Scan::try_new(vec![
                Detection {
                    position: [0.0; 3],
                    velocity: [0.0; 3],
                    snr: 0.0
                };
                MAX_DETECTIONS + 1
            ])
            .is_err()
        );
    }
}

phoxal_macros::phoxal_api_fragment! {
    path robot / component(instance) / mmwave(capability);

    topic scan: Sample<Scan>;
}
