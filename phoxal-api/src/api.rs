//! Robot API endpoint declarations over domain-owned payloads.

use phoxal_macros::phoxal_api;

phoxal_api! {
    version v0_1 {
        drive {
            command target: Setpoint<crate::domains::v0_1::drive::Target>;
            topic state: State<crate::domains::v0_1::drive::State>;
        }
        joint(joint) {
            topic state: Event<crate::domains::v0_1::joint::JointState>;
        }
        frame {
            topic tree: State<crate::domains::v0_1::frame::Tree>;
            topic static_transforms: State<crate::domains::v0_1::frame::StaticTransforms>;
            query lookup: crate::domains::v0_1::frame::LookupRequest => crate::domains::v0_1::frame::LookupResponse;
        }
        power {
            command command: Setpoint<crate::domains::v0_1::power::Command>;
            topic state: State<crate::domains::v0_1::power::State>;
        }
        motion {
            command manual: Setpoint<crate::domains::v0_1::motion::ManualCommand>;
            topic state: State<crate::domains::v0_1::motion::State>;
        }
        safety {
            topic constraints: State<crate::domains::v0_1::safety::MotionConstraints>;
            topic state: State<crate::domains::v0_1::safety::State>;
        }
        navigation {
            command request: Setpoint<crate::domains::v0_1::navigation::Request>;
            topic state: State<crate::domains::v0_1::navigation::State>;
            topic progress: State<crate::domains::v0_1::navigation::Progress>;
            topic result: State<crate::domains::v0_1::navigation::Result>;
            topic candidate: State<crate::domains::v0_1::navigation::Candidate>;
            query next_frontier: crate::domains::v0_1::navigation::FrontierRequest => crate::domains::v0_1::navigation::FrontierResponse;
        }
        perception {
            topic detections: State<crate::domains::v0_1::perception::Detections>;
            topic state: State<crate::domains::v0_1::perception::State>;
        }
        video {
            query open: crate::domains::v0_1::video::OpenRequest => crate::domains::v0_1::video::OpenOutcome;
        }
        component(instance) {
            motor(capability) {
                command command: Setpoint<crate::domains::v0_1::component::motor::Command>;
            }
            encoder(capability) {
                topic sample: Sample<crate::domains::v0_1::component::encoder::Sample>;
            }
            accelerometer(capability) {
                topic sample: Sample<crate::domains::v0_1::component::accelerometer::Sample>;
            }
            gyroscope(capability) {
                topic sample: Sample<crate::domains::v0_1::component::gyroscope::Sample>;
            }
            magnetometer(capability) {
                topic sample: Sample<crate::domains::v0_1::component::magnetometer::Sample>;
            }
            imu(capability) {
                topic sample: Sample<crate::domains::v0_1::component::imu::Sample>;
            }
            range(capability) {
                topic sample: Sample<crate::domains::v0_1::component::range::Sample>;
            }
            gnss(capability) {
                topic sample: Sample<crate::domains::v0_1::component::gnss::Sample>;
            }
            camera(capability) {
                topic frame: Sample<crate::domains::v0_1::component::camera::Frame>;
            }
            depth(capability) {
                topic frame: Sample<crate::domains::v0_1::component::depth::Frame>;
            }
            lidar(capability) {
                topic scan: Sample<crate::domains::v0_1::component::lidar::Scan>;
            }
            mmwave(capability) {
                topic scan: Sample<crate::domains::v0_1::component::mmwave::Scan>;
            }
            microphone(capability) {
                topic frame: Sample<crate::domains::v0_1::component::microphone::Frame>;
            }
            led(capability) {
                command command: Setpoint<crate::domains::v0_1::component::led::Command>;
            }
            speaker(capability) {
                command stream: Setpoint<crate::domains::v0_1::component::speaker::Chunk>;
            }
            battery(capability) {
                topic state: State<crate::domains::v0_1::component::battery::State>;
            }
            emergency_stop(capability) {
                topic state: State<crate::domains::v0_1::component::emergency_stop::State>;
            }
        }
        odometry {
            topic state: State<crate::domains::v0_1::odometry::State>;
        }
        localize {
            topic state: State<crate::domains::v0_1::localize::LocalizationState>;
        }
        map {
            topic revision: State<crate::domains::v0_1::map::Revision>;
            query submap: crate::domains::v0_1::map::SubmapRequest => crate::domains::v0_1::map::SubmapResponse;
        }
    }

    latest version v0_2 extends v0_1 {
        remove power;
        drive {
            replace command target: Setpoint<crate::domains::v0_2::drive::Target>;
            replace topic state: State<crate::domains::v0_2::drive::State>;
        }
        motion {
            replace topic state: State<crate::domains::v0_2::motion::State>;
        }
        perception {
            replace topic detections: State<crate::domains::v0_2::perception::Detections>;
            replace topic state: State<crate::domains::v0_2::perception::State>;
        }
        navigation {
            remove request;
            replace topic state: State<crate::domains::v0_2::navigation::State>;
            replace topic progress: State<crate::domains::v0_2::navigation::Progress>;
            replace topic result: Event<crate::domains::v0_2::navigation::Result>;
            replace topic candidate: State<crate::domains::v0_2::navigation::Candidate>;
            query start: crate::domains::v0_2::navigation::StartRequest => crate::domains::v0_2::navigation::StartResponse;
            query cancel: crate::domains::v0_2::navigation::CancelRequest => crate::domains::v0_2::navigation::CancelResponse;
        }
        safety {
            replace topic constraints: State<crate::domains::v0_2::safety::MotionConstraints>;
            replace topic state: State<crate::domains::v0_2::safety::State>;
        }
        map {
            replace query submap: crate::domains::v0_1::map::SubmapRequest => crate::domains::v0_2::map::SubmapResponse;
        }
        component(instance) {
            speaker(capability) {
                replace command stream: Stream<crate::domains::v0_1::component::speaker::Chunk>;
            }
            motor(capability) {
                replace command command: Setpoint<crate::domains::v0_2::component::motor::Command>;
            }
            camera(capability) {
                replace topic frame: Sample<crate::domains::v0_2::component::camera::Frame>;
            }
        }
    }
}
