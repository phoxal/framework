# OAK-D Lite component

The component mount uses +X forward, +Y left, and +Z up.
Its native cameras look forward along mount +X, with image right along -Y and image up along +Z.
The authored left and right camera centers are separated by 75 mm; the left camera is on +Y.
These simplified model parameters do not constitute device calibration or a stereo reconstruction algorithm.

MuJoCo cameras look along camera -Z, so their MJCF `xyaxes` explicitly maps the camera frame into this mount convention.
The native component acceptance renders the actual model toward a target at known distance and checks forward depth, image up, and the left/right baseline.
The driver package builds an executable from its local `api/` closure through the ordinary `build.rs` helper.
Its native model remains a component asset, and the selected driver process retains hardware behavior without a communication-only library.
