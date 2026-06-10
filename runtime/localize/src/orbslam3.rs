//! ORB-SLAM3 backend. The FFI layer compiles in every build; when
//! `phoxal-runtime-localize-orb-slam3-sys` is built without the native library, its symbols are stubs
//! that report errors instead of entering ORB-SLAM3.
mod active {
    use std::collections::VecDeque;
    use std::ffi::CString;
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result, anyhow, bail};
    use phoxal::api::component::v1::capability::{camera, depth, imu};
    use phoxal::api::frame::v1::FrameId;
    use phoxal::api::localize::v1::{
        AffectedKeyframeSummary, Covariance, ImuBiasEstimate, Keyframe, KeyframeId,
        LocalizationMode, LocalizationRevisionCause, LocalizationSource, LocalizationStatus,
        LocalizationStatusReason, PoseEstimate, VelocityEstimate,
    };
    use phoxal::bus::typed::Received;
    use phoxal::runtime::clock::Step;

    use crate::pose_math::{compose_poses, invert_pose};
    use crate::registration::DepthRegistration;
    use crate::runtime::{
        BackendUpdate, LocalizeBackend, NewRevision, current_revision,
        initial_sensor_integration_revision,
    };

    const ODOM_FRAME_ID: &str = "odom";
    const BASE_FRAME_ID: &str = "base_footprint";
    /// Minimum translation (m) between successive ORB keyframes the backend publishes, so the map's
    /// submap anchor follows the trajectory and occupancy stays within the local grid.
    const KEYFRAME_MIN_TRANSLATION_M: f64 = 0.5;

    /// Maximum absolute timestamp difference allowed when pairing a color frame
    /// with a depth frame. Color and depth from one RGB-D sensor share a logical
    /// capture time; this absorbs small per-stream stamping jitter while staying
    /// well under a frame period so adjacent frames never cross-pair.
    const PAIRING_TOLERANCE_NS: u64 = 5_000_000;

    /// Bounds on the synchronizer buffers so a stalled stream cannot grow memory
    /// without limit. When exceeded the oldest entries are dropped.
    const MAX_BUFFERED_FRAMES: usize = 256;
    const MAX_BUFFERED_IMU: usize = 8_192;

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct TimedSample<T> {
        timestamp_ns: u64,
        data: T,
    }

    impl<T> TimedSample<T> {
        fn new(timestamp_ns: u64, data: T) -> Self {
            Self { timestamp_ns, data }
        }
    }

    #[derive(Debug, Clone)]
    pub(crate) struct OrbSlam3Config {
        pub(crate) vocabulary_path: PathBuf,
        pub(crate) settings_path: PathBuf,
        pub(crate) inertial: bool,
        pub(crate) color_intrinsics: crate::settings::CameraIntrinsics,
        pub(crate) depth_intrinsics: crate::settings::CameraIntrinsics,
        pub(crate) camera_optical_to_base: ([f64; 3], [f64; 4]),
    }

    pub(crate) struct OrbSlam3Backend {
        handle: *mut phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Handle,
        registration: DepthRegistration,
        sync: VisualInertialSync,
        last_pose: Option<PoseEstimate>,
        last_pose_at_ns: Option<u64>,
        initial_revision_emitted: bool,
        last_keyframe_translation_m: Option<[f64; 3]>,
        keyframe_seq: u64,
        source: LocalizationSource,
        imu_count_since_log: u64,
        camera_count_since_log: u64,
        depth_count_since_log: u64,
        tracking_packet_count_since_log: u64,
        last_io_log_ns: Option<u64>,
        camera_optical_to_base: ([f64; 3], [f64; 4]),
    }

    // SAFETY: ORB-SLAM3 System supports the RGB-D tracking calls used here when
    // calls are serialized. The runtime owns the backend behind `&mut self`, so
    // no two FFI calls can run concurrently through this handle.
    unsafe impl Send for OrbSlam3Backend {}

    impl OrbSlam3Backend {
        pub(crate) fn new(config: OrbSlam3Config) -> Result<Self> {
            let registration =
                DepthRegistration::new(&config.color_intrinsics, &config.depth_intrinsics);
            let vocabulary_path = path_to_cstring(&config.vocabulary_path).with_context(|| {
                format!(
                    "invalid ORB-SLAM3 vocabulary path {}",
                    config.vocabulary_path.display()
                )
            })?;
            let settings_path = path_to_cstring(&config.settings_path).with_context(|| {
                format!(
                    "invalid ORB-SLAM3 settings path {}",
                    config.settings_path.display()
                )
            })?;
            let (handle, source) = if config.inertial {
                let handle = unsafe {
                    // SAFETY: Both C strings are NUL-terminated and live for the duration
                    // of this call. The wrapper returns either a valid owned handle or NULL.
                    phoxal_runtime_localize_orb_slam3_sys::os3_new_rgbd_inertial(
                        vocabulary_path.as_ptr(),
                        settings_path.as_ptr(),
                    )
                };
                (handle, LocalizationSource::OrbSlam3RgbdInertial)
            } else {
                let handle = unsafe {
                    // SAFETY: Both C strings are NUL-terminated and live for the duration
                    // of this call. The wrapper returns either a valid owned handle or NULL.
                    phoxal_runtime_localize_orb_slam3_sys::os3_new_rgbd(
                        vocabulary_path.as_ptr(),
                        settings_path.as_ptr(),
                    )
                };
                (handle, LocalizationSource::OrbSlam3Rgbd)
            };
            if handle.is_null() {
                bail!("failed to initialize ORB-SLAM3 RGB-D backend");
            }

            Ok(Self {
                handle,
                registration,
                sync: VisualInertialSync::new(PAIRING_TOLERANCE_NS, config.inertial),
                last_pose: None,
                last_pose_at_ns: None,
                initial_revision_emitted: false,
                last_keyframe_translation_m: None,
                keyframe_seq: 0,
                source,
                imu_count_since_log: 0,
                camera_count_since_log: 0,
                depth_count_since_log: 0,
                tracking_packet_count_since_log: 0,
                last_io_log_ns: None,
                camera_optical_to_base: config.camera_optical_to_base,
            })
        }

        pub(crate) fn ingest_imu(&mut self, sample: TimedSample<imu::Sample>) -> Result<()> {
            let timestamp_ns = sample.timestamp_ns;
            self.sync.push_imu(sample);
            self.imu_count_since_log += 1;
            self.log_io_rates(timestamp_ns);
            Ok(())
        }

        pub(crate) fn ingest_camera(&mut self, sample: TimedSample<camera::Frame>) -> Result<()> {
            let frame = ColorFrame::from_sample(sample)?;
            let timestamp_ns = frame.timestamp_ns;
            self.sync.push_color(frame);
            self.camera_count_since_log += 1;
            self.log_io_rates(timestamp_ns);
            Ok(())
        }

        pub(crate) fn ingest_depth(&mut self, sample: TimedSample<depth::Depth>) {
            let timestamp_ns = sample.timestamp_ns;
            self.depth_count_since_log += 1;
            self.log_io_rates(timestamp_ns);
            self.sync.push_depth(DepthFrame {
                timestamp_ns,
                samples_mm: sample.data.samples_mm().to_vec(),
            });
        }

        fn drain_tracking(&mut self) -> Result<Option<(PoseEstimate, u64)>> {
            let mut latest = None;
            while let Some(packet) = self.sync.next_packet() {
                latest = Some(self.track_packet(packet)?);
                self.tracking_packet_count_since_log += 1;
            }
            Ok(latest)
        }

        fn track_packet(&mut self, packet: TrackingPacket) -> Result<(PoseEstimate, u64)> {
            let TrackingPacket {
                color,
                depth,
                visual_ts_ns,
                last_visual_ts_ns,
                imu,
            } = packet;

            let (configured_width, configured_height) = self.registration.color_dimensions();
            if color.width != configured_width || color.height != configured_height {
                bail!(
                    "color frame {}x{} does not match configured {}x{}",
                    color.width,
                    color.height,
                    configured_width,
                    configured_height
                );
            }
            let registered_depth = self.registration.register(&depth.samples_mm)?;
            debug_assert_eq!(
                registered_depth.len(),
                self.registration.color_pixel_count()
            );

            let cols = i32::try_from(color.width).context("ORB-SLAM3 color width exceeds i32")?;
            let rows = i32::try_from(color.height).context("ORB-SLAM3 color height exceeds i32")?;
            let color_step_bytes = i32::try_from(
                usize::try_from(color.width)
                    .context("ORB-SLAM3 color width does not fit usize")?
                    .checked_mul(3)
                    .context("ORB-SLAM3 color row stride overflow")?,
            )
            .context("ORB-SLAM3 color row stride exceeds i32")?;
            let depth_step_bytes = i32::try_from(
                usize::try_from(color.width)
                    .context("ORB-SLAM3 depth width does not fit usize")?
                    .checked_mul(std::mem::size_of::<u16>())
                    .context("ORB-SLAM3 depth row stride overflow")?,
            )
            .context("ORB-SLAM3 depth row stride exceeds i32")?;

            let imu_ffi: Vec<phoxal_runtime_localize_orb_slam3_sys::OrbSlam3ImuSample> =
                imu.iter().map(imu_sample_to_ffi).collect();
            let imu_count =
                i32::try_from(imu_ffi.len()).context("ORB-SLAM3 IMU sample count exceeds i32")?;
            let first_imu_ts_ns = imu.first().map(|sample| sample.timestamp_ns);
            let last_imu_ts_ns = imu.last().map(|sample| sample.timestamp_ns);
            tracing::debug!(
                color_ts_ns = color.timestamp_ns,
                depth_ts_ns = depth.timestamp_ns,
                visual_ts_ns,
                last_visual_ts_ns = ?last_visual_ts_ns,
                imu_count = imu_ffi.len(),
                first_imu_ts_ns = ?first_imu_ts_ns,
                last_imu_ts_ns = ?last_imu_ts_ns,
                "ORB-SLAM3 visual-inertial tracking packet"
            );

            let mut translation_m = [0.0_f64; 3];
            let mut rotation_xyzw = [0.0_f64; 4];
            let status = unsafe {
                // SAFETY: Buffers remain borrowed for the duration of this call, dimensions
                // and row strides were checked above, the IMU slice is valid for `imu_count`
                // elements, and output arrays have the required size.
                phoxal_runtime_localize_orb_slam3_sys::os3_track_rgbd(
                    self.handle,
                    color.bgr.as_ptr(),
                    cols,
                    rows,
                    color_step_bytes,
                    registered_depth.as_ptr(),
                    depth_step_bytes,
                    timestamp_s(visual_ts_ns),
                    imu_ffi.as_ptr(),
                    imu_count,
                    translation_m.as_mut_ptr(),
                    rotation_xyzw.as_mut_ptr(),
                )
            };
            status_result(status, "track ORB-SLAM3 RGB-D frame")?;
            // ORB-SLAM3 returns Tcw; publish Twb by inverting to Twc and applying the static
            // camera-optical -> base_footprint extrinsic.
            let (camera_translation_m, camera_rotation_xyzw) =
                invert_pose(translation_m, rotation_xyzw);
            let (translation_m, rotation_xyzw) = compose_poses(
                camera_translation_m,
                camera_rotation_xyzw,
                self.camera_optical_to_base.0,
                self.camera_optical_to_base.1,
            );

            Ok((
                PoseEstimate {
                    frame_id: FrameId::new(ODOM_FRAME_ID),
                    child_frame_id: FrameId::new(BASE_FRAME_ID),
                    translation_m,
                    rotation_xyzw,
                },
                visual_ts_ns,
            ))
        }

        fn log_io_rates(&mut self, now_ns: u64) {
            const IO_LOG_PERIOD_NS: u64 = 2_000_000_000;
            match self.last_io_log_ns {
                None => self.last_io_log_ns = Some(now_ns),
                Some(last) => {
                    let elapsed = now_ns.saturating_sub(last);
                    if elapsed >= IO_LOG_PERIOD_NS {
                        let secs = elapsed as f64 / 1e9;
                        tracing::info!(
                            imu = self.imu_count_since_log,
                            camera = self.camera_count_since_log,
                            depth = self.depth_count_since_log,
                            tracking_packets = self.tracking_packet_count_since_log,
                            imu_hz = self.imu_count_since_log as f64 / secs,
                            camera_hz = self.camera_count_since_log as f64 / secs,
                            depth_hz = self.depth_count_since_log as f64 / secs,
                            tracking_hz = self.tracking_packet_count_since_log as f64 / secs,
                            skipped_no_imu = self.sync.skipped_no_imu_since_log,
                            "ORB localize sensor input rates"
                        );
                        self.imu_count_since_log = 0;
                        self.camera_count_since_log = 0;
                        self.depth_count_since_log = 0;
                        self.tracking_packet_count_since_log = 0;
                        self.sync.skipped_no_imu_since_log = 0;
                        self.last_io_log_ns = Some(now_ns);
                    }
                }
            }
        }
    }

    impl Drop for OrbSlam3Backend {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: The handle was returned by an ORB-SLAM3 constructor and is owned here.
                phoxal_runtime_localize_orb_slam3_sys::os3_destroy(self.handle);
            }
        }
    }

    #[async_trait::async_trait]
    impl LocalizeBackend for OrbSlam3Backend {
        fn name(&self) -> LocalizationSource {
            self.source
        }

        fn ingest_odometry(
            &mut self,
            _sample: Received<phoxal::api::odometry::v1::OdometryEstimate>,
        ) {
        }

        fn ingest_imu(&mut self, sample: Received<imu::Sample>) -> Result<()> {
            Self::ingest_imu(self, timed_from_received(sample))
        }

        fn ingest_camera(&mut self, sample: Received<camera::Frame>) -> Result<()> {
            Self::ingest_camera(self, timed_from_received(sample))
        }

        fn ingest_depth(&mut self, sample: Received<depth::Depth>) -> Result<()> {
            Self::ingest_depth(self, timed_from_received(sample));
            Ok(())
        }

        fn step(&mut self, _step: Step) -> Result<BackendUpdate> {
            if let Some((pose, valid_at_ns)) = self.drain_tracking()? {
                self.last_pose = Some(pose);
                self.last_pose_at_ns = Some(valid_at_ns);
            }
            let mut update = update_from_state(
                self.tracking_state(),
                self.last_pose.clone(),
                self.last_pose_at_ns,
            );

            if update.mode == LocalizationMode::Tracking
                && let Some(pose) = update.pose.as_ref()
                && should_emit_keyframe(self.last_keyframe_translation_m, pose.translation_m)
            {
                update.keyframe = Some(Keyframe {
                    keyframe_id: KeyframeId::new(format!("orb-slam3-{}", self.keyframe_seq)),
                    revision: current_revision(),
                    pose: pose.clone(),
                    descriptors: Vec::new(),
                });
                self.last_keyframe_translation_m = Some(pose.translation_m);
                self.keyframe_seq += 1;
            }

            if let Some(revision) =
                initial_sensor_integration_revision(update.mode, self.initial_revision_emitted)
            {
                self.initial_revision_emitted = true;
                update.new_revision = Some(revision);
            } else if self.initial_revision_emitted && self.poll_map_changed() {
                update.new_revision = Some(NewRevision {
                    cause: LocalizationRevisionCause::LoopClosure,
                    affected_keyframes: AffectedKeyframeSummary {
                        keyframe_ids: Vec::new(),
                        region: None,
                    },
                });
            }
            Ok(update)
        }
    }

    fn timed_from_received<T>(sample: Received<T>) -> TimedSample<T> {
        TimedSample::new(sample.at_ns.unwrap_or(0), sample.value)
    }

    struct ColorFrame {
        timestamp_ns: u64,
        width: u32,
        height: u32,
        bgr: Vec<u8>,
    }

    impl ColorFrame {
        fn from_sample(sample: TimedSample<camera::Frame>) -> Result<Self> {
            let pixel_count = usize::try_from(sample.data.width())
                .context("camera width does not fit usize")?
                .checked_mul(
                    usize::try_from(sample.data.height())
                        .context("camera height does not fit usize")?,
                )
                .context("camera pixel count overflow")?;
            let bgr = match sample.data.encoding() {
                camera::Encoding::Rgb8 => rgb_to_bgr(sample.data.data(), pixel_count)?,
                camera::Encoding::Rgba8 => rgba_to_bgr(sample.data.data(), pixel_count)?,
                camera::Encoding::Jpeg | camera::Encoding::Png | camera::Encoding::L8 => bail!(
                    "ORB-SLAM3 requires unpacked RGB8/RGBA8 color frames, found {:?}",
                    sample.data.encoding()
                ),
            };

            Ok(Self {
                timestamp_ns: sample.timestamp_ns,
                width: sample.data.width(),
                height: sample.data.height(),
                bgr,
            })
        }
    }

    struct DepthFrame {
        timestamp_ns: u64,
        samples_mm: Vec<u16>,
    }

    /// One synchronized visual-inertial tracking packet: a paired RGB-D frame
    /// stamped at the color timestamp, plus exactly the IMU samples that fall in
    /// `(last_visual_ts_ns, visual_ts_ns]`.
    struct TrackingPacket {
        color: ColorFrame,
        depth: DepthFrame,
        visual_ts_ns: u64,
        last_visual_ts_ns: Option<u64>,
        imu: Vec<TimedSample<imu::Sample>>,
    }

    /// Buffers typed Zenoh sensor streams and emits deterministic visual-inertial
    /// tracking packets. Color and depth are paired by timestamp, the color stamp
    /// is the single visual timestamp, frames are tracked in monotonic visual
    /// order, and each frame consumes only the IMU samples in its visual
    /// interval. Inputs may arrive out of order; the buffers are kept sorted so
    /// the emitted packet sequence is independent of arrival order.
    struct VisualInertialSync {
        imu: VecDeque<TimedSample<imu::Sample>>,
        color: VecDeque<ColorFrame>,
        depth: VecDeque<DepthFrame>,
        last_emitted_visual_ts_ns: Option<u64>,
        pairing_tolerance_ns: u64,
        requires_imu: bool,
        skipped_no_imu_since_log: u64,
    }

    impl VisualInertialSync {
        fn new(pairing_tolerance_ns: u64, requires_imu: bool) -> Self {
            Self {
                imu: VecDeque::new(),
                color: VecDeque::new(),
                depth: VecDeque::new(),
                last_emitted_visual_ts_ns: None,
                pairing_tolerance_ns,
                requires_imu,
                skipped_no_imu_since_log: 0,
            }
        }

        fn push_imu(&mut self, sample: TimedSample<imu::Sample>) {
            let ts = sample.timestamp_ns;
            insert_by_ts(&mut self.imu, sample, ts, |sample| sample.timestamp_ns);
            while self.imu.len() > MAX_BUFFERED_IMU {
                self.imu.pop_front();
            }
        }

        fn push_color(&mut self, frame: ColorFrame) {
            let ts = frame.timestamp_ns;
            insert_by_ts(&mut self.color, frame, ts, |frame| frame.timestamp_ns);
            while self.color.len() > MAX_BUFFERED_FRAMES {
                self.color.pop_front();
            }
        }

        fn push_depth(&mut self, frame: DepthFrame) {
            let ts = frame.timestamp_ns;
            insert_by_ts(&mut self.depth, frame, ts, |frame| frame.timestamp_ns);
            while self.depth.len() > MAX_BUFFERED_FRAMES {
                self.depth.pop_front();
            }
        }

        /// Emit the next ready tracking packet, or `None` when more input is
        /// needed. Drains buffers as it goes; callers loop until `None`.
        fn next_packet(&mut self) -> Option<TrackingPacket> {
            loop {
                let color_ts = self.color.front()?.timestamp_ns;

                // Depth frames too old to ever pair with this (oldest) color frame
                // are stale: no future color frame is older, so drop them.
                while let Some(depth) = self.depth.front() {
                    if depth.timestamp_ns + self.pairing_tolerance_ns < color_ts {
                        self.depth.pop_front();
                    } else {
                        break;
                    }
                }

                let Some(match_idx) = self.closest_depth_within_tolerance(color_ts) else {
                    // No depth matched. If a depth frame has already advanced past
                    // this color's window, the color can never pair: drop it and
                    // retry. Otherwise the match may still arrive, so wait.
                    let depth_advanced = self.depth.front().is_some_and(|depth| {
                        depth.timestamp_ns > color_ts + self.pairing_tolerance_ns
                    });
                    if depth_advanced {
                        self.color.pop_front();
                        continue;
                    }
                    return None;
                };

                let visual_ts_ns = color_ts;

                // Track strictly forward in visual time. A frame at or before the
                // last tracked visual timestamp is a late/duplicate arrival; drop it.
                if self
                    .last_emitted_visual_ts_ns
                    .is_some_and(|last| visual_ts_ns <= last)
                {
                    self.color.pop_front();
                    for _ in 0..=match_idx {
                        self.depth.pop_front();
                    }
                    continue;
                }

                if self.requires_imu {
                    let latest_buffered_imu_ts = self.imu.back().map(|sample| sample.timestamp_ns);
                    if latest_buffered_imu_ts.is_none_or(|latest| latest < visual_ts_ns) {
                        return None;
                    }
                }

                let color = self.color.pop_front().expect("color front checked above");
                // Drop depth frames preceding the match (older, unmatchable) and the
                // matched frame itself.
                let mut depth = None;
                for _ in 0..=match_idx {
                    depth = self.depth.pop_front();
                }
                let depth = depth.expect("matched depth index is valid");

                let previous_emitted_visual_ts_ns = self.last_emitted_visual_ts_ns;
                let imu = self.extract_imu_window(previous_emitted_visual_ts_ns, visual_ts_ns);

                if self.requires_imu && imu.is_empty() {
                    self.skipped_no_imu_since_log += 1;
                    tracing::warn!(
                        visual_ts_ns,
                        previous_emitted_visual_ts_ns = ?previous_emitted_visual_ts_ns,
                        latest_buffered_imu_ts_ns = ?self.imu.back().map(|sample| sample.timestamp_ns),
                        skipped_no_imu_since_log = self.skipped_no_imu_since_log,
                        "ORB skipping visual frame with no IMU coverage"
                    );
                    continue;
                }

                self.last_emitted_visual_ts_ns = Some(visual_ts_ns);

                let visual_gap_ms =
                    previous_emitted_visual_ts_ns.map(|prev| (visual_ts_ns - prev) as f64 / 1.0e6);
                tracing::debug!(
                    visual_ts_ns,
                    visual_gap_ms = ?visual_gap_ms,
                    imu_count = imu.len(),
                    first_imu_ts_ns = ?imu.first().map(|sample| sample.timestamp_ns),
                    last_imu_ts_ns = ?imu.last().map(|sample| sample.timestamp_ns),
                    latest_buffered_imu_ts_ns = ?self.imu.back().map(|sample| sample.timestamp_ns),
                    "ORB visual-inertial packet emitted"
                );

                return Some(TrackingPacket {
                    color,
                    depth,
                    visual_ts_ns,
                    last_visual_ts_ns: previous_emitted_visual_ts_ns,
                    imu,
                });
            }
        }

        /// Index of the buffered depth frame closest to `color_ts` within the
        /// pairing tolerance, or `None` if no depth frame is within tolerance.
        fn closest_depth_within_tolerance(&self, color_ts: u64) -> Option<usize> {
            let mut best: Option<(usize, u64)> = None;
            for (idx, depth) in self.depth.iter().enumerate() {
                if depth.timestamp_ns > color_ts + self.pairing_tolerance_ns {
                    break;
                }
                let diff = color_ts.abs_diff(depth.timestamp_ns);
                if diff <= self.pairing_tolerance_ns
                    && best.is_none_or(|(_, best_diff)| diff < best_diff)
                {
                    best = Some((idx, diff));
                }
            }
            best.map(|(idx, _)| idx)
        }

        /// Remove and return the IMU samples in `(lower, upper]`. Samples at or
        /// before `lower` were consumed by an earlier frame and are dropped;
        /// samples after `upper` belong to a future frame and stay buffered.
        fn extract_imu_window(
            &mut self,
            lower: Option<u64>,
            upper: u64,
        ) -> Vec<TimedSample<imu::Sample>> {
            if let Some(lower) = lower {
                while let Some(sample) = self.imu.front() {
                    if sample.timestamp_ns <= lower {
                        self.imu.pop_front();
                    } else {
                        break;
                    }
                }
            }
            let mut window = Vec::new();
            while let Some(sample) = self.imu.front() {
                if sample.timestamp_ns <= upper {
                    window.push(self.imu.pop_front().expect("imu front checked above"));
                } else {
                    break;
                }
            }
            window
        }
    }

    /// Insert `item` into a timestamp-sorted deque, keeping ascending order and
    /// placing equal timestamps after existing ones (stable FIFO).
    fn insert_by_ts<T>(deque: &mut VecDeque<T>, item: T, ts: u64, key: impl Fn(&T) -> u64) {
        let index = deque.partition_point(|existing| key(existing) <= ts);
        deque.insert(index, item);
    }

    fn imu_sample_to_ffi(
        sample: &TimedSample<imu::Sample>,
    ) -> phoxal_runtime_localize_orb_slam3_sys::OrbSlam3ImuSample {
        let accel = sample.data.linear_acceleration_mps2();
        let gyro = sample.data.angular_velocity_radps();
        phoxal_runtime_localize_orb_slam3_sys::OrbSlam3ImuSample {
            accel_x: f64::from(accel[0]),
            accel_y: f64::from(accel[1]),
            accel_z: f64::from(accel[2]),
            gyro_x: f64::from(gyro[0]),
            gyro_y: f64::from(gyro[1]),
            gyro_z: f64::from(gyro[2]),
            timestamp_s: timestamp_s(sample.timestamp_ns),
        }
    }

    fn rgb_to_bgr(data: &[u8], pixel_count: usize) -> Result<Vec<u8>> {
        let expected_len = pixel_count
            .checked_mul(3)
            .context("RGB frame byte count overflow")?;
        if data.len() != expected_len {
            bail!(
                "RGB frame has {} bytes, expected {}",
                data.len(),
                expected_len
            );
        }
        let mut bgr = Vec::with_capacity(data.len());
        for pixel in data.chunks_exact(3) {
            bgr.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        Ok(bgr)
    }

    fn rgba_to_bgr(data: &[u8], pixel_count: usize) -> Result<Vec<u8>> {
        let expected_len = pixel_count
            .checked_mul(4)
            .context("RGBA frame byte count overflow")?;
        if data.len() != expected_len {
            bail!(
                "RGBA frame has {} bytes, expected {}",
                data.len(),
                expected_len
            );
        }
        let mut bgr = Vec::with_capacity(
            pixel_count
                .checked_mul(3)
                .context("BGR frame byte count overflow")?,
        );
        for pixel in data.chunks_exact(4) {
            bgr.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        Ok(bgr)
    }

    fn update_from_state(
        state: phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState,
        pose: Option<PoseEstimate>,
        valid_at_ns: Option<u64>,
    ) -> BackendUpdate {
        let (mode, status) = match state {
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState_OS3_TRACKING_OK => (
                LocalizationMode::Tracking,
                LocalizationStatus {
                    healthy: true,
                    reasons: Vec::new(),
                },
            ),
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState_OS3_TRACKING_RECENTLY_LOST => (
                LocalizationMode::Relocalizing,
                LocalizationStatus {
                    healthy: false,
                    reasons: vec![LocalizationStatusReason::BackendInitializing],
                },
            ),
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState_OS3_TRACKING_LOST => (
                LocalizationMode::Lost,
                LocalizationStatus {
                    healthy: false,
                    reasons: vec![LocalizationStatusReason::SensorStale],
                },
            ),
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState_OS3_TRACKING_NOT_READY
            | phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState_OS3_TRACKING_NO_IMAGES
            | phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState_OS3_TRACKING_NOT_INITIALIZED => (
                LocalizationMode::Initializing,
                LocalizationStatus {
                    healthy: false,
                    reasons: vec![LocalizationStatusReason::BackendInitializing],
                },
            ),
            _ => (
                LocalizationMode::Lost,
                LocalizationStatus {
                    healthy: false,
                    reasons: vec![LocalizationStatusReason::BackendError],
                },
            ),
        };

        BackendUpdate {
            mode,
            pose,
            keyframe: None,
            velocity: None::<VelocityEstimate>,
            covariance: None::<Covariance>,
            imu_bias: None::<ImuBiasEstimate>,
            status,
            valid_at_ns,
            new_revision: None,
        }
    }

    fn status_result(
        status: phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Status,
        operation: &str,
    ) -> Result<()> {
        if status == phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Status_OS3_OK {
            return Ok(());
        }
        Err(anyhow!("{operation} failed with {}", status_name(status)))
    }

    fn status_name(status: phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Status) -> &'static str {
        match status {
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Status_OS3_ERR_INIT_FAILED => {
                "init_failed"
            }
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Status_OS3_ERR_TRACK_FAILED => {
                "track_failed"
            }
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Status_OS3_ERR_INVALID_HANDLE => {
                "invalid_handle"
            }
            phoxal_runtime_localize_orb_slam3_sys::OrbSlam3Status_OS3_ERR_INVALID_INPUT => {
                "invalid_input"
            }
            _ => "unknown_status",
        }
    }

    fn path_to_cstring(path: &Path) -> Result<CString> {
        let Some(text) = path.to_str() else {
            bail!("path must be valid UTF-8");
        };
        CString::new(text).context("path contains an interior NUL byte")
    }

    fn timestamp_s(timestamp_ns: u64) -> f64 {
        timestamp_ns as f64 / 1_000_000_000.0
    }

    fn translation_distance_m(a: [f64; 3], b: [f64; 3]) -> f64 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    /// True when the backend should publish a new keyframe at `current` given the last keyframe's
    /// translation (`None` => no keyframe yet => always emit the first).
    fn should_emit_keyframe(
        last_translation_m: Option<[f64; 3]>,
        current_translation_m: [f64; 3],
    ) -> bool {
        match last_translation_m {
            None => true,
            Some(prev) => {
                translation_distance_m(prev, current_translation_m) >= KEYFRAME_MIN_TRANSLATION_M
            }
        }
    }

    impl OrbSlam3Backend {
        fn tracking_state(&self) -> phoxal_runtime_localize_orb_slam3_sys::OrbSlam3TrackingState {
            unsafe {
                // SAFETY: `self.handle` remains valid until Drop.
                phoxal_runtime_localize_orb_slam3_sys::os3_tracking_state(self.handle)
            }
        }

        fn poll_map_changed(&self) -> bool {
            unsafe {
                // SAFETY: `self.handle` is valid until Drop.
                phoxal_runtime_localize_orb_slam3_sys::os3_poll_map_changed(self.handle) != 0
            }
        }
    }

    #[cfg(test)]
    mod synchronizer {
        use super::*;

        const TOL_NS: u64 = 5;

        fn color_at(timestamp_ns: u64) -> ColorFrame {
            ColorFrame {
                timestamp_ns,
                width: 1,
                height: 1,
                bgr: vec![0, 0, 0],
            }
        }

        fn depth_at(timestamp_ns: u64) -> DepthFrame {
            DepthFrame {
                timestamp_ns,
                samples_mm: vec![0],
            }
        }

        fn imu_at(timestamp_ns: u64) -> TimedSample<imu::Sample> {
            TimedSample::new(
                timestamp_ns,
                imu::Sample::from_motion([0.0, 0.0, 0.0], [0.0, 0.0, 9.81]),
            )
        }

        /// Drain every ready packet into a comparable summary so tests can assert
        /// on the emitted sequence without the FFI tracker.
        struct PacketSummary {
            visual_ts_ns: u64,
            last_visual_ts_ns: Option<u64>,
            color_ts_ns: u64,
            depth_ts_ns: u64,
            imu_ts_ns: Vec<u64>,
        }

        fn drain_all(sync: &mut VisualInertialSync) -> Vec<PacketSummary> {
            let mut summaries = Vec::new();
            while let Some(packet) = sync.next_packet() {
                summaries.push(PacketSummary {
                    visual_ts_ns: packet.visual_ts_ns,
                    last_visual_ts_ns: packet.last_visual_ts_ns,
                    color_ts_ns: packet.color.timestamp_ns,
                    depth_ts_ns: packet.depth.timestamp_ns,
                    imu_ts_ns: packet.imu.iter().map(|s| s.timestamp_ns).collect(),
                });
            }
            summaries
        }

        #[test]
        fn future_imu_samples_are_not_consumed_early() {
            let mut sync = VisualInertialSync::new(TOL_NS, false);
            // 50 belongs to the first frame's interval; 150 is in the future.
            sync.push_imu(imu_at(50));
            sync.push_imu(imu_at(150));
            sync.push_color(color_at(100));
            sync.push_depth(depth_at(100));

            let first = sync.next_packet().expect("paired frame at 100");
            assert_eq!(
                first.imu.iter().map(|s| s.timestamp_ns).collect::<Vec<_>>(),
                vec![50],
                "frame at 100 must not pull in the 150 sample"
            );
            assert!(
                sync.next_packet().is_none(),
                "no second frame yet; the future sample stays buffered"
            );

            // The buffered future sample is delivered to its own frame.
            sync.push_color(color_at(200));
            sync.push_depth(depth_at(200));
            let second = sync.next_packet().expect("paired frame at 200");
            assert_eq!(
                second
                    .imu
                    .iter()
                    .map(|s| s.timestamp_ns)
                    .collect::<Vec<_>>(),
                vec![150],
                "the previously-future sample lands in the 200 interval"
            );
        }

        #[test]
        fn stale_imu_samples_are_dropped() {
            let mut sync = VisualInertialSync::new(TOL_NS, false);
            sync.push_imu(imu_at(90));
            sync.push_color(color_at(100));
            sync.push_depth(depth_at(100));
            let first = sync.next_packet().expect("paired frame at 100");
            assert_eq!(first.imu.len(), 1, "frame at 100 consumes the 90 sample");

            // A sample at 95 arrives late, already covered by the 100 frame, plus a
            // fresh sample at 150 for the next interval.
            sync.push_imu(imu_at(95));
            sync.push_imu(imu_at(150));
            sync.push_color(color_at(200));
            sync.push_depth(depth_at(200));
            let second = sync.next_packet().expect("paired frame at 200");
            assert_eq!(
                second
                    .imu
                    .iter()
                    .map(|s| s.timestamp_ns)
                    .collect::<Vec<_>>(),
                vec![150],
                "the stale 95 sample is dropped, not handed to the 200 frame"
            );
        }

        #[test]
        fn paired_frames_use_the_color_timestamp() {
            let mut sync = VisualInertialSync::new(TOL_NS, false);
            sync.push_color(color_at(100));
            sync.push_depth(depth_at(103));
            let packet = sync
                .next_packet()
                .expect("color and depth pair within tolerance");
            assert_eq!(packet.color.timestamp_ns, 100);
            assert_eq!(packet.depth.timestamp_ns, 103);
            assert_eq!(
                packet.visual_ts_ns, 100,
                "the visual timestamp is the color stamp, not max(color, depth)"
            );
        }

        #[test]
        fn imu_window_respects_open_lower_closed_upper_bounds() {
            let mut sync = VisualInertialSync::new(0, false);
            // Boundary samples: 100 belongs to frame 100, 200 belongs to frame 200.
            for ts in [40, 100, 140, 200, 260] {
                sync.push_imu(imu_at(ts));
            }
            for ts in [100, 200, 300] {
                sync.push_color(color_at(ts));
                sync.push_depth(depth_at(ts));
            }

            let packets = drain_all(&mut sync);
            assert_eq!(packets.len(), 3);
            for packet in &packets {
                let lower = packet.last_visual_ts_ns;
                for &imu_ts in &packet.imu_ts_ns {
                    if let Some(lower) = lower {
                        assert!(
                            imu_ts > lower,
                            "imu {imu_ts} must be strictly after last visual {lower}"
                        );
                    }
                    assert!(
                        imu_ts <= packet.visual_ts_ns,
                        "imu {imu_ts} must be at or before visual {}",
                        packet.visual_ts_ns
                    );
                }
            }
            // The closed upper / open lower boundary places 100 and 200 deterministically.
            assert_eq!(packets[0].imu_ts_ns, vec![40, 100]);
            assert_eq!(packets[1].imu_ts_ns, vec![140, 200]);
            assert_eq!(packets[2].imu_ts_ns, vec![260]);
        }

        #[test]
        fn out_of_order_arrival_is_deterministic() {
            let mut ordered = VisualInertialSync::new(0, false);
            ordered.push_color(color_at(100));
            ordered.push_depth(depth_at(100));
            ordered.push_imu(imu_at(60));
            ordered.push_color(color_at(200));
            ordered.push_depth(depth_at(200));
            ordered.push_imu(imu_at(160));
            ordered.push_color(color_at(300));
            ordered.push_depth(depth_at(300));
            ordered.push_imu(imu_at(260));
            ordered.push_imu(imu_at(360));

            let mut scrambled = VisualInertialSync::new(0, false);
            scrambled.push_imu(imu_at(360));
            scrambled.push_color(color_at(300));
            scrambled.push_imu(imu_at(60));
            scrambled.push_depth(depth_at(100));
            scrambled.push_color(color_at(100));
            scrambled.push_depth(depth_at(300));
            scrambled.push_imu(imu_at(260));
            scrambled.push_color(color_at(200));
            scrambled.push_depth(depth_at(200));
            scrambled.push_imu(imu_at(160));

            let ordered_packets = drain_all(&mut ordered);
            let scrambled_packets = drain_all(&mut scrambled);

            assert_eq!(ordered_packets.len(), scrambled_packets.len());
            for (a, b) in ordered_packets.iter().zip(&scrambled_packets) {
                assert_eq!(a.visual_ts_ns, b.visual_ts_ns);
                assert_eq!(a.last_visual_ts_ns, b.last_visual_ts_ns);
                assert_eq!(a.color_ts_ns, b.color_ts_ns);
                assert_eq!(a.depth_ts_ns, b.depth_ts_ns);
                assert_eq!(a.imu_ts_ns, b.imu_ts_ns);
            }
            assert_eq!(
                ordered_packets
                    .iter()
                    .map(|p| (p.visual_ts_ns, p.imu_ts_ns.clone()))
                    .collect::<Vec<_>>(),
                vec![(100, vec![60]), (200, vec![160]), (300, vec![260])],
                "future sample 360 stays buffered for a later frame"
            );
        }

        #[test]
        fn inertial_sync_waits_until_imu_reaches_visual_timestamp() {
            let mut sync = VisualInertialSync::new(TOL_NS, true);
            sync.push_color(color_at(100));
            sync.push_depth(depth_at(100));
            sync.push_imu(imu_at(60));
            // IMU watermark (60) has not reached the visual timestamp (100): hold the frame.
            assert!(
                sync.next_packet().is_none(),
                "must wait until buffered IMU reaches the visual timestamp"
            );

            sync.push_imu(imu_at(120));
            let packet = sync
                .next_packet()
                .expect("IMU watermark now past 100; packet emits");
            assert_eq!(packet.visual_ts_ns, 100);
            assert_eq!(
                packet
                    .imu
                    .iter()
                    .map(|s| s.timestamp_ns)
                    .collect::<Vec<_>>(),
                vec![60],
                "frame 100 takes the 60 sample; 120 stays buffered for a later frame"
            );
            assert!(
                sync.next_packet().is_none(),
                "no further visual frames; 120 remains buffered"
            );
        }

        #[test]
        fn inertial_sync_does_not_emit_empty_imu_packet() {
            let mut sync = VisualInertialSync::new(TOL_NS, true);
            sync.push_color(color_at(100));
            sync.push_depth(depth_at(100));
            // Only IMU is at 120: the watermark passes 100, but no sample falls at or
            // before 100, so frame 100 has empty coverage and must be skipped, not emitted.
            sync.push_imu(imu_at(120));
            assert!(
                sync.next_packet().is_none(),
                "must not emit a packet with an empty IMU window"
            );
        }

        #[test]
        fn inertial_empty_skip_does_not_advance_last_emitted_visual_timestamp() {
            let mut sync = VisualInertialSync::new(TOL_NS, true);

            // First visual has no IMU sample at/before it. The future IMU watermark
            // lets the synchronizer know 100 is covered, but the window is empty.
            sync.push_color(color_at(100));
            sync.push_depth(depth_at(100));
            sync.push_imu(imu_at(120));

            assert!(
                sync.next_packet().is_none(),
                "empty-IMU visual frame must be skipped"
            );

            // The next emitted packet should still behave as the first emitted ORB frame.
            sync.push_color(color_at(200));
            sync.push_depth(depth_at(200));
            sync.push_imu(imu_at(220));

            let packet = sync
                .next_packet()
                .expect("second visual has IMU coverage from 120");

            assert_eq!(
                packet.last_visual_ts_ns, None,
                "skipped frames must not advance the last emitted ORB visual timestamp"
            );
            assert_eq!(
                packet
                    .imu
                    .iter()
                    .map(|s| s.timestamp_ns)
                    .collect::<Vec<_>>(),
                vec![120],
            );
        }

        #[test]
        fn non_inertial_sync_emits_without_imu() {
            let mut sync = VisualInertialSync::new(TOL_NS, false);
            sync.push_color(color_at(100));
            sync.push_depth(depth_at(100));
            let packet = sync
                .next_packet()
                .expect("RGB-D mode emits paired frames without waiting for IMU");
            assert_eq!(packet.visual_ts_ns, 100);
            assert!(
                packet.imu.is_empty(),
                "non-inertial RGB-D packets carry no IMU"
            );
        }
    }

    #[cfg(test)]
    mod conformance {
        use std::path::PathBuf;

        use anyhow::Context as _;
        use phoxal::api::simulation::v1::clock::Clock;

        use super::*;

        const SETTINGS: &str = r#"%YAML:1.0
File.version: "1.0"
Camera.type: "PinHole"
Camera1.fx: 500.0
Camera1.fy: 500.0
Camera1.cx: 320.0
Camera1.cy: 240.0
Camera1.k1: 0.0
Camera1.k2: 0.0
Camera1.p1: 0.0
Camera1.p2: 0.0
Camera.width: 640
Camera.height: 480
Camera.fps: 30
Camera.RGB: 1
Stereo.ThDepth: 40.0
Stereo.b: 0.05
RGBD.DepthMapFactor: 1000.0
IMU.T_b_c1: !!opencv-matrix
   rows: 4
   cols: 4
   dt: f
   data: [1.0, 0.0, 0.0, 0.0,
          0.0, 1.0, 0.0, 0.0,
          0.0, 0.0, 1.0, 0.0,
          0.0, 0.0, 0.0, 1.0]
IMU.InsertKFsWhenLost: 0
IMU.NoiseGyro: 1e-2
IMU.NoiseAcc: 1e-1
IMU.GyroWalk: 1e-6
IMU.AccWalk: 1e-4
IMU.Frequency: 200.0
ORBextractor.nFeatures: 1250
ORBextractor.scaleFactor: 1.2
ORBextractor.nLevels: 8
ORBextractor.iniThFAST: 20
ORBextractor.minThFAST: 7
Viewer.KeyFrameSize: 0.05
Viewer.KeyFrameLineWidth: 1.0
Viewer.GraphLineWidth: 0.9
Viewer.PointSize: 2.0
Viewer.CameraSize: 0.08
Viewer.CameraLineWidth: 3.0
Viewer.ViewpointX: 0.0
Viewer.ViewpointY: -0.7
Viewer.ViewpointZ: -3.5
Viewer.ViewpointF: 500.0
"#;

        const WIDTH: u32 = 640;
        const HEIGHT: u32 = 480;
        const FRAME_COUNT: u64 = 10;
        const FRAME_DT_NS: u64 = 33_333_333;
        const IMU_DT_NS: u64 = 5_000_000;
        const IMU_SAMPLES_PER_FRAME: u64 = 6;

        #[test]
        fn translation_distance_uses_3d_euclidean_distance() {
            assert_eq!(
                translation_distance_m([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
                0.0
            );
            assert_eq!(
                translation_distance_m([1.0, 2.0, 3.0], [4.0, 6.0, 3.0]),
                5.0
            );
        }

        #[test]
        fn keyframe_cadence_emits_first_keyframe() {
            assert!(should_emit_keyframe(None, [1.0, 2.0, 3.0]));
        }

        #[test]
        fn keyframe_cadence_suppresses_small_motion() {
            assert!(!should_emit_keyframe(
                Some([0.0, 0.0, 0.0]),
                [KEYFRAME_MIN_TRANSLATION_M - 0.001, 0.0, 0.0],
            ));
        }

        #[test]
        fn keyframe_cadence_emits_after_minimum_motion() {
            assert!(should_emit_keyframe(
                Some([0.0, 0.0, 0.0]),
                [KEYFRAME_MIN_TRANSLATION_M, 0.0, 0.0],
            ));
            assert!(should_emit_keyframe(
                Some([1.0, 1.0, 1.0]),
                [1.0, 1.0 + KEYFRAME_MIN_TRANSLATION_M + 0.001, 1.0],
            ));
        }

        #[test]
        fn backend_runs_rgbd_inertial_ffi_lifecycle() -> Result<()> {
            if !phoxal_runtime_localize_orb_slam3_sys::LINKED {
                eprintln!("skipping: ORB-SLAM3 native library not linked into this build");
                return Ok(());
            }

            let orb_dir = std::env::var("ORB_SLAM3_DIR").context("ORB_SLAM3_DIR set in image")?;
            let vocab = PathBuf::from(&orb_dir).join("Vocabulary/ORBvoc.txt");
            if !vocab.exists() {
                eprintln!("skipping: ORB-SLAM3 vocabulary not present");
                return Ok(());
            }

            let settings_path = std::env::temp_dir().join("orbslam3_synthetic_rgbd_inertial.yaml");
            std::fs::write(&settings_path, SETTINGS)
                .context("synthetic ORB-SLAM3 settings written")?;

            let backend = OrbSlam3Backend::new(OrbSlam3Config {
                vocabulary_path: vocab,
                settings_path,
                inertial: true,
                color_intrinsics: crate::settings::CameraIntrinsics::from_horizontal_fov(
                    WIDTH, HEIGHT, 1.2,
                )?,
                depth_intrinsics: crate::settings::CameraIntrinsics::from_horizontal_fov(
                    WIDTH, HEIGHT, 1.2,
                )?,
                camera_optical_to_base: ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]),
            });
            let mut backend = backend.context(
                "ORB-SLAM3 RGB-D inertial backend must construct from real vocab + valid settings",
            )?;

            let mut final_mode = None;
            for frame_index in 0..FRAME_COUNT {
                let frame_time_ns = (frame_index + 1) * FRAME_DT_NS;
                let imu_start_ns = frame_index * FRAME_DT_NS;
                for imu_index in 0..IMU_SAMPLES_PER_FRAME {
                    let imu_time_ns = imu_start_ns + ((imu_index + 1) * IMU_DT_NS);
                    backend.ingest_imu(TimedSample::new(imu_time_ns, synthetic_imu()))?;
                }

                backend.ingest_camera(TimedSample::new(frame_time_ns, synthetic_rgb_frame()))?;
                backend.ingest_depth(TimedSample::new(frame_time_ns, synthetic_depth()));
                let update = backend
                    .step(step_at(frame_time_ns))
                    .context("step must not fail across the FFI boundary")?;
                final_mode = Some(update.mode);
            }

            assert_eq!(
                final_mode,
                Some(LocalizationMode::Initializing),
                "synthetic RGB-D + IMU should keep ORB-SLAM3 initializing; Tracking would be unexpected without coherent visual parallax"
            );
            Ok(())
        }

        fn synthetic_imu() -> imu::Sample {
            imu::Sample::from_motion([0.01, -0.01, 0.005], [0.0, 0.0, 9.81])
        }

        fn synthetic_rgb_frame() -> camera::Frame {
            let mut data = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let value = x as u8 ^ y as u8;
                    data.extend_from_slice(&[value, value, value]);
                }
            }
            camera::Frame::new(WIDTH, HEIGHT, camera::Encoding::Rgb8, data)
        }

        fn synthetic_depth() -> depth::Depth {
            depth::Depth::new(vec![1500_u16; (WIDTH * HEIGHT) as usize])
        }

        fn step_at(time_ns: u64) -> Step {
            Step::new(Clock::new(1, time_ns / FRAME_DT_NS, time_ns, FRAME_DT_NS))
        }
    }
}

pub(crate) use active::{OrbSlam3Backend, OrbSlam3Config};
